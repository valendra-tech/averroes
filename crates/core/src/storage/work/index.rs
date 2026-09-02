use super::types::{
    ConversationSearchResult, EmbeddingIndexStatus, IndexedConversationFragment, VectorSearchHit,
};
use super::{WorkDatabaseError, WorkMessage, WorkMessageRole};
use crate::memory::{decode_embedding, encode_embedding, ConversationFragment};
use crate::work::EmbeddingConfig;
use rusqlite::{params, Connection, OptionalExtension, Transaction};

pub(super) fn load_documents(
    connection: &Connection,
) -> Result<Vec<super::ConversationDocument>, WorkDatabaseError> {
    let mut statement = connection
        .prepare("SELECT id, context_summary FROM conversations ORDER BY updated_at DESC")?;
    let ids = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    load_documents_for_ids(connection, ids)
}

pub(super) fn load_pending_documents(
    connection: &Connection,
    config: &EmbeddingConfig,
) -> Result<Vec<super::ConversationDocument>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT c.id, c.context_summary FROM conversations c
         WHERE NOT EXISTS (
             SELECT 1 FROM conversation_embeddings e
             WHERE e.conversation_id = c.id
               AND e.connection_id = ?1
               AND e.model_id = ?2
         )
           AND (
               EXISTS (
                   SELECT 1 FROM messages m
                   WHERE m.conversation_id = c.id AND length(trim(m.text)) > 0
               )
               OR length(trim(COALESCE(c.context_summary, ''))) > 0
           )
         ORDER BY c.updated_at DESC, c.id",
    )?;
    let ids = statement
        .query_map(params![&config.connection_id.0, &config.model_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    load_documents_for_ids(connection, ids)
}

pub(super) fn pending_document_count(
    connection: &Connection,
    config: &EmbeddingConfig,
) -> Result<usize, WorkDatabaseError> {
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM conversations c
         WHERE NOT EXISTS (
             SELECT 1 FROM conversation_embeddings e
             WHERE e.conversation_id = c.id
               AND e.connection_id = ?1
               AND e.model_id = ?2
         )
           AND (
               EXISTS (
                   SELECT 1 FROM messages m
                   WHERE m.conversation_id = c.id AND length(trim(m.text)) > 0
               )
               OR length(trim(COALESCE(c.context_summary, ''))) > 0
           )",
        params![&config.connection_id.0, &config.model_id],
        |row| row.get::<_, i64>(0),
    )? as usize)
}

fn load_documents_for_ids(
    connection: &Connection,
    ids: Vec<(String, Option<String>)>,
) -> Result<Vec<super::ConversationDocument>, WorkDatabaseError> {
    ids.into_iter()
        .map(|(id, context_summary)| {
            let mut messages = connection.prepare(
                "SELECT role, text, reasoning FROM messages
                 WHERE conversation_id = ?1 ORDER BY position",
            )?;
            let rows = messages.query_map(params![id], |row| {
                Ok(WorkMessage {
                    role: WorkMessageRole::parse(&row.get::<_, String>(0)?),
                    text: row.get(1)?,
                    reasoning: row.get(2)?,
                    reasoning_complete: true,
                    reasoning_expanded: false,
                    tool_activities: Vec::new(),
                    expanded_tool_groups: Vec::new(),
                })
            })?;
            Ok(super::ConversationDocument {
                id,
                context_summary,
                messages: rows.collect::<rusqlite::Result<Vec<_>>>()?,
            })
        })
        .collect()
}

pub(super) fn replace_embeddings(
    transaction: &Transaction<'_>,
    conversation_id: &str,
    connection_id: &str,
    model_id: &str,
    fragments: &[ConversationFragment],
    embeddings: &[Vec<f32>],
) -> Result<(), WorkDatabaseError> {
    if fragments.len() != embeddings.len() {
        return Err(WorkDatabaseError::Index(
            "embedding count does not match fragments".into(),
        ));
    }
    transaction.execute(
        "DELETE FROM conversation_embeddings WHERE conversation_id = ?1",
        params![conversation_id],
    )?;
    let mut statement = transaction.prepare(
        "INSERT INTO conversation_embeddings
            (conversation_id, message_position, chunk_index, text, content_hash, connection_id, model_id, embedding, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    let timestamp = super::now();
    for (fragment, embedding) in fragments.iter().zip(embeddings) {
        if embedding.is_empty() {
            return Err(WorkDatabaseError::Index(
                "provider returned an empty vector".into(),
            ));
        }
        statement.execute(params![
            conversation_id,
            fragment.message_position as i64,
            fragment.chunk_index as i64,
            fragment.text,
            fragment.content_hash,
            connection_id,
            model_id,
            encode_embedding(embedding),
            timestamp,
        ])?;
    }
    Ok(())
}

pub(super) fn load_fragments(
    connection: &Connection,
    connection_id: &str,
    model_id: &str,
) -> Result<Vec<IndexedConversationFragment>, WorkDatabaseError> {
    let mut statement = connection.prepare(
        "SELECT e.conversation_id, e.message_position, e.chunk_index,
                c.title, c.project_id, c.updated_at,
                e.text, e.content_hash, e.connection_id, e.embedding
         FROM conversation_embeddings e
         JOIN conversations c ON c.id = e.conversation_id
         WHERE e.connection_id = ?1 AND e.model_id = ?2
         ORDER BY c.updated_at DESC, e.message_position, e.chunk_index",
    )?;
    let rows = statement.query_map(params![connection_id, model_id], |row| {
        Ok(IndexedConversationFragment {
            conversation_id: row.get(0)?,
            message_position: row.get::<_, i64>(1)? as usize,
            chunk_index: row.get::<_, i64>(2)? as usize,
            title: row.get(3)?,
            project_id: row.get(4)?,
            updated_at: row.get(5)?,
            text: row.get(6)?,
            content_hash: row.get(7)?,
            connection_id: super::ConnectionId(row.get(8)?),
            embedding: row.get(9)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

const VECTOR_TABLE: &str = "conversation_vectors";

pub(super) fn rebuild_vector_table(
    connection: &mut Connection,
    config: &EmbeddingConfig,
) -> Result<usize, WorkDatabaseError> {
    let fragments = load_fragments(connection, &config.connection_id.0, &config.model_id)?;
    connection.execute_batch(&format!("DROP TABLE IF EXISTS \"{VECTOR_TABLE}\";"))?;
    if fragments.is_empty() {
        return Ok(0);
    }

    let dimension = fragments
        .iter()
        .find_map(|fragment| decode_embedding(&fragment.embedding).map(|vector| vector.len()))
        .filter(|dimension| *dimension > 0)
        .ok_or_else(|| WorkDatabaseError::Index("conversation vectors are invalid".into()))?;
    let create_sql = format!(
        "CREATE VIRTUAL TABLE \"{VECTOR_TABLE}\" USING vector(\
            dim={dimension}, type=float4, metric=cosine,\
            m=16, ef_construction=200, ef_search=64, sync_every=128,\
            metadata=\"conversation_id TEXT, message_position INTEGER, chunk_index INTEGER,\
                      content_hash TEXT, connection_id TEXT, model_id TEXT\"\
        );"
    );
    connection.execute_batch(&create_sql)?;

    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare(&format!(
            "INSERT INTO \"{VECTOR_TABLE}\"\
             (vector, conversation_id, message_position, chunk_index, content_hash, connection_id, model_id)\
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        ))?;
        for fragment in &fragments {
            let vector = decode_embedding(&fragment.embedding).ok_or_else(|| {
                WorkDatabaseError::Index("conversation vector has an invalid blob".into())
            })?;
            if vector.len() != dimension {
                return Err(WorkDatabaseError::Index(
                    "conversation vectors have inconsistent dimensions".into(),
                ));
            }
            statement.execute(params![
                fragment.embedding.as_slice(),
                &fragment.conversation_id,
                fragment.message_position as i64,
                fragment.chunk_index as i64,
                &fragment.content_hash,
                &fragment.connection_id.0,
                &config.model_id,
            ])?;
        }
    }
    transaction.commit()?;
    // Persist the HNSW graph in sqlite-vector-rs' shadow table so it remains
    // available after the next application restart.
    connection.query_row(
        &format!("SELECT vector_sync_index('{VECTOR_TABLE}')"),
        [],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(fragments.len())
}

pub(super) fn vector_search(
    connection: &Connection,
    config: &EmbeddingConfig,
    query: &[f32],
    limit: usize,
) -> Result<Vec<VectorSearchHit>, WorkDatabaseError> {
    let table_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![VECTOR_TABLE],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    if !table_exists {
        return Ok(Vec::new());
    }

    let query_json = serde_json::to_string(query)?;
    let mut statement = connection.prepare(&format!(
        "SELECT v.conversation_id, c.title, c.project_id, c.updated_at, e.text, v.distance
         FROM \"{VECTOR_TABLE}\" v
         JOIN conversations c ON c.id = v.conversation_id
         JOIN conversation_embeddings e
           ON e.conversation_id = v.conversation_id
          AND e.content_hash = v.content_hash
          AND e.connection_id = v.connection_id
          AND e.model_id = v.model_id
         WHERE knn_match(v.distance, vector_from_json(?1, 'float4'))
           AND v.connection_id = ?2
           AND v.model_id = ?3
         ORDER BY v.distance ASC
         LIMIT ?4"
    ))?;
    let rows = statement.query_map(
        params![
            query_json,
            &config.connection_id.0,
            &config.model_id,
            limit as i64
        ],
        |row| {
            Ok(VectorSearchHit {
                conversation_id: row.get(0)?,
                title: row.get(1)?,
                project_id: row.get(2)?,
                updated_at: row.get(3)?,
                text: row.get(4)?,
                distance: row.get::<_, f64>(5)? as f32,
            })
        },
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn text_search(
    connection: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<ConversationSearchResult>, WorkDatabaseError> {
    let pattern = format!("%{}%", escape_like_pattern(query.trim()));
    let mut statement = connection.prepare(
        "SELECT c.id, c.title, c.project_id, c.updated_at,
                COALESCE(
                    (SELECT substr(e.text, 1, 220) FROM conversation_embeddings e
                     WHERE e.conversation_id = c.id
                       AND e.text LIKE ?1 COLLATE NOCASE ESCAPE '\\'
                     ORDER BY e.message_position, e.chunk_index LIMIT 1),
                    (SELECT substr(m.text, 1, 220) FROM messages m
                     WHERE m.conversation_id = c.id
                       AND m.text LIKE ?1 COLLATE NOCASE ESCAPE '\\'
                     ORDER BY m.position LIMIT 1),
                    c.title
                )
         FROM conversations c
         WHERE c.title LIKE ?1 COLLATE NOCASE ESCAPE '\\'
            OR EXISTS (SELECT 1 FROM messages m
                      WHERE m.conversation_id = c.id
                        AND m.text LIKE ?1 COLLATE NOCASE ESCAPE '\\')
            OR EXISTS (SELECT 1 FROM conversation_embeddings e
                      WHERE e.conversation_id = c.id
                        AND e.text LIKE ?1 COLLATE NOCASE ESCAPE '\\')
         ORDER BY c.updated_at DESC,
                  COALESCE((SELECT MAX(m.id) FROM messages m
                            WHERE m.conversation_id = c.id), 0) DESC,
                  c.id
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![pattern, limit as i64], |row| {
        Ok(ConversationSearchResult {
            conversation_id: row.get(0)?,
            title: row.get(1)?,
            project_id: row.get(2)?,
            updated_at: row.get(3)?,
            snippet: row.get(4)?,
            score: 0,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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

pub(super) fn status(
    connection: &Connection,
    config: Option<EmbeddingConfig>,
) -> Result<EmbeddingIndexStatus, WorkDatabaseError> {
    let total_conversations =
        connection.query_row("SELECT COUNT(*) FROM conversations", [], |row| {
            row.get::<_, i64>(0)
        })? as usize;
    let (indexed_conversations, indexed_fragments) = match config.as_ref() {
        Some(config) => (
            connection.query_row(
                "SELECT COUNT(DISTINCT conversation_id) FROM conversation_embeddings
                 WHERE connection_id = ?1 AND model_id = ?2",
                params![&config.connection_id.0, config.model_id],
                |row| row.get::<_, i64>(0),
            )? as usize,
            connection.query_row(
                "SELECT COUNT(*) FROM conversation_embeddings
                 WHERE connection_id = ?1 AND model_id = ?2",
                params![&config.connection_id.0, config.model_id],
                |row| row.get::<_, i64>(0),
            )? as usize,
        ),
        None => (0, 0),
    };
    Ok(EmbeddingIndexStatus {
        config,
        total_conversations,
        indexed_conversations,
        indexed_fragments,
    })
}

pub(super) fn embedding_config(
    connection: &Connection,
    key: &str,
) -> Result<Option<EmbeddingConfig>, WorkDatabaseError> {
    let value = connection
        .query_row(
            "SELECT value FROM preferences WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    value
        .map(|value| serde_json::from_str(&value).map_err(WorkDatabaseError::from))
        .transpose()
}

pub(super) fn save_embedding_config(
    connection: &Connection,
    key: &str,
    config: &EmbeddingConfig,
) -> Result<(), WorkDatabaseError> {
    let value = serde_json::to_string(config)?;
    connection.execute(
        "INSERT INTO preferences (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, super::now()],
    )?;
    Ok(())
}
