use super::types::{
    note_search_key, CheckpointStatus, TaskPriority, TaskStatus, WorkHistoryEntry, WorkHistoryKind,
    WorkMessage, WorkMessageRole, WorkNote, WorkNoteSearchPage,
};
use super::{WorkCheckpoint, WorkConversation, WorkDatabaseError, WorkSource, WorkTask};
use rusqlite::{params, types::Type, Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;

fn json_column<T: DeserializeOwned>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T> {
    let value = row.get::<_, String>(index)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
    })
}

fn history_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkHistoryEntry> {
    history_entry_from_row_at(row, 0)
}

fn history_entry_from_row_at(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<WorkHistoryEntry> {
    Ok(WorkHistoryEntry {
        entry_id: row.get(offset)?,
        parent_id: row.get(offset + 1)?,
        thread_id: row.get(offset + 2)?,
        window_id: row.get(offset + 3)?,
        sequence: row.get(offset + 4)?,
        timestamp: row.get(offset + 5)?,
        kind: WorkHistoryKind::parse(&row.get::<_, String>(offset + 6)?),
        text: row.get(offset + 7)?,
        payload: json_column(row, offset + 8)?,
        images: json_column(row, offset + 9)?,
    })
}

fn history_entry_with_conversation_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, WorkHistoryEntry)> {
    Ok((row.get(0)?, history_entry_from_row_at(row, 1)?))
}

pub(super) fn append_history_entries(
    transaction: &Transaction<'_>,
    conversation_id: &str,
    entries: &[WorkHistoryEntry],
) -> Result<(), WorkDatabaseError> {
    for entry in entries {
        let payload = serde_json::to_string(&entry.payload)?;
        let images = serde_json::to_string(&entry.images)?;
        let inserted = match transaction.execute(
            "INSERT INTO conversation_history
            (conversation_id, entry_id, parent_id, thread_id, window_id, sequence,
             timestamp, kind, text, payload_json, images_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(conversation_id, entry_id) DO NOTHING",
            params![
                conversation_id,
                entry.entry_id,
                entry.parent_id,
                entry.thread_id,
                entry.window_id,
                entry.sequence,
                entry.timestamp,
                entry.kind.as_str(),
                entry.text,
                payload,
                images,
            ],
        ) {
            Ok(inserted) => inserted,
            Err(error) => {
                match transaction
                    .query_row(
                        "SELECT entry_id FROM conversation_history
                         WHERE conversation_id = ?1 AND sequence = ?2",
                        params![conversation_id, entry.sequence],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                {
                    Ok(Some(existing_entry_id)) => {
                        return Err(history_conflict(conversation_id, entry, existing_entry_id));
                    }
                    Ok(None) => return Err(error.into()),
                    Err(query_error) => return Err(query_error.into()),
                }
            }
        };

        if inserted == 0 {
            let Some(existing) = load_history_entry(transaction, conversation_id, &entry.entry_id)?
            else {
                return Err(history_conflict(
                    conversation_id,
                    entry,
                    entry.entry_id.clone(),
                ));
            };
            if existing == *entry {
                continue;
            }
            return Err(history_conflict(conversation_id, entry, existing.entry_id));
        }
    }
    Ok(())
}

fn history_conflict(
    conversation_id: &str,
    entry: &WorkHistoryEntry,
    existing_entry_id: String,
) -> WorkDatabaseError {
    WorkDatabaseError::HistoryConflict {
        conversation_id: conversation_id.into(),
        sequence: entry.sequence,
        entry_id: entry.entry_id.clone(),
        existing_entry_id,
    }
}

pub(super) fn load_history_entry(
    connection: &Connection,
    conversation_id: &str,
    entry_id: &str,
) -> Result<Option<WorkHistoryEntry>, WorkDatabaseError> {
    connection
        .query_row(
            "SELECT entry_id, parent_id, thread_id, window_id, sequence, timestamp,
                    kind, text, payload_json, images_json
             FROM conversation_history
             WHERE conversation_id = ?1 AND entry_id = ?2",
            params![conversation_id, entry_id],
            history_entry_from_row,
        )
        .optional()
        .map_err(Into::into)
}

pub(super) fn load_history_entries(
    connection: &Connection,
    conversation_id: &str,
) -> Result<Vec<WorkHistoryEntry>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT entry_id, parent_id, thread_id, window_id, sequence, timestamp,
                kind, text, payload_json, images_json
         FROM conversation_history
         WHERE conversation_id = ?1
         ORDER BY sequence, entry_id",
    )?;
    let rows = statement.query_map(params![conversation_id], history_entry_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn search_history(
    connection: &Connection,
    conversation_id: &str,
    query: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<WorkHistoryEntry>, WorkDatabaseError> {
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let pattern = format!("%{}%", escape_like_pattern(query.trim()));
    let mut statement = connection.prepare(
        "SELECT entry_id, parent_id, thread_id, window_id, sequence, timestamp,
                kind, text, payload_json, images_json
         FROM conversation_history
         WHERE conversation_id = ?1
           AND text LIKE ?2 COLLATE NOCASE ESCAPE '\\'
         ORDER BY sequence, entry_id
         LIMIT ?3 OFFSET ?4",
    )?;
    let rows = statement.query_map(
        params![conversation_id, pattern, limit as i64, offset as i64],
        history_entry_from_row,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn list_history_workspace(
    connection: &Connection,
    workspace_root: &str,
) -> Result<Vec<(String, WorkHistoryEntry)>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT h.conversation_id, h.entry_id, h.parent_id, h.thread_id, h.window_id,
                h.sequence, h.timestamp, h.kind, h.text, h.payload_json, h.images_json
         FROM conversation_history h
         JOIN conversations c ON c.id = h.conversation_id
         JOIN projects p ON p.id = c.project_id
         WHERE p.root = ?1
         ORDER BY h.sequence, h.entry_id, h.conversation_id",
    )?;
    let rows = statement.query_map(
        params![workspace_root],
        history_entry_with_conversation_from_row,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn search_history_workspace(
    connection: &Connection,
    workspace_root: &str,
    query: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<(String, WorkHistoryEntry)>, WorkDatabaseError> {
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let pattern = format!("%{}%", escape_like_pattern(query.trim()));
    let mut statement = connection.prepare(
        "SELECT h.conversation_id, h.entry_id, h.parent_id, h.thread_id, h.window_id,
                h.sequence, h.timestamp, h.kind, h.text, h.payload_json, h.images_json
         FROM conversation_history h
         JOIN conversations c ON c.id = h.conversation_id
         JOIN projects p ON p.id = c.project_id
         WHERE p.root = ?1
           AND h.text LIKE ?2 COLLATE NOCASE ESCAPE '\\'
         ORDER BY h.sequence, h.entry_id, h.conversation_id
         LIMIT ?3 OFFSET ?4",
    )?;
    let rows = statement.query_map(
        params![workspace_root, pattern, limit as i64, offset as i64],
        history_entry_with_conversation_from_row,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn load_history_entry_in_scope(
    connection: &Connection,
    conversation_id: &str,
    workspace_root: &str,
    entry_id: &str,
) -> Result<Option<(String, WorkHistoryEntry)>, WorkDatabaseError> {
    connection
        .query_row(
            "SELECT h.conversation_id, h.entry_id, h.parent_id, h.thread_id, h.window_id,
                    h.sequence, h.timestamp, h.kind, h.text, h.payload_json, h.images_json
             FROM conversation_history h
             LEFT JOIN conversations c ON c.id = h.conversation_id
             LEFT JOIN projects p ON p.id = c.project_id
             WHERE h.entry_id = ?3
               AND (h.conversation_id = ?1 OR p.root = ?2)
             ORDER BY CASE WHEN h.conversation_id = ?1 THEN 0 ELSE 1 END,
                      h.sequence, h.conversation_id
             LIMIT 1",
            params![conversation_id, workspace_root, entry_id],
            history_entry_with_conversation_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

pub(super) fn upsert_note(
    transaction: &Transaction<'_>,
    note: &WorkNote,
) -> Result<(), WorkDatabaseError> {
    transaction.execute(
        "INSERT INTO notes
            (workspace_root, path, content, path_search, content_search, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(workspace_root, path) DO UPDATE SET
            content = excluded.content,
            path_search = excluded.path_search,
            content_search = excluded.content_search,
            updated_at = excluded.updated_at",
        params![
            note.workspace_root,
            note.path,
            note.content,
            note_search_key(&note.path),
            note_search_key(&note.content),
            note.created_at,
            note.updated_at,
        ],
    )?;
    Ok(())
}

pub(super) fn load_note(
    connection: &Connection,
    workspace_root: &str,
    path: &str,
) -> Result<Option<WorkNote>, WorkDatabaseError> {
    connection
        .query_row(
            "SELECT workspace_root, path, content, created_at, updated_at
             FROM notes WHERE workspace_root = ?1 AND path = ?2",
            params![workspace_root, path],
            |row| {
                Ok(WorkNote {
                    workspace_root: row.get(0)?,
                    path: row.get(1)?,
                    content: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub(super) fn list_notes(
    connection: &Connection,
    workspace_root: &str,
) -> Result<Vec<WorkNote>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT workspace_root, path, content, created_at, updated_at
         FROM notes
         WHERE workspace_root = ?1
         ORDER BY path COLLATE NOCASE, path",
    )?;
    let rows = statement.query_map(params![workspace_root], |row| {
        Ok(WorkNote {
            workspace_root: row.get(0)?,
            path: row.get(1)?,
            content: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn search_notes(
    connection: &Connection,
    workspace_root: &str,
    query: &str,
) -> Result<Vec<WorkNote>, WorkDatabaseError> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let pattern = format!("%{}%", escape_like_pattern(&note_search_key(query.trim())));
    let mut statement = connection.prepare(
        "SELECT workspace_root, path, content, created_at, updated_at
         FROM notes
         WHERE workspace_root = ?1
           AND (path_search LIKE ?2 ESCAPE '\\'
                OR content_search LIKE ?2 ESCAPE '\\')
         ORDER BY updated_at DESC, path COLLATE NOCASE, path",
    )?;
    let rows = statement.query_map(params![workspace_root, pattern], |row| {
        Ok(WorkNote {
            workspace_root: row.get(0)?,
            path: row.get(1)?,
            content: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn search_notes_page(
    connection: &Connection,
    workspace_root: &str,
    query: &str,
    limit: usize,
    offset: usize,
) -> Result<WorkNoteSearchPage, WorkDatabaseError> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(WorkNoteSearchPage {
            notes: Vec::new(),
            total: 0,
        });
    }
    let pattern = format!("%{}%", escape_like_pattern(&note_search_key(query)));
    let total = connection.query_row(
        "SELECT COUNT(*)
         FROM notes
         WHERE workspace_root = ?1
           AND (path_search LIKE ?2 ESCAPE '\\'
                OR content_search LIKE ?2 ESCAPE '\\')",
        params![workspace_root, pattern],
        |row| row.get::<_, i64>(0),
    )?;
    if limit == 0 {
        return Ok(WorkNoteSearchPage {
            notes: Vec::new(),
            total: total as usize,
        });
    }
    let mut statement = connection.prepare(
        "SELECT workspace_root, path, content, created_at, updated_at
         FROM notes
         WHERE workspace_root = ?1
           AND (path_search LIKE ?2 ESCAPE '\\'
                OR content_search LIKE ?2 ESCAPE '\\')
         ORDER BY updated_at DESC, path COLLATE NOCASE, path
         LIMIT ?3 OFFSET ?4",
    )?;
    let rows = statement.query_map(
        params![
            workspace_root,
            pattern,
            i64::try_from(limit).unwrap_or(i64::MAX),
            i64::try_from(offset).unwrap_or(i64::MAX),
        ],
        |row| {
            Ok(WorkNote {
                workspace_root: row.get(0)?,
                path: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        },
    )?;
    Ok(WorkNoteSearchPage {
        notes: rows.collect::<rusqlite::Result<Vec<_>>>()?,
        total: total as usize,
    })
}

pub(super) fn replace_messages(
    transaction: &Transaction<'_>,
    conversation: &WorkConversation,
) -> Result<(), WorkDatabaseError> {
    if content_equal(transaction, conversation)? {
        return Ok(());
    }
    if !indexed_messages_equal(transaction, conversation)? {
        transaction.execute(
            "DELETE FROM conversation_embeddings WHERE conversation_id = ?1",
            params![conversation.id],
        )?;
    }
    transaction.execute(
        "DELETE FROM messages WHERE conversation_id = ?1",
        params![conversation.id],
    )?;
    let mut statement = transaction.prepare(
        "INSERT INTO messages
            (conversation_id, position, role, text, reasoning, reasoning_complete,
             reasoning_expanded, tool_activities_json, expanded_tool_groups_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for (position, message) in conversation.messages.iter().enumerate() {
        statement.execute(params![
            conversation.id,
            position as i64,
            message.role.as_str(),
            message.text,
            message.reasoning,
            message.reasoning_complete as i64,
            message.reasoning_expanded as i64,
            serde_json::to_string(&message.tool_activities)?,
            serde_json::to_string(&message.expanded_tool_groups)?,
        ])?;
    }
    Ok(())
}

/// Presentation-only changes (for example expanding a tool result) must not
/// invalidate semantic embeddings. The index currently contains the visible
/// message text and reasoning, so compare only those fields here.
fn indexed_messages_equal(
    connection: &Connection,
    conversation: &WorkConversation,
) -> Result<bool, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT role, text, reasoning FROM messages
         WHERE conversation_id = ?1 ORDER BY position",
    )?;
    let rows = statement.query_map(params![conversation.id], |row| {
        Ok((
            WorkMessageRole::parse(&row.get::<_, String>(0)?),
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let stored = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let current = conversation
        .messages
        .iter()
        .map(|message| {
            (
                message.role,
                message.text.clone(),
                message.reasoning.clone(),
            )
        })
        .collect::<Vec<_>>();
    Ok(stored == current)
}

pub(super) fn messages_equal(
    connection: &Connection,
    conversation: &WorkConversation,
) -> Result<bool, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT role, text, reasoning, reasoning_complete, reasoning_expanded,
                tool_activities_json, expanded_tool_groups_json
         FROM messages
         WHERE conversation_id = ?1 ORDER BY position",
    )?;
    let rows = statement.query_map(params![conversation.id], |row| {
        Ok(WorkMessage {
            role: WorkMessageRole::parse(&row.get::<_, String>(0)?),
            text: row.get(1)?,
            reasoning: row.get(2)?,
            reasoning_complete: row.get::<_, i64>(3)? != 0,
            reasoning_expanded: row.get::<_, i64>(4)? != 0,
            tool_activities: json_column(row, 5)?,
            expanded_tool_groups: json_column(row, 6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()? == conversation.messages)
}

pub(super) fn content_equal(
    connection: &Connection,
    conversation: &WorkConversation,
) -> Result<bool, WorkDatabaseError> {
    if !messages_equal(connection, conversation)? {
        return Ok(false);
    }
    let stored = connection
        .query_row(
            "SELECT context_summary, agent_threads_json, agent_thread_transcripts_json,
                    active_context_json, active_window_id
             FROM conversations WHERE id = ?1",
            params![conversation.id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((
        stored_context,
        stored_agents,
        stored_transcripts,
        stored_active_context,
        stored_active_window,
    )) = stored
    else {
        return Ok(false);
    };
    let stored_agents = serde_json::from_str::<serde_json::Value>(&stored_agents)?;
    let stored_transcripts = serde_json::from_str::<serde_json::Value>(&stored_transcripts)?;
    let stored_active_context = serde_json::from_str::<serde_json::Value>(&stored_active_context)?;
    Ok(stored_context == conversation.context_summary
        && stored_agents == serde_json::to_value(&conversation.agent_threads)?
        && stored_transcripts == serde_json::to_value(&conversation.agent_thread_transcripts)?
        && stored_active_context == serde_json::to_value(&conversation.active_context)?
        && stored_active_window == conversation.active_window_id)
}

pub(super) fn load_messages(
    connection: &Connection,
    conversation_id: &str,
) -> Result<Vec<WorkMessage>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT role, text, reasoning, reasoning_complete, reasoning_expanded,
                tool_activities_json, expanded_tool_groups_json
         FROM messages
         WHERE conversation_id = ?1 ORDER BY position",
    )?;
    let rows = statement.query_map(params![conversation_id], |row| {
        Ok(WorkMessage {
            role: WorkMessageRole::parse(&row.get::<_, String>(0)?),
            text: row.get(1)?,
            reasoning: row.get(2)?,
            reasoning_complete: row.get::<_, i64>(3)? != 0,
            reasoning_expanded: row.get::<_, i64>(4)? != 0,
            tool_activities: json_column(row, 5)?,
            expanded_tool_groups: json_column(row, 6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn upsert_checkpoint_connection(
    connection: &Connection,
    conversation_id: &str,
    checkpoint: &WorkCheckpoint,
) -> Result<(), WorkDatabaseError> {
    connection.execute(
        "INSERT INTO checkpoints
            (conversation_id, checkpoint_id, title, status, detail, message_position, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(conversation_id, checkpoint_id) DO UPDATE SET
            title = excluded.title,
            status = excluded.status,
            detail = excluded.detail,
            message_position = COALESCE(excluded.message_position, checkpoints.message_position),
            updated_at = excluded.updated_at",
        params![
            conversation_id,
            checkpoint.id,
            checkpoint.title,
            checkpoint.status.as_str(),
            checkpoint.detail,
            checkpoint.message_position.map(|position| position as i64),
            checkpoint.updated_at,
        ],
    )?;
    Ok(())
}

pub(super) fn upsert_checkpoint_tx(
    transaction: &Transaction<'_>,
    conversation_id: &str,
    checkpoint: &WorkCheckpoint,
) -> Result<(), WorkDatabaseError> {
    upsert_checkpoint_connection(transaction, conversation_id, checkpoint)
}

pub(super) fn load_checkpoints(
    connection: &Connection,
    conversation_id: &str,
) -> Result<Vec<WorkCheckpoint>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT checkpoint_id, title, status, detail, message_position, updated_at FROM checkpoints
         WHERE conversation_id = ?1 ORDER BY updated_at, rowid",
    )?;
    let rows = statement.query_map(params![conversation_id], |row| {
        Ok(WorkCheckpoint {
            id: row.get(0)?,
            title: row.get(1)?,
            status: CheckpointStatus::parse(&row.get::<_, String>(2)?),
            detail: row.get(3)?,
            message_position: row
                .get::<_, Option<i64>>(4)?
                .map(|position| position as usize),
            updated_at: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn upsert_task_connection(
    connection: &Connection,
    conversation_id: &str,
    task: &WorkTask,
) -> Result<(), WorkDatabaseError> {
    connection.execute(
        "INSERT INTO tasks
            (conversation_id, task_id, title, description, parent_task_id, depends_on_json,
             priority, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(conversation_id, task_id) DO UPDATE SET
            title = excluded.title,
            description = excluded.description,
            parent_task_id = excluded.parent_task_id,
            depends_on_json = excluded.depends_on_json,
            priority = excluded.priority,
            status = excluded.status,
            updated_at = excluded.updated_at",
        params![
            conversation_id,
            task.id,
            task.title,
            task.description,
            task.parent_task_id,
            serde_json::to_string(&task.depends_on)?,
            task.priority.as_str(),
            task.status.as_str(),
            task.created_at,
            task.updated_at,
        ],
    )?;
    Ok(())
}

pub(super) fn upsert_task_tx(
    transaction: &Transaction<'_>,
    conversation_id: &str,
    task: &WorkTask,
) -> Result<(), WorkDatabaseError> {
    upsert_task_connection(transaction, conversation_id, task)
}

pub(super) fn load_tasks(
    connection: &Connection,
    conversation_id: &str,
) -> Result<Vec<WorkTask>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT task_id, title, description, parent_task_id, depends_on_json, priority,
                status, created_at, updated_at
         FROM tasks
         WHERE conversation_id = ?1
         ORDER BY CASE status
                    WHEN 'in_progress' THEN 0
                    WHEN 'pending' THEN 1
                    WHEN 'blocked' THEN 2
                    WHEN 'done' THEN 3
                    ELSE 4
                  END,
                  CASE priority WHEN 'high' THEN 0 WHEN 'normal' THEN 1 ELSE 2 END,
                  created_at, task_id",
    )?;
    let rows = statement.query_map(params![conversation_id], |row| {
        Ok(WorkTask {
            id: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            parent_task_id: row.get(3)?,
            depends_on: json_column(row, 4)?,
            priority: TaskPriority::parse(&row.get::<_, String>(5)?),
            status: TaskStatus::parse(&row.get::<_, String>(6)?),
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn upsert_source_connection(
    connection: &Connection,
    conversation_id: &str,
    source: &WorkSource,
) -> Result<(), WorkDatabaseError> {
    connection.execute(
        "INSERT INTO sources
            (conversation_id, source_key, kind, label, url, title, detail, use_count, last_used_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(conversation_id, source_key) DO UPDATE SET
            kind = excluded.kind,
            label = excluded.label,
            url = excluded.url,
            title = excluded.title,
            detail = excluded.detail,
            use_count = MAX(sources.use_count, excluded.use_count),
            last_used_at = excluded.last_used_at",
        params![
            conversation_id,
            source.key,
            source.kind,
            source.label,
            source.url,
            source.title,
            source.detail,
            source.count,
            source.last_used_at,
        ],
    )?;
    Ok(())
}

pub(super) fn upsert_source_tx(
    transaction: &Transaction<'_>,
    conversation_id: &str,
    source: &WorkSource,
) -> Result<(), WorkDatabaseError> {
    upsert_source_connection(transaction, conversation_id, source)
}

pub(super) fn load_sources(
    connection: &Connection,
    conversation_id: &str,
) -> Result<Vec<WorkSource>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT source_key, kind, label, url, title, detail, use_count, last_used_at FROM sources
         WHERE conversation_id = ?1 ORDER BY last_used_at DESC",
    )?;
    let rows = statement.query_map(params![conversation_id], |row| {
        Ok(WorkSource {
            key: row.get(0)?,
            kind: row.get(1)?,
            label: row.get(2)?,
            url: row.get(3)?,
            title: row.get(4)?,
            detail: row.get(5)?,
            count: row.get(6)?,
            last_used_at: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
