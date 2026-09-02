use crate::config::{create_private_dir, ConfigPaths};
use crate::connection::{ConnectionId, SessionBinding};
use crate::memory::{compile_global_memory_prompt, GlobalMemory};
mod index;
mod rows;
mod schema;
mod types;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
pub use types::*;

const DATABASE_FILE: &str = "averroes.db";
const LAST_BINDING_KEY: &str = "last_session_binding";
const EMBEDDING_CONFIG_KEY: &str = "conversation_embedding_config";
const GLOBAL_MEMORY_PROMPT_KEY: &str = "global_memory_system_prompt";
const VECTOR_EXTENSION_ENTRYPOINT: &str = "sqlite3_sqlitevectorrs_init";

pub struct WorkDatabase {
    path: PathBuf,
    connection: Mutex<Connection>,
    vector_extension_available: bool,
}

impl WorkDatabase {
    pub fn open(paths: &ConfigPaths) -> Result<Arc<Self>, WorkDatabaseError> {
        let database = Self::open_at(paths.root.join(DATABASE_FILE))?;
        database.ensure_default_workspace(&paths.default_workspace_root())?;
        Ok(database)
    }

    pub fn open_at(path: PathBuf) -> Result<Arc<Self>, WorkDatabaseError> {
        let parent = path
            .parent()
            .ok_or_else(|| WorkDatabaseError::InvalidPath(path.clone()))?;
        create_private_dir(parent).map_err(|error| WorkDatabaseError::Io(error.to_string()))?;
        let connection = Connection::open(&path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&connection)?;
        let vector_extension_available = match load_vector_extension(&connection) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "sqlite-vector-rs extension unavailable; semantic search will use SQLite fallback"
                );
                false
            }
        };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| WorkDatabaseError::Io(error.to_string()))?;
        }

        Ok(Arc::new(Self {
            path,
            connection: Mutex::new(connection),
            vector_extension_available,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn vector_index_available(&self) -> bool {
        self.vector_extension_available
    }

    pub fn projects(&self) -> Result<Vec<WorkProject>, WorkDatabaseError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id, name, root, created_at, last_opened_at
             FROM projects ORDER BY last_opened_at DESC, name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], schema::project_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn open_project(&self, root: &Path) -> Result<WorkProject, WorkDatabaseError> {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Workspace")
            .to_string();
        let timestamp = now();
        let id = uuid::Uuid::new_v4().to_string();
        let root_text = root.to_string_lossy().to_string();
        let connection = self.connection.lock();
        connection.execute(
            "INSERT INTO projects (id, name, root, created_at, last_opened_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(root) DO UPDATE SET last_opened_at = excluded.last_opened_at",
            params![id, name, root_text, timestamp],
        )?;
        connection
            .query_row(
                "SELECT id, name, root, created_at, last_opened_at FROM projects WHERE root = ?1",
                params![root_text],
                schema::project_from_row,
            )
            .map_err(Into::into)
    }

    /// Create the built-in workspace and attach legacy conversations that did
    /// not have a workspace yet. `open_at` intentionally remains a low-level
    /// database constructor for isolated tests and tooling.
    pub fn ensure_default_workspace(&self, root: &Path) -> Result<WorkProject, WorkDatabaseError> {
        create_private_dir(root).map_err(|error| WorkDatabaseError::Io(error.to_string()))?;
        let project = self.open_project(root)?;
        self.connection.lock().execute(
            "UPDATE conversations SET project_id = ?1 WHERE project_id IS NULL",
            params![project.id],
        )?;
        Ok(project)
    }

    pub fn touch_project(&self, project_id: &str) -> Result<(), WorkDatabaseError> {
        self.connection.lock().execute(
            "UPDATE projects SET last_opened_at = ?2 WHERE id = ?1",
            params![project_id, now()],
        )?;
        Ok(())
    }

    pub fn conversation_folders(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<WorkConversationFolder>, WorkDatabaseError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id, workspace_id, name, created_at, updated_at
             FROM conversation_folders
             WHERE workspace_id = ?1
             ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows =
            statement.query_map(params![workspace_id], schema::conversation_folder_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn conversation_folder_ids(
        &self,
        workspace_id: &str,
    ) -> Result<HashMap<String, String>, WorkDatabaseError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT members.conversation_id, members.folder_id
             FROM conversation_folder_members members
             JOIN conversation_folders folders ON folders.id = members.folder_id
             WHERE folders.workspace_id = ?1",
        )?;
        let rows = statement.query_map(params![workspace_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect())
    }

    pub fn create_conversation_folder(
        &self,
        workspace_id: &str,
        name: &str,
    ) -> Result<WorkConversationFolder, WorkDatabaseError> {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() {
            return Err(WorkDatabaseError::InvalidFolder(
                "folder name cannot be empty".into(),
            ));
        }
        let timestamp = now();
        let folder = WorkConversationFolder {
            id: uuid::Uuid::new_v4().to_string(),
            workspace_id: workspace_id.to_owned(),
            name,
            created_at: timestamp,
            updated_at: timestamp,
        };
        let connection = self.connection.lock();
        connection.execute(
            "INSERT INTO conversation_folders
                (id, workspace_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                folder.id,
                folder.workspace_id,
                folder.name,
                folder.created_at,
                folder.updated_at
            ],
        )?;
        Ok(folder)
    }

    pub fn set_conversation_folder(
        &self,
        conversation_id: &str,
        folder_id: Option<&str>,
    ) -> Result<(), WorkDatabaseError> {
        let connection = self.connection.lock();
        match folder_id {
            Some(folder_id) => {
                let valid = connection.query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM conversations conversations
                         JOIN conversation_folders folders
                           ON folders.workspace_id = conversations.project_id
                         WHERE conversations.id = ?1 AND folders.id = ?2
                     )",
                    params![conversation_id, folder_id],
                    |row| row.get::<_, i64>(0),
                )? != 0;
                if !valid {
                    return Err(WorkDatabaseError::InvalidFolder(
                        "conversation and folder must belong to the same workspace".into(),
                    ));
                }
                connection.execute(
                    "INSERT INTO conversation_folder_members (conversation_id, folder_id)
                     VALUES (?1, ?2)
                     ON CONFLICT(conversation_id) DO UPDATE SET folder_id = excluded.folder_id",
                    params![conversation_id, folder_id],
                )?;
            }
            None => {
                connection.execute(
                    "DELETE FROM conversation_folder_members WHERE conversation_id = ?1",
                    params![conversation_id],
                )?;
            }
        }
        Ok(())
    }

    pub fn delete_conversation_folder(&self, folder_id: &str) -> Result<bool, WorkDatabaseError> {
        let deleted = self.connection.lock().execute(
            "DELETE FROM conversation_folders WHERE id = ?1",
            params![folder_id],
        )?;
        Ok(deleted > 0)
    }

    pub fn conversation_summaries(
        &self,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>, WorkDatabaseError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id, title, project_id, pinned, unread, updated_at
             FROM conversations
             ORDER BY updated_at DESC,
                      COALESCE((SELECT MAX(m.id) FROM messages m
                                WHERE m.conversation_id = conversations.id), 0) DESC,
                      id
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok(ConversationSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                project_id: row.get(2)?,
                pinned: row.get::<_, i64>(3)? != 0,
                unread: row.get::<_, i64>(4)? != 0,
                updated_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn conversation(&self, id: &str) -> Result<Option<WorkConversation>, WorkDatabaseError> {
        let connection = self.connection.lock();
        let row = connection
            .query_row(
                "SELECT id, title, project_id, pinned, unread, created_at, updated_at, binding_json,
                        context_summary, context_usage_json, agent_threads_json,
                        agent_thread_transcripts_json
                 FROM conversations WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)? != 0,
                        row.get::<_, i64>(4)? != 0,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, String>(11)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            title,
            project_id,
            pinned,
            unread,
            created_at,
            updated_at,
            binding_json,
            context_summary,
            context_usage_json,
            agent_threads_json,
            agent_thread_transcripts_json,
        )) = row
        else {
            return Ok(None);
        };
        let binding = serde_json::from_str(&binding_json)?;
        let context_usage = serde_json::from_str(&context_usage_json)?;
        let agent_threads = serde_json::from_str(&agent_threads_json)?;
        let agent_thread_transcripts = serde_json::from_str(&agent_thread_transcripts_json)?;
        let messages = rows::load_messages(&connection, &id)?;
        let checkpoints = rows::load_checkpoints(&connection, &id)?;
        let tasks = rows::load_tasks(&connection, &id)?;
        let sources = rows::load_sources(&connection, &id)?;
        Ok(Some(WorkConversation {
            id,
            title,
            project_id,
            pinned,
            unread,
            created_at,
            updated_at,
            binding,
            context_summary,
            context_usage,
            messages,
            checkpoints,
            tasks,
            sources,
            agent_threads,
            agent_thread_transcripts,
        }))
    }

    pub fn save_conversation(
        &self,
        conversation: &WorkConversation,
    ) -> Result<(), WorkDatabaseError> {
        let binding = serde_json::to_string(&conversation.binding)?;
        let agent_threads = serde_json::to_string(&conversation.agent_threads)?;
        let agent_thread_transcripts =
            serde_json::to_string(&conversation.agent_thread_transcripts)?;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let existing_updated_at = transaction
            .query_row(
                "SELECT updated_at FROM conversations WHERE id = ?1",
                params![conversation.id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let updated_at =
            if existing_updated_at.is_some() && rows::content_equal(&transaction, conversation)? {
                existing_updated_at.unwrap_or(conversation.updated_at)
            } else {
                conversation.updated_at
            };
        transaction.execute(
            "INSERT INTO conversations
                (id, title, project_id, pinned, unread, created_at, updated_at, binding_json,
                 context_summary, context_usage_json, agent_threads_json,
                 agent_thread_transcripts_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                project_id = excluded.project_id,
                pinned = excluded.pinned,
                unread = excluded.unread,
                updated_at = excluded.updated_at,
                binding_json = excluded.binding_json,
                context_summary = excluded.context_summary,
                context_usage_json = excluded.context_usage_json,
                agent_threads_json = excluded.agent_threads_json,
                agent_thread_transcripts_json = excluded.agent_thread_transcripts_json",
            params![
                conversation.id,
                conversation.title,
                conversation.project_id,
                conversation.pinned as i64,
                conversation.unread as i64,
                conversation.created_at,
                updated_at,
                binding,
                conversation.context_summary,
                serde_json::to_string(&conversation.context_usage)?,
                agent_threads,
                agent_thread_transcripts,
            ],
        )?;
        rows::replace_messages(&transaction, conversation)?;
        for checkpoint in &conversation.checkpoints {
            rows::upsert_checkpoint_tx(&transaction, &conversation.id, checkpoint)?;
        }
        for task in &conversation.tasks {
            rows::upsert_task_tx(&transaction, &conversation.id, task)?;
        }
        for source in &conversation.sources {
            rows::upsert_source_tx(&transaction, &conversation.id, source)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn conversation_documents(&self) -> Result<Vec<ConversationDocument>, WorkDatabaseError> {
        index::load_documents(&self.connection.lock())
    }

    pub fn pending_conversation_documents(
        &self,
        config: &EmbeddingConfig,
    ) -> Result<Vec<ConversationDocument>, WorkDatabaseError> {
        index::load_pending_documents(&self.connection.lock(), config)
    }

    pub fn pending_embedding_count(
        &self,
        config: &EmbeddingConfig,
    ) -> Result<usize, WorkDatabaseError> {
        index::pending_document_count(&self.connection.lock(), config)
    }

    pub fn replace_conversation_embeddings(
        &self,
        conversation_id: &str,
        connection_id: &ConnectionId,
        model_id: &str,
        fragments: &[crate::memory::ConversationFragment],
        embeddings: &[Vec<f32>],
    ) -> Result<(), WorkDatabaseError> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        index::replace_embeddings(
            &transaction,
            conversation_id,
            &connection_id.0,
            model_id,
            fragments,
            embeddings,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn indexed_fragments(
        &self,
        connection_id: &ConnectionId,
        model_id: &str,
    ) -> Result<Vec<IndexedConversationFragment>, WorkDatabaseError> {
        index::load_fragments(&self.connection.lock(), &connection_id.0, model_id)
    }

    pub fn rebuild_vector_index(
        &self,
        config: &EmbeddingConfig,
    ) -> Result<usize, WorkDatabaseError> {
        if !self.vector_extension_available {
            return Ok(0);
        }
        index::rebuild_vector_table(&mut self.connection.lock(), config)
    }

    pub fn search_conversations_vector(
        &self,
        config: &EmbeddingConfig,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<VectorSearchHit>, WorkDatabaseError> {
        if !self.vector_extension_available || query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        index::vector_search(&self.connection.lock(), config, query, limit)
    }

    pub fn search_conversations_text(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ConversationSearchResult>, WorkDatabaseError> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        index::text_search(&self.connection.lock(), query, limit)
    }

    /// Returns the compact, generated global-memory system-prompt fragment.
    pub fn global_memory_prompt(&self) -> Result<Option<String>, WorkDatabaseError> {
        let stored = self
            .connection
            .lock()
            .query_row(
                "SELECT value FROM preferences WHERE key = ?1",
                params![GLOBAL_MEMORY_PROMPT_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(stored.filter(|prompt| !prompt.trim().is_empty()))
    }

    pub fn global_memories(&self) -> Result<Vec<GlobalMemory>, WorkDatabaseError> {
        load_global_memories(&self.connection.lock())
    }

    /// Stores a user-confirmed entry and regenerates the bounded system prompt
    /// from the full durable set in the same SQLite transaction.
    pub fn create_global_memory(
        &self,
        content: &str,
    ) -> Result<(GlobalMemory, Option<String>), WorkDatabaseError> {
        let content = content.split_whitespace().collect::<Vec<_>>().join(" ");
        if content.is_empty() {
            return Err(WorkDatabaseError::InvalidGlobalMemory(
                "memory content cannot be empty".into(),
            ));
        }
        let timestamp = now();
        let memory = GlobalMemory {
            id: uuid::Uuid::new_v4().to_string(),
            content,
            created_at: timestamp,
            updated_at: timestamp,
        };
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO global_memories (id, content, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                memory.id,
                memory.content,
                memory.created_at,
                memory.updated_at
            ],
        )?;
        let prompt = rebuild_global_memory_prompt(&transaction)?;
        transaction.commit()?;
        Ok((memory, prompt))
    }

    /// Deletes an entry by its full UUID or the eight-character ID displayed
    /// in the compiled prompt, then regenerates that prompt atomically.
    pub fn delete_global_memory(
        &self,
        id: &str,
    ) -> Result<(bool, Option<String>), WorkDatabaseError> {
        let id = id.trim();
        if id.is_empty() {
            return Err(WorkDatabaseError::InvalidGlobalMemory(
                "memory id cannot be empty".into(),
            ));
        }
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let deleted = transaction.execute(
            "DELETE FROM global_memories WHERE id = ?1 OR substr(id, 1, 8) = ?1",
            params![id],
        )?;
        let prompt = rebuild_global_memory_prompt(&transaction)?;
        transaction.commit()?;
        Ok((deleted > 0, prompt))
    }

    /// Loads a small, direct slice after deep-memory search identifies the
    /// relevant conversation. It intentionally excludes hidden reasoning.
    pub fn deep_memory_excerpt(
        &self,
        conversation_id: &str,
        start: usize,
        limit: usize,
    ) -> Result<Option<DeepMemoryExcerpt>, WorkDatabaseError> {
        if limit == 0 {
            return Ok(None);
        }
        let connection = self.connection.lock();
        let Some((title, context_summary)) = connection
            .query_row(
                "SELECT title, context_summary FROM conversations WHERE id = ?1",
                params![conversation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT position, role, text FROM messages
             WHERE conversation_id = ?1
             ORDER BY position
             LIMIT ?2 OFFSET ?3",
        )?;
        let messages = statement
            .query_map(
                params![conversation_id, limit as i64, start as i64],
                |row| {
                    Ok(DeepMemoryMessage {
                        position: row.get::<_, i64>(0)? as usize,
                        role: WorkMessageRole::parse(&row.get::<_, String>(1)?),
                        text: row.get(2)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(DeepMemoryExcerpt {
            conversation_id: conversation_id.to_owned(),
            title,
            context_summary,
            messages,
        }))
    }

    pub fn embedding_config(&self) -> Result<Option<EmbeddingConfig>, WorkDatabaseError> {
        index::embedding_config(&self.connection.lock(), EMBEDDING_CONFIG_KEY)
    }

    pub fn save_embedding_config(&self, config: &EmbeddingConfig) -> Result<(), WorkDatabaseError> {
        index::save_embedding_config(&self.connection.lock(), EMBEDDING_CONFIG_KEY, config)
    }

    pub fn embedding_index_status(&self) -> Result<EmbeddingIndexStatus, WorkDatabaseError> {
        let connection = self.connection.lock();
        index::status(
            &connection,
            index::embedding_config(&connection, EMBEDDING_CONFIG_KEY)?,
        )
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<bool, WorkDatabaseError> {
        let changed = self.connection.lock().execute(
            "UPDATE conversations SET pinned = ?2 WHERE id = ?1",
            params![id, pinned as i64],
        )?;
        Ok(changed > 0)
    }

    pub fn set_conversation_unread(
        &self,
        id: &str,
        unread: bool,
    ) -> Result<bool, WorkDatabaseError> {
        let changed = self.connection.lock().execute(
            "UPDATE conversations SET unread = ?2 WHERE id = ?1",
            params![id, unread as i64],
        )?;
        Ok(changed > 0)
    }

    pub fn rename_conversation(&self, id: &str, title: &str) -> Result<bool, WorkDatabaseError> {
        let changed = self.connection.lock().execute(
            "UPDATE conversations SET title = ?2 WHERE id = ?1",
            params![id, title],
        )?;
        Ok(changed > 0)
    }

    pub fn delete_conversation(&self, id: &str) -> Result<bool, WorkDatabaseError> {
        let deleted = self
            .connection
            .lock()
            .execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
        Ok(deleted > 0)
    }

    pub fn upsert_checkpoint(
        &self,
        session_id: &str,
        checkpoint: &WorkCheckpoint,
    ) -> Result<(), WorkDatabaseError> {
        let connection = self.connection.lock();
        rows::upsert_checkpoint_connection(&connection, session_id, checkpoint)
    }

    pub fn tasks(&self, session_id: &str) -> Result<Vec<WorkTask>, WorkDatabaseError> {
        let connection = self.connection.lock();
        rows::load_tasks(&connection, session_id)
    }

    pub fn upsert_task(&self, session_id: &str, task: &WorkTask) -> Result<(), WorkDatabaseError> {
        let connection = self.connection.lock();
        rows::upsert_task_connection(&connection, session_id, task)
    }

    pub fn mark_task_as_done(
        &self,
        session_id: &str,
        task_id: &str,
        updated_at: i64,
    ) -> Result<Option<WorkTask>, WorkDatabaseError> {
        let connection = self.connection.lock();
        let changed = connection.execute(
            "UPDATE tasks SET status = 'done', updated_at = ?3
             WHERE conversation_id = ?1 AND task_id = ?2",
            params![session_id, task_id, updated_at],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let task = connection.query_row(
            "SELECT task_id, title, status, created_at, updated_at FROM tasks
             WHERE conversation_id = ?1 AND task_id = ?2",
            params![session_id, task_id],
            |row| {
                Ok(WorkTask {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    status: TaskStatus::parse(&row.get::<_, String>(2)?),
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            },
        )?;
        Ok(Some(task))
    }

    pub fn record_source(
        &self,
        session_id: &str,
        source: &WorkSource,
    ) -> Result<(), WorkDatabaseError> {
        let connection = self.connection.lock();
        rows::upsert_source_connection(&connection, session_id, source)
    }

    pub fn last_binding(&self) -> Result<Option<SessionBinding>, WorkDatabaseError> {
        let serialized = self
            .connection
            .lock()
            .query_row(
                "SELECT value FROM preferences WHERE key = ?1",
                params![LAST_BINDING_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        serialized
            .map(|serialized| serde_json::from_str(&serialized).map_err(Into::into))
            .transpose()
    }

    pub fn remember_binding(&self, binding: &SessionBinding) -> Result<(), WorkDatabaseError> {
        if !binding.is_ready() {
            return Ok(());
        }
        let serialized = serde_json::to_string(binding)?;
        self.connection.lock().execute(
            "INSERT INTO preferences (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = excluded.updated_at",
            params![LAST_BINDING_KEY, serialized, now()],
        )?;
        Ok(())
    }

    pub fn onboarding_steps(&self) -> Result<Vec<WorkOnboardingStep>, WorkDatabaseError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT step_id, completed, completed_at, updated_at
             FROM onboarding_steps
             ORDER BY updated_at ASC, step_id ASC",
        )?;
        let steps = statement.query_map([], |row| {
            Ok(WorkOnboardingStep {
                id: row.get(0)?,
                completed: row.get::<_, i64>(1)? != 0,
                completed_at: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        steps
            .collect::<Result<Vec<_>, _>>()
            .map_err(WorkDatabaseError::from)
    }

    pub fn set_onboarding_step(
        &self,
        step_id: &str,
        completed: bool,
    ) -> Result<(), WorkDatabaseError> {
        let step_id = step_id.trim();
        if step_id.is_empty() || step_id.len() > 80 {
            return Err(WorkDatabaseError::InvalidOnboardingStep(
                "step id must contain between 1 and 80 bytes".into(),
            ));
        }
        let timestamp = now();
        self.connection.lock().execute(
            "INSERT INTO onboarding_steps (step_id, completed, completed_at, updated_at)
             VALUES (?1, ?2, CASE WHEN ?2 = 1 THEN ?3 ELSE NULL END, ?3)
             ON CONFLICT(step_id) DO UPDATE SET
                completed = excluded.completed,
                completed_at = CASE
                    WHEN excluded.completed = 1
                        THEN COALESCE(onboarding_steps.completed_at, excluded.completed_at)
                    ELSE NULL
                END,
                updated_at = CASE
                    WHEN onboarding_steps.completed <> excluded.completed
                        THEN excluded.updated_at
                    ELSE onboarding_steps.updated_at
                END",
            params![step_id, i64::from(completed), timestamp],
        )?;
        Ok(())
    }

    pub fn forget_binding_for_connection(
        &self,
        connection_id: &ConnectionId,
    ) -> Result<(), WorkDatabaseError> {
        let Some(binding) = self.last_binding()? else {
            return Ok(());
        };
        if binding.connection_id.as_ref() == Some(connection_id) {
            self.connection.lock().execute(
                "DELETE FROM preferences WHERE key = ?1",
                params![LAST_BINDING_KEY],
            )?;
        }
        Ok(())
    }
}

fn load_global_memories(connection: &Connection) -> Result<Vec<GlobalMemory>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT id, content, created_at, updated_at FROM global_memories
         ORDER BY updated_at ASC, id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(GlobalMemory {
            id: row.get(0)?,
            content: row.get(1)?,
            created_at: row.get(2)?,
            updated_at: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn rebuild_global_memory_prompt(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<Option<String>, WorkDatabaseError> {
    let memories = load_global_memories(transaction)?;
    let prompt = compile_global_memory_prompt(&memories).map(|prompt| prompt.content);
    match &prompt {
        Some(prompt) => {
            transaction.execute(
                "INSERT INTO preferences (key, value, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET
                    value = excluded.value,
                    updated_at = excluded.updated_at",
                params![GLOBAL_MEMORY_PROMPT_KEY, prompt, now()],
            )?;
        }
        None => {
            transaction.execute(
                "DELETE FROM preferences WHERE key = ?1",
                params![GLOBAL_MEMORY_PROMPT_KEY],
            )?;
        }
    }
    Ok(prompt)
}

/// Load the trusted sqlite-vector-rs module from Cargo's build output without
/// consulting environment variables. Cargo builds cdylib dependencies in
/// `target/{profile}/deps`, while packaged builds may place the module next to
/// the application executable.
fn load_vector_extension(connection: &Connection) -> Result<(), WorkDatabaseError> {
    let path = vector_extension_path().ok_or_else(|| {
        WorkDatabaseError::Io(
            "sqlite-vector-rs library was not found beside the app or in target/{debug,release}/deps"
                .into(),
        )
    })?;
    unsafe {
        connection
            .load_extension_enable()
            .map_err(|error| WorkDatabaseError::Io(error.to_string()))?;
        // Cargo adds a hash to libraries built in `target/*/deps`. If the
        // entrypoint is left empty, SQLite derives it from that filename and
        // looks for a hash-specific symbol that the extension does not export.
        let result = connection.load_extension(&path, Some(VECTOR_EXTENSION_ENTRYPOINT));
        let disable_result = connection.load_extension_disable();
        result
            .map_err(|error| WorkDatabaseError::Io(error.to_string()))
            .and_then(|_| disable_result.map_err(|error| WorkDatabaseError::Io(error.to_string())))
    }
}

fn vector_extension_path() -> Option<PathBuf> {
    let mut directories = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            directories.push(parent.to_path_buf());
            directories.push(parent.join("deps"));
        }
    }
    if let Ok(current) = std::env::current_dir() {
        directories.push(current.join("target/debug"));
        directories.push(current.join("target/debug/deps"));
        directories.push(current.join("target/release"));
        directories.push(current.join("target/release/deps"));
    }

    let extension = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().map(|name| name.to_string_lossy()) else {
                continue;
            };
            let Some(file_extension) = path
                .extension()
                .map(|extension| extension.to_string_lossy())
            else {
                continue;
            };
            let is_extension = (name.starts_with("libsqlite_vector_rs")
                || (cfg!(target_os = "windows") && name.starts_with("sqlite_vector_rs")))
                && file_extension == extension;
            if is_extension {
                return Some(path);
            }
        }
    }
    None
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Debug, thiserror::Error)]
pub enum WorkDatabaseError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("could not serialize work state: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("storage error: {0}")]
    Io(String),
    #[error("conversation index error: {0}")]
    Index(String),
    #[error("invalid database path: {0}")]
    InvalidPath(PathBuf),
    #[error("invalid global memory: {0}")]
    InvalidGlobalMemory(String),
    #[error("invalid conversation folder: {0}")]
    InvalidFolder(String),
    #[error("invalid onboarding step: {0}")]
    InvalidOnboardingStep(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> (tempfile::TempDir, Arc<WorkDatabase>) {
        let directory = tempfile::tempdir().unwrap();
        let database = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        (directory, database)
    }

    #[test]
    fn migration_corrects_the_legacy_gpt_5_6_context_limit() {
        let (_directory, database) = database();
        {
            let connection = database.connection.lock();
            connection
                .execute(
                    "INSERT INTO conversations
                     (id, title, created_at, updated_at, binding_json, context_usage_json)
                     VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
                    params![
                        "legacy-gpt-5-6-context",
                        "Legacy context",
                        now(),
                        r#"{"model_id":"gpt-5.6-luna"}"#,
                        r#"{"input_tokens":172258,"output_tokens":20,"context_limit":128000}"#,
                    ],
                )
                .unwrap();
            schema::migrate(&connection).unwrap();
        }

        let conversation = database
            .conversation("legacy-gpt-5-6-context")
            .unwrap()
            .unwrap();

        assert_eq!(conversation.context_usage.context_limit, 1_050_000);
        assert_eq!(conversation.context_usage.input_tokens, Some(172_258));
    }

    #[test]
    fn saves_projects_and_reopens_the_same_root() {
        let (directory, database) = database();
        let root = directory.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let first = database.open_project(&root).unwrap();
        let second = database.open_project(&root).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(database.projects().unwrap().len(), 1);
    }

    #[test]
    fn conversation_folders_are_scoped_to_their_workspace() {
        let (directory, database) = database();
        let first_root = directory.path().join("first");
        let second_root = directory.path().join("second");
        std::fs::create_dir_all(&first_root).unwrap();
        std::fs::create_dir_all(&second_root).unwrap();
        let first = database.open_project(&first_root).unwrap();
        let second = database.open_project(&second_root).unwrap();
        let folder = database
            .create_conversation_folder(&first.id, "Research")
            .unwrap();

        let conversation = WorkConversation {
            id: "foldered-conversation".into(),
            title: "Foldered".into(),
            project_id: Some(first.id.clone()),
            pinned: false,
            unread: false,
            created_at: now(),
            updated_at: now(),
            binding: SessionBinding::default(),
            context_summary: None,
            context_usage: crate::agent::ContextUsage::default(),
            messages: Vec::new(),
            checkpoints: Vec::new(),
            tasks: Vec::new(),
            sources: Vec::new(),
            agent_threads: Vec::new(),
            agent_thread_transcripts: std::collections::HashMap::new(),
        };
        database.save_conversation(&conversation).unwrap();

        database
            .set_conversation_folder(&conversation.id, Some(&folder.id))
            .unwrap();
        assert_eq!(
            database.conversation_folder_ids(&first.id).unwrap()[&conversation.id],
            folder.id
        );
        assert!(database
            .set_conversation_folder(&conversation.id, Some(&second.id))
            .is_err());
        database
            .set_conversation_folder(&conversation.id, None)
            .unwrap();
        assert!(database
            .conversation_folder_ids(&first.id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn round_trips_a_complete_conversation() {
        let (directory, database) = database();
        let timestamp = now();
        let conversation = WorkConversation {
            id: "conversation-1".into(),
            title: "Build the interface".into(),
            project_id: None,
            pinned: true,
            unread: true,
            created_at: timestamp,
            updated_at: timestamp,
            binding: SessionBinding {
                connection_id: Some(ConnectionId("connection-1".into())),
                model_id: Some("model-1".into()),
                reasoning_effort: Some("high".into()),
                tools: vec!["discover_tools".into(), "web_search_intrernal".into()],
            },
            context_summary: Some(
                "Objective: ship the interface.\nNext action: verify the release.".into(),
            ),
            context_usage: crate::agent::ContextUsage {
                input_tokens: Some(42),
                output_tokens: Some(7),
                cache_read_input_tokens: Some(30),
                cache_creation_input_tokens: Some(2),
                reasoning_output_tokens: Some(5),
                context_limit: 100,
            },
            messages: vec![WorkMessage {
                role: WorkMessageRole::User,
                text: "Make it excellent".into(),
                reasoning: String::new(),
                reasoning_complete: true,
                reasoning_expanded: false,
                tool_activities: Vec::new(),
                expanded_tool_groups: Vec::new(),
            }],
            checkpoints: vec![WorkCheckpoint {
                id: "ui".into(),
                title: "Rebuild shell".into(),
                status: CheckpointStatus::InProgress,
                detail: None,
                message_position: Some(0),
                updated_at: timestamp,
            }],
            tasks: vec![WorkTask {
                id: "task-release".into(),
                title: "Verify the release".into(),
                status: TaskStatus::Pending,
                created_at: timestamp,
                updated_at: timestamp,
            }],
            sources: vec![WorkSource {
                key: "file:app.rs".into(),
                kind: "file".into(),
                label: "app.rs".into(),
                url: None,
                title: None,
                detail: None,
                count: 1,
                last_used_at: timestamp,
            }],
            agent_threads: Vec::new(),
            agent_thread_transcripts: std::collections::HashMap::new(),
        };
        database.save_conversation(&conversation).unwrap();
        drop(database);
        let reopened = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        assert_eq!(
            reopened.conversation(&conversation.id).unwrap(),
            Some(conversation)
        );
    }

    #[test]
    fn deleting_a_conversation_cascades_its_activity() {
        let (_directory, database) = database();
        let timestamp = now();
        let conversation = WorkConversation {
            id: "temporary".into(),
            title: "Temporary".into(),
            project_id: None,
            pinned: false,
            unread: false,
            created_at: timestamp,
            updated_at: timestamp,
            binding: SessionBinding::default(),
            context_summary: None,
            context_usage: crate::agent::ContextUsage::default(),
            messages: Vec::new(),
            checkpoints: Vec::new(),
            tasks: Vec::new(),
            sources: Vec::new(),
            agent_threads: Vec::new(),
            agent_thread_transcripts: std::collections::HashMap::new(),
        };
        database.save_conversation(&conversation).unwrap();
        assert!(database.delete_conversation(&conversation.id).unwrap());
        assert!(!database.delete_conversation(&conversation.id).unwrap());
        assert!(database.conversation(&conversation.id).unwrap().is_none());
    }

    #[test]
    fn renames_a_conversation_without_changing_its_contents() {
        let (_directory, database) = database();
        let timestamp = now();
        let conversation = WorkConversation {
            id: "rename-me".into(),
            title: "Old title".into(),
            project_id: None,
            pinned: false,
            unread: false,
            created_at: timestamp,
            updated_at: timestamp,
            binding: SessionBinding::default(),
            context_summary: None,
            context_usage: crate::agent::ContextUsage::default(),
            messages: vec![WorkMessage {
                role: WorkMessageRole::User,
                text: "Keep me".into(),
                reasoning: String::new(),
                reasoning_complete: true,
                reasoning_expanded: false,
                tool_activities: Vec::new(),
                expanded_tool_groups: Vec::new(),
            }],
            checkpoints: Vec::new(),
            tasks: Vec::new(),
            sources: Vec::new(),
            agent_threads: Vec::new(),
            agent_thread_transcripts: std::collections::HashMap::new(),
        };
        database.save_conversation(&conversation).unwrap();
        assert!(database
            .rename_conversation(&conversation.id, "New title")
            .unwrap());
        let renamed = database.conversation(&conversation.id).unwrap().unwrap();
        assert_eq!(renamed.title, "New title");
        assert_eq!(renamed.messages, conversation.messages);
        assert!(database.set_pinned(&conversation.id, true).unwrap());
        assert!(
            database
                .conversation(&conversation.id)
                .unwrap()
                .unwrap()
                .pinned
        );
        assert!(!database.set_pinned("missing", true).unwrap());
    }

    #[test]
    fn migrates_and_persists_unread_conversation_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("averroes.db");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE projects (
                        id TEXT PRIMARY KEY,
                        name TEXT NOT NULL,
                        root TEXT NOT NULL UNIQUE,
                        created_at INTEGER NOT NULL,
                        last_opened_at INTEGER NOT NULL
                    );
                    CREATE TABLE conversations (
                        id TEXT PRIMARY KEY,
                        title TEXT NOT NULL,
                        project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
                        pinned INTEGER NOT NULL DEFAULT 0,
                        created_at INTEGER NOT NULL,
                        updated_at INTEGER NOT NULL,
                        binding_json TEXT NOT NULL DEFAULT '{}'
                    );
                    INSERT INTO conversations
                        (id, title, pinned, created_at, updated_at, binding_json)
                    VALUES ('legacy', 'Legacy', 0, 1, 1, '{}');",
                )
                .unwrap();
        }

        let database = WorkDatabase::open_at(path).unwrap();
        assert!(!database.conversation("legacy").unwrap().unwrap().unread);
        assert!(database.set_conversation_unread("legacy", true).unwrap());
        assert!(database.conversation("legacy").unwrap().unwrap().unread);
        assert!(
            database
                .conversation_summaries(10)
                .unwrap()
                .into_iter()
                .find(|conversation| conversation.id == "legacy")
                .unwrap()
                .unread
        );
        assert!(!database.set_conversation_unread("missing", true).unwrap());
    }

    #[test]
    fn remembers_the_last_complete_connection_and_model_pair() {
        let (directory, database) = database();
        let binding = SessionBinding {
            connection_id: Some(ConnectionId("chatgpt-work".into())),
            model_id: Some("gpt-5.2-codex".into()),
            reasoning_effort: None,
            tools: vec!["file_read".into(), "grep".into()],
        };
        database.remember_binding(&binding).unwrap();
        drop(database);

        let reopened = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        assert_eq!(reopened.last_binding().unwrap(), Some(binding));
    }

    #[test]
    fn onboarding_steps_persist_and_can_become_pending_again() {
        let (directory, database) = database();
        database
            .set_onboarding_step("welcome_introduction", true)
            .unwrap();
        database
            .set_onboarding_step("active_connection", false)
            .unwrap();
        drop(database);

        let reopened = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        let steps = reopened.onboarding_steps().unwrap();
        let introduction = steps
            .iter()
            .find(|step| step.id == "welcome_introduction")
            .unwrap();
        assert!(introduction.completed);
        assert!(introduction.completed_at.is_some());
        assert!(
            !steps
                .iter()
                .find(|step| step.id == "active_connection")
                .unwrap()
                .completed
        );

        reopened
            .set_onboarding_step("active_connection", true)
            .unwrap();
        reopened
            .set_onboarding_step("active_connection", false)
            .unwrap();
        let connection = reopened
            .onboarding_steps()
            .unwrap()
            .into_iter()
            .find(|step| step.id == "active_connection")
            .unwrap();
        assert!(!connection.completed);
        assert_eq!(connection.completed_at, None);
    }

    #[test]
    fn global_memory_rebuilds_and_persists_a_short_system_prompt() {
        let (directory, database) = database();
        let (memory, prompt) = database
            .create_global_memory("  Prefer   English  UI copy. ")
            .unwrap();
        let prompt = prompt.unwrap();
        assert!(prompt.contains("Confirmed Global Memory"));
        assert!(prompt.contains("Prefer English UI copy."));
        assert!(prompt.contains(&memory.id[..8]));

        drop(database);
        let reopened = WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        assert_eq!(reopened.global_memory_prompt().unwrap(), Some(prompt));
        assert!(reopened.delete_global_memory(&memory.id[..8]).unwrap().0);
        assert_eq!(reopened.global_memory_prompt().unwrap(), None);
    }

    #[test]
    fn deep_memory_excerpt_excludes_reasoning_and_respects_the_requested_slice() {
        let (_directory, database) = database();
        let conversation = WorkConversation {
            id: "deep-memory".into(),
            title: "Architecture notes".into(),
            project_id: None,
            pinned: false,
            unread: false,
            created_at: 1,
            updated_at: 1,
            binding: SessionBinding::default(),
            context_summary: None,
            context_usage: crate::agent::ContextUsage::default(),
            messages: vec![
                WorkMessage {
                    role: WorkMessageRole::User,
                    text: "First decision".into(),
                    reasoning: "hidden chain of thought".into(),
                    reasoning_complete: true,
                    reasoning_expanded: false,
                    tool_activities: Vec::new(),
                    expanded_tool_groups: Vec::new(),
                },
                WorkMessage {
                    role: WorkMessageRole::Assistant,
                    text: "Second decision".into(),
                    reasoning: String::new(),
                    reasoning_complete: true,
                    reasoning_expanded: false,
                    tool_activities: Vec::new(),
                    expanded_tool_groups: Vec::new(),
                },
            ],
            checkpoints: Vec::new(),
            tasks: Vec::new(),
            sources: Vec::new(),
            agent_threads: Vec::new(),
            agent_thread_transcripts: std::collections::HashMap::new(),
        };
        database.save_conversation(&conversation).unwrap();

        let excerpt = database
            .deep_memory_excerpt(&conversation.id, 1, 1)
            .unwrap()
            .unwrap();
        assert_eq!(excerpt.title, "Architecture notes");
        assert_eq!(excerpt.messages.len(), 1);
        assert_eq!(excerpt.messages[0].position, 1);
        assert_eq!(excerpt.messages[0].text, "Second decision");
    }

    #[test]
    fn conversation_order_follows_last_message_even_when_pinned_state_changes() {
        let (_directory, database) = database();
        for (id, updated_at) in [("older", 10), ("newer", 20)] {
            database
                .save_conversation(&WorkConversation {
                    id: id.into(),
                    title: id.into(),
                    project_id: None,
                    pinned: false,
                    unread: false,
                    created_at: updated_at,
                    updated_at,
                    binding: SessionBinding::default(),
                    context_summary: None,
                    context_usage: crate::agent::ContextUsage::default(),
                    messages: vec![WorkMessage {
                        role: WorkMessageRole::User,
                        text: id.into(),
                        reasoning: String::new(),
                        reasoning_complete: true,
                        reasoning_expanded: false,
                        tool_activities: Vec::new(),
                        expanded_tool_groups: Vec::new(),
                    }],
                    checkpoints: Vec::new(),
                    tasks: Vec::new(),
                    sources: Vec::new(),
                    agent_threads: Vec::new(),
                    agent_thread_transcripts: std::collections::HashMap::new(),
                })
                .unwrap();
        }
        database.set_pinned("older", true).unwrap();
        let summaries = database.conversation_summaries(10).unwrap();
        assert_eq!(summaries[0].id, "newer");
        assert_eq!(summaries[1].id, "older");
    }

    #[test]
    fn conversation_order_uses_message_write_order_when_timestamps_tie() {
        let (_directory, database) = database();
        for id in ["older", "newer"] {
            database
                .save_conversation(&WorkConversation {
                    id: id.into(),
                    title: id.into(),
                    project_id: None,
                    pinned: false,
                    unread: false,
                    created_at: 10,
                    updated_at: 10,
                    binding: SessionBinding::default(),
                    context_summary: None,
                    context_usage: crate::agent::ContextUsage::default(),
                    messages: vec![WorkMessage {
                        role: WorkMessageRole::User,
                        text: id.into(),
                        reasoning: String::new(),
                        reasoning_complete: true,
                        reasoning_expanded: false,
                        tool_activities: Vec::new(),
                        expanded_tool_groups: Vec::new(),
                    }],
                    checkpoints: Vec::new(),
                    tasks: Vec::new(),
                    sources: Vec::new(),
                    agent_threads: Vec::new(),
                    agent_thread_transcripts: std::collections::HashMap::new(),
                })
                .unwrap();
        }
        let mut older = database.conversation("older").unwrap().unwrap();
        older.messages[0].text = "new last message".into();
        older.updated_at = 10;
        database.save_conversation(&older).unwrap();

        let summaries = database.conversation_summaries(10).unwrap();
        assert_eq!(summaries[0].id, "older");
        assert_eq!(summaries[1].id, "newer");
    }

    #[test]
    fn conversation_text_search_treats_like_wildcards_as_text() {
        let (_directory, database) = database();
        database
            .save_conversation(&WorkConversation {
                id: "wildcards".into(),
                title: "Wildcards".into(),
                project_id: None,
                pinned: false,
                unread: false,
                created_at: 1,
                updated_at: 1,
                binding: SessionBinding::default(),
                context_summary: None,
                context_usage: crate::agent::ContextUsage::default(),
                messages: vec![WorkMessage {
                    role: WorkMessageRole::User,
                    text: "A literal 100% result".into(),
                    reasoning: String::new(),
                    reasoning_complete: true,
                    reasoning_expanded: false,
                    tool_activities: Vec::new(),
                    expanded_tool_groups: Vec::new(),
                }],
                checkpoints: Vec::new(),
                tasks: Vec::new(),
                sources: Vec::new(),
                agent_threads: Vec::new(),
                agent_thread_transcripts: std::collections::HashMap::new(),
            })
            .unwrap();

        assert_eq!(
            database.search_conversations_text("100%", 10).unwrap()[0].conversation_id,
            "wildcards"
        );
        assert!(database
            .search_conversations_text("_", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn embedding_fragments_round_trip_and_are_invalidated_by_new_messages() {
        let (_directory, database) = database();
        let conversation = WorkConversation {
            id: "indexed".into(),
            title: "Indexed conversation".into(),
            project_id: None,
            pinned: false,
            unread: false,
            created_at: 1,
            updated_at: 1,
            binding: SessionBinding::default(),
            context_summary: None,
            context_usage: crate::agent::ContextUsage::default(),
            messages: vec![WorkMessage {
                role: WorkMessageRole::User,
                text: "Remember this decision".into(),
                reasoning: String::new(),
                reasoning_complete: true,
                reasoning_expanded: false,
                tool_activities: Vec::new(),
                expanded_tool_groups: Vec::new(),
            }],
            checkpoints: Vec::new(),
            tasks: Vec::new(),
            sources: Vec::new(),
            agent_threads: Vec::new(),
            agent_thread_transcripts: std::collections::HashMap::new(),
        };
        database.save_conversation(&conversation).unwrap();
        let embedding_config = EmbeddingConfig {
            connection_id: ConnectionId("openai".into()),
            model_id: "text-embedding-test".into(),
        };
        database.save_embedding_config(&embedding_config).unwrap();
        let fragments = vec![crate::memory::ConversationFragment {
            message_position: 0,
            chunk_index: 0,
            text: "User: Remember this decision".into(),
            content_hash: crate::memory::content_hash("User: Remember this decision"),
        }];
        database
            .replace_conversation_embeddings(
                "indexed",
                &ConnectionId("openai".into()),
                "text-embedding-test",
                &fragments,
                &[vec![1.0, 0.0]],
            )
            .unwrap();
        let status = database.embedding_index_status().unwrap();
        assert_eq!(status.indexed_conversations, 1);
        assert_eq!(status.indexed_fragments, 1);
        if database.vector_index_available() {
            assert_eq!(database.rebuild_vector_index(&embedding_config).unwrap(), 1);
            let hits = database
                .search_conversations_vector(&embedding_config, &[1.0, 0.0], 5)
                .unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].conversation_id, "indexed");
        }
        assert_eq!(
            database
                .search_conversations_text("decision", 5)
                .unwrap()
                .len(),
            1
        );

        let mut changed = conversation;
        changed.messages[0].text = "A different decision".into();
        changed.updated_at = 2;
        database.save_conversation(&changed).unwrap();
        assert_eq!(
            database.embedding_index_status().unwrap().indexed_fragments,
            0
        );
    }

    #[test]
    fn incomplete_binding_does_not_replace_the_last_complete_pair() {
        let (_directory, database) = database();
        let complete = SessionBinding {
            connection_id: Some(ConnectionId("openai-work".into())),
            model_id: Some("gpt-5.2".into()),
            reasoning_effort: None,
            tools: vec!["bash".into()],
        };
        database.remember_binding(&complete).unwrap();
        database
            .remember_binding(&SessionBinding {
                connection_id: Some(ConnectionId("anthropic-work".into())),
                model_id: None,
                reasoning_effort: None,
                tools: vec!["web_fetch".into()],
            })
            .unwrap();
        assert_eq!(database.last_binding().unwrap(), Some(complete));
    }

    #[test]
    fn deleting_a_connection_forgets_its_remembered_pair() {
        let (_directory, database) = database();
        let connection_id = ConnectionId("removed".into());
        database
            .remember_binding(&SessionBinding {
                connection_id: Some(connection_id.clone()),
                model_id: Some("model".into()),
                reasoning_effort: None,
                tools: vec!["file_read".into()],
            })
            .unwrap();
        database
            .forget_binding_for_connection(&connection_id)
            .unwrap();
        assert_eq!(database.last_binding().unwrap(), None);
    }

    #[test]
    fn round_trips_tool_and_agent_history() {
        use crate::agent::orchestration::{AgentThreadSnapshot, AgentThreadStatus};

        let (_directory, database) = database();
        let timestamp = now();
        let activity = WorkToolActivity {
            call_id: Some("call-1".into()),
            name: "web_fetch".into(),
            text_offset: 0,
            group_id: Some(2),
            input: "{\"url\":\"https://example.com\"}".into(),
            summary: "Example page".into(),
            output: "partial page output".into(),
            state: WorkToolActivityState::Completed,
            duration_ms: Some(125),
            expanded: true,
            inside_reasoning: false,
        };
        let conversation = WorkConversation {
            id: "conversation-with-history".into(),
            title: "History".into(),
            project_id: None,
            pinned: false,
            unread: false,
            created_at: timestamp,
            updated_at: timestamp,
            binding: SessionBinding::default(),
            context_summary: None,
            context_usage: crate::agent::ContextUsage::default(),
            messages: vec![WorkMessage {
                role: WorkMessageRole::Assistant,
                text: "Answer".into(),
                reasoning: "Reasoning".into(),
                reasoning_complete: true,
                reasoning_expanded: true,
                tool_activities: vec![activity.clone()],
                expanded_tool_groups: vec![2],
            }],
            checkpoints: Vec::new(),
            tasks: Vec::new(),
            sources: Vec::new(),
            agent_threads: vec![AgentThreadSnapshot {
                id: "thread-1".into(),
                thread_id: "thread-1".into(),
                agent_id: "researcher".into(),
                parent_session_id: "conversation-with-history".into(),
                title: "Research".into(),
                model_id: "model-1".into(),
                status: AgentThreadStatus::Completed,
                enabled_tools: vec!["discover_tools".into(), "web_fetch".into()],
                prompt: "Find the answer".into(),
                output: "Agent answer".into(),
                created_at: timestamp,
                updated_at: timestamp,
            }],
            agent_thread_transcripts: std::collections::HashMap::from([(
                "thread-1".into(),
                vec![WorkMessage {
                    role: WorkMessageRole::Assistant,
                    text: "Agent answer".into(),
                    reasoning: "Agent reasoning".into(),
                    reasoning_complete: true,
                    reasoning_expanded: false,
                    tool_activities: vec![activity],
                    expanded_tool_groups: Vec::new(),
                }],
            )]),
        };

        database.save_conversation(&conversation).unwrap();
        let database_path = database.path().to_path_buf();
        drop(database);
        let database = WorkDatabase::open_at(database_path).unwrap();
        assert_eq!(
            database.conversation(&conversation.id).unwrap(),
            Some(conversation)
        );
    }
}
