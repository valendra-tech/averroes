use rusqlite::Connection;
use std::path::PathBuf;

pub struct SessionStore {
    db: Connection,
}

impl SessionStore {
    pub fn open() -> anyhow::Result<Self> {
        let db_path = data_dir().join("sessions.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Connection::open(&db_path)?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );",
        )?;
        Ok(Self { db })
    }

    pub fn create_session(&self, name: &str) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        self.db.execute(
            "INSERT INTO sessions (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, name, now, now],
        )?;
        Ok(id)
    }

    pub fn save_message(&self, session_id: &str, role: &str, content: &str) -> anyhow::Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.db.execute(
            "INSERT INTO messages (session_id, role, content, timestamp) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, role, content, now],
        )?;
        self.db.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now, session_id],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<(String, String)>> {
        let mut stmt = self.db.prepare("SELECT id, name FROM sessions ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }
}

fn data_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("averroes")
}
