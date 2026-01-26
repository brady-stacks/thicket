use rusqlite::{Connection, Result as SqlResult};
use serde_json::Value;
use std::sync::Mutex;

pub struct Cache {
    conn: Mutex<Connection>,
}

impl Cache {
    pub fn new() -> SqlResult<Self> {
        Self::with_path("thicket_cache.db")
    }

    pub fn with_path(path: &str) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        let cache = Cache {
            conn: Mutex::new(conn),
        };
        cache.init()?;
        Ok(cache)
    }

    fn init(&self) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS contract_cache (
                url TEXT PRIMARY KEY,
                source_code TEXT NOT NULL,
                cost_map TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )",
            [],
        )?;
        Ok(())
    }

    pub fn get(&self, url: &str) -> SqlResult<Option<(String, Value)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT source_code, cost_map FROM contract_cache WHERE url = ?1"
        )?;
        
        let mut rows = stmt.query_map([url], |row| {
            let source_code: String = row.get(0)?;
            let cost_map_str: String = row.get(1)?;
            let cost_map: Value = serde_json::from_str(&cost_map_str)
                .map_err(|_| rusqlite::Error::InvalidColumnType(2, "TEXT".to_string(), rusqlite::types::Type::Text))?;
            Ok((source_code, cost_map))
        })?;

        if let Some(row) = rows.next() {
            row.map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn set(&self, url: &str, source_code: &str, cost_map: &Value) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        let cost_map_str = serde_json::to_string(cost_map)
            .map_err(|_| rusqlite::Error::InvalidColumnType(0, "TEXT".to_string(), rusqlite::types::Type::Text))?;
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        conn.execute(
            "INSERT OR REPLACE INTO contract_cache (url, source_code, cost_map, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![url, source_code, cost_map_str, created_at],
        )?;
        Ok(())
    }

    pub fn get_recent_urls(&self, limit: i32) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT url FROM contract_cache WHERE url LIKE 'http%' ORDER BY created_at DESC LIMIT ?1"
        )?;
        
        let rows = stmt.query_map([limit], |row| {
            let url: String = row.get(0)?;
            Ok(url)
        })?;

        let mut urls = Vec::new();
        for row in rows {
            urls.push(row?);
        }
        Ok(urls)
    }

    pub fn get_recent_sources(&self, limit: i32) -> SqlResult<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT url, source_code FROM contract_cache WHERE url NOT LIKE 'http%' ORDER BY created_at DESC LIMIT ?1"
        )?;
        
        let rows = stmt.query_map([limit], |row| {
            let url: String = row.get(0)?;
            let source_code: String = row.get(1)?;
            Ok((url, source_code))
        })?;

        let mut sources = Vec::new();
        for row in rows {
            sources.push(row?);
        }
        Ok(sources)
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self::new().expect("Failed to initialize cache")
    }
}
