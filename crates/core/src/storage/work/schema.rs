use super::types::WorkProject;
use rusqlite::Connection;
use std::path::PathBuf;

pub(super) fn migrate(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            root TEXT NOT NULL UNIQUE,
            created_at INTEGER NOT NULL,
            last_opened_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS conversations (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
            pinned INTEGER NOT NULL DEFAULT 0,
            unread INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            binding_json TEXT NOT NULL DEFAULT '{}',
            context_summary TEXT,
            context_usage_json TEXT NOT NULL DEFAULT '{}'
        );
        -- Pinned is a presentation group, not a recency signal. Keep the
        -- storage index aligned with the query used by the sidebar/search.
        DROP INDEX IF EXISTS conversations_updated;
        CREATE INDEX conversations_updated
            ON conversations(updated_at DESC, id);
        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            role TEXT NOT NULL,
            text TEXT NOT NULL,
            reasoning TEXT NOT NULL DEFAULT '',
            UNIQUE(conversation_id, position)
        );
        CREATE INDEX IF NOT EXISTS messages_conversation_id
            ON messages(conversation_id, id);
        CREATE TABLE IF NOT EXISTS checkpoints (
            conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            checkpoint_id TEXT NOT NULL,
            title TEXT NOT NULL,
            status TEXT NOT NULL,
            detail TEXT,
            message_position INTEGER,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(conversation_id, checkpoint_id)
        );
        CREATE TABLE IF NOT EXISTS tasks (
            conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            task_id TEXT NOT NULL,
            title TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(conversation_id, task_id)
        );
        CREATE INDEX IF NOT EXISTS tasks_conversation_updated
            ON tasks(conversation_id, status, updated_at, task_id);
        CREATE TABLE IF NOT EXISTS sources (
            conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            source_key TEXT NOT NULL,
            kind TEXT NOT NULL,
            label TEXT NOT NULL,
            url TEXT,
            title TEXT,
            detail TEXT,
            use_count INTEGER NOT NULL DEFAULT 1,
            last_used_at INTEGER NOT NULL,
            PRIMARY KEY(conversation_id, source_key)
        );
        CREATE TABLE IF NOT EXISTS preferences (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS global_memories (
            id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS global_memories_updated
            ON global_memories(updated_at ASC, id ASC);
        CREATE TABLE IF NOT EXISTS conversation_embeddings (
            conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            message_position INTEGER NOT NULL,
            chunk_index INTEGER NOT NULL,
            text TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            connection_id TEXT NOT NULL DEFAULT '',
            model_id TEXT NOT NULL,
            embedding BLOB NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(conversation_id, message_position, chunk_index)
        );
        CREATE INDEX IF NOT EXISTS conversation_embeddings_hash
            ON conversation_embeddings(content_hash);",
    )?;
    if !conversation_has_column(connection, "unread")? {
        connection.execute(
            "ALTER TABLE conversations ADD COLUMN unread INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !conversation_has_column(connection, "context_summary")? {
        connection.execute(
            "ALTER TABLE conversations ADD COLUMN context_summary TEXT",
            [],
        )?;
    }
    if !conversation_has_column(connection, "context_usage_json")? {
        connection.execute(
            "ALTER TABLE conversations ADD COLUMN context_usage_json TEXT NOT NULL DEFAULT '{}'",
            [],
        )?;
    }
    if !table_has_column(connection, "conversation_embeddings", "connection_id")? {
        connection.execute(
            "ALTER TABLE conversation_embeddings ADD COLUMN connection_id TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !table_has_column(connection, "checkpoints", "message_position")? {
        connection.execute(
            "ALTER TABLE checkpoints ADD COLUMN message_position INTEGER",
            [],
        )?;
    }
    if !table_has_column(connection, "sources", "url")? {
        connection.execute("ALTER TABLE sources ADD COLUMN url TEXT", [])?;
    }
    if !table_has_column(connection, "sources", "title")? {
        connection.execute("ALTER TABLE sources ADD COLUMN title TEXT", [])?;
    }
    connection.execute_batch(
        "DROP INDEX IF EXISTS conversation_embeddings_model;
         CREATE INDEX conversation_embeddings_model
             ON conversation_embeddings(connection_id, model_id, conversation_id);",
    )?;
    connection.pragma_update(None, "user_version", 10)?;
    Ok(())
}

fn conversation_has_column(connection: &Connection, column: &str) -> rusqlite::Result<bool> {
    table_has_column(connection, "conversations", column)
}

fn table_has_column(connection: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    // Table names are internal constants at every call site; they cannot be
    // bound as SQLite parameters, so keep this helper private to migrations.
    let table = table.replace('"', "\"\"");
    let mut statement = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for existing in columns {
        if existing? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkProject> {
    Ok(WorkProject {
        id: row.get(0)?,
        name: row.get(1)?,
        root: PathBuf::from(row.get::<_, String>(2)?),
        created_at: row.get(3)?,
        last_opened_at: row.get(4)?,
    })
}
