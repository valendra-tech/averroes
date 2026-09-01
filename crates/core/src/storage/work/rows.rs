use super::types::{CheckpointStatus, TaskStatus, WorkMessage, WorkMessageRole};
use super::{WorkCheckpoint, WorkConversation, WorkDatabaseError, WorkSource, WorkTask};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

pub(super) fn replace_messages(
    transaction: &Transaction<'_>,
    conversation: &WorkConversation,
) -> Result<(), WorkDatabaseError> {
    if content_equal(transaction, conversation)? {
        return Ok(());
    }
    transaction.execute(
        "DELETE FROM conversation_embeddings WHERE conversation_id = ?1",
        params![conversation.id],
    )?;
    transaction.execute(
        "DELETE FROM messages WHERE conversation_id = ?1",
        params![conversation.id],
    )?;
    let mut statement = transaction.prepare(
        "INSERT INTO messages (conversation_id, position, role, text, reasoning)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (position, message) in conversation.messages.iter().enumerate() {
        statement.execute(params![
            conversation.id,
            position as i64,
            message.role.as_str(),
            message.text,
            message.reasoning,
        ])?;
    }
    Ok(())
}

pub(super) fn messages_equal(
    connection: &Connection,
    conversation: &WorkConversation,
) -> Result<bool, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT role, text, reasoning FROM messages
         WHERE conversation_id = ?1 ORDER BY position",
    )?;
    let rows = statement.query_map(params![conversation.id], |row| {
        Ok(WorkMessage {
            role: WorkMessageRole::parse(&row.get::<_, String>(0)?),
            text: row.get(1)?,
            reasoning: row.get(2)?,
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
    let stored_context = connection
        .query_row(
            "SELECT context_summary FROM conversations WHERE id = ?1",
            params![conversation.id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(stored_context == conversation.context_summary)
}

pub(super) fn load_messages(
    connection: &Connection,
    conversation_id: &str,
) -> Result<Vec<WorkMessage>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT role, text, reasoning FROM messages
         WHERE conversation_id = ?1 ORDER BY position",
    )?;
    let rows = statement.query_map(params![conversation_id], |row| {
        Ok(WorkMessage {
            role: WorkMessageRole::parse(&row.get::<_, String>(0)?),
            text: row.get(1)?,
            reasoning: row.get(2)?,
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
            (conversation_id, task_id, title, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(conversation_id, task_id) DO UPDATE SET
            title = excluded.title,
            status = excluded.status,
            updated_at = excluded.updated_at",
        params![
            conversation_id,
            task.id,
            task.title,
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
        "SELECT task_id, title, status, created_at, updated_at FROM tasks
         WHERE conversation_id = ?1
         ORDER BY CASE status WHEN 'pending' THEN 0 ELSE 1 END, created_at, task_id",
    )?;
    let rows = statement.query_map(params![conversation_id], |row| {
        Ok(WorkTask {
            id: row.get(0)?,
            title: row.get(1)?,
            status: TaskStatus::parse(&row.get::<_, String>(2)?),
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
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
