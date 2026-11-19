//! Application state and the SQLite store (replaces PostgreSQL + Redis).

use crate::models::*;
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

pub struct AppState {
    pub db: Mutex<Connection>,
    pub data_dir: PathBuf,
    pub res_dir: PathBuf,
    pub http: reqwest::Client,
    pub cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// One heavy pipeline at a time; renders inside it run in parallel.
    pub task_sem: Arc<Semaphore>,
    pub encoder_cache: Mutex<Option<String>>,
    pub auth_status: Mutex<HashMap<String, String>>,
    pub publishing: Arc<Semaphore>,
}

pub type State = Arc<AppState>;

/// Stored encrypted (Windows DPAPI).
const SECRET_KEYS: [&str; 6] = ["settings", "publish_vault", "chatgpt_auth", "remote_token", "remote_sessions", "remote_pair"];

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl AppState {
    pub fn open(data_dir: PathBuf, res_dir: PathBuf) -> Result<Self> {
        for sub in ["sources", "clips", "work", "tools", "fonts", "cache/broll", "models"] {
            std::fs::create_dir_all(data_dir.join(sub))?;
        }
        let db_path = data_dir.join("nexora-clip.db");
        if !db_path.exists() {
            // A database from an earlier build used another file name: take it over.
            let older = std::fs::read_dir(&data_dir).into_iter().flatten().flatten().map(|e| e.path()).find(|p| p.extension().map(|x| x == "db").unwrap_or(false));
            if let Some(old) = older {
                for suffix in ["", "-wal", "-shm"] {
                    let from = PathBuf::from(format!("{}{suffix}", old.to_string_lossy()));
                    let to = PathBuf::from(format!("{}{suffix}", db_path.to_string_lossy()));
                    if from.exists() {
                        let _ = std::fs::rename(from, to);
                    }
                }
            }
        }
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS tasks (
               id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '', source_type TEXT NOT NULL,
               source_url TEXT NOT NULL DEFAULT '', source_path TEXT NOT NULL DEFAULT '',
               status TEXT NOT NULL, progress INTEGER NOT NULL DEFAULT 0, message TEXT NOT NULL DEFAULT '',
               error TEXT NOT NULL DEFAULT '', workflow TEXT NOT NULL DEFAULT 'highlights',
               options TEXT NOT NULL DEFAULT '{}', summary TEXT NOT NULL DEFAULT '',
               key_topics TEXT NOT NULL DEFAULT '[]', analysis TEXT NOT NULL DEFAULT '',
               duration REAL NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS clips (
               id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
               idx INTEGER NOT NULL, doc TEXT NOT NULL, file_path TEXT NOT NULL DEFAULT '',
               thumb_path TEXT NOT NULL DEFAULT '', duration REAL NOT NULL DEFAULT 0,
               status TEXT NOT NULL DEFAULT 'ready', error TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS clips_task ON clips(task_id, idx);",
        )?;
        // Anything left mid-flight by a previous run can only be resumed by hand.
        conn.execute(
            "UPDATE tasks SET status='error', error='Interrupted when the app closed. Press Resume to continue.' WHERE status IN ('queued','processing')",
            [],
        )?;
        conn.execute("UPDATE clips SET status='error', error='Interrupted' WHERE status='rendering'", [])?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0);
        if version < 1 {
            conn.execute_batch("PRAGMA user_version = 1;")?;
        }
        let http = reqwest::Client::builder()
            .user_agent("NexoraClip/1.0")
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()?;
        Ok(Self {
            db: Mutex::new(conn),
            data_dir,
            res_dir,
            http,
            cancels: Mutex::new(HashMap::new()),
            task_sem: Arc::new(Semaphore::new(1)),
            encoder_cache: Mutex::new(None),
            auth_status: Mutex::new(HashMap::new()),
            publishing: Arc::new(Semaphore::new(1)),
        })
    }

    pub fn settings(&self) -> Settings {
        let db = self.db.lock().unwrap();
        drop(db);
        self.kv_get("settings").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        self.kv_set("settings", &serde_json::to_string(s)?)
    }

    pub fn kv_get(&self, key: &str) -> Option<String> {
        let db = self.db.lock().unwrap();
        let raw: Option<String> = db.query_row("SELECT value FROM kv WHERE key=?1", params![key], |r| r.get(0)).optional().ok().flatten();
        drop(db);
        let raw = raw?;
        if SECRET_KEYS.contains(&key) {
            return crate::secret::open(&raw);
        }
        Some(raw)
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        let stored = if SECRET_KEYS.contains(&key) { crate::secret::seal(value) } else { value.to_string() };
        let db = self.db.lock().unwrap();
        db.execute("INSERT INTO kv(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, stored])?;
        Ok(())
    }

    // ---- tasks ----

    pub fn insert_task(&self, t: &Task) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO tasks(id,title,source_type,source_url,source_path,status,progress,message,workflow,options,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,0,?7,?8,?9,?10,?10)",
            params![t.id, t.title, t.source_type, t.source_url, t.source_path, t.status, t.message, t.workflow,
                    serde_json::to_string(&t.options)?, t.created_at],
        )?;
        Ok(())
    }

    fn row_to_task(r: &rusqlite::Row) -> rusqlite::Result<Task> {
        let options: String = r.get("options")?;
        let topics: String = r.get("key_topics")?;
        Ok(Task {
            id: r.get("id")?,
            title: r.get("title")?,
            source_type: r.get("source_type")?,
            source_url: r.get("source_url")?,
            source_path: r.get("source_path")?,
            status: r.get("status")?,
            progress: r.get("progress")?,
            message: r.get("message")?,
            error: r.get("error")?,
            workflow: r.get("workflow")?,
            options: serde_json::from_str(&options).unwrap_or_default(),
            summary: r.get("summary")?,
            key_topics: serde_json::from_str(&topics).unwrap_or_default(),
            duration: r.get("duration")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
            clip_count: r.get("clip_count").unwrap_or(0),
            thumb: r.get::<_, Option<String>>("thumb").unwrap_or(None).unwrap_or_default(),
        })
    }

    const TASK_SELECT: &'static str = "SELECT t.*, (SELECT COUNT(*) FROM clips c WHERE c.task_id=t.id) AS clip_count,
        (SELECT thumb_path FROM clips c WHERE c.task_id=t.id AND thumb_path<>'' ORDER BY idx LIMIT 1) AS thumb FROM tasks t";

    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare(&format!("{} ORDER BY t.created_at DESC", Self::TASK_SELECT))?;
        let rows = st.query_map([], Self::row_to_task)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_task(&self, id: &str) -> Result<Task> {
        let db = self.db.lock().unwrap();
        db.query_row(&format!("{} WHERE t.id=?1", Self::TASK_SELECT), params![id], Self::row_to_task)
            .optional()?
            .ok_or_else(|| anyhow!("Task not found"))
    }

    pub fn set_task_fields(&self, id: &str, fields: &[(&str, &dyn rusqlite::ToSql)]) -> Result<()> {
        let db = self.db.lock().unwrap();
        let mut sql = String::from("UPDATE tasks SET updated_at=?1");
        let ts = now();
        let mut values: Vec<&dyn rusqlite::ToSql> = vec![&ts];
        for (i, (name, value)) in fields.iter().enumerate() {
            sql.push_str(&format!(", {}=?{}", name, i + 2));
            values.push(*value);
        }
        sql.push_str(&format!(" WHERE id=?{}", fields.len() + 2));
        values.push(&id);
        db.execute(&sql, values.as_slice())?;
        Ok(())
    }

    pub fn get_analysis(&self, id: &str) -> Option<Analysis> {
        let db = self.db.lock().unwrap();
        let raw: String = db.query_row("SELECT analysis FROM tasks WHERE id=?1", params![id], |r| r.get(0)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn delete_task(&self, id: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute("DELETE FROM clips WHERE task_id=?1", params![id])?;
        db.execute("DELETE FROM tasks WHERE id=?1", params![id])?;
        Ok(())
    }

    // ---- clips ----

    fn row_to_clip(r: &rusqlite::Row) -> rusqlite::Result<Clip> {
        let doc: String = r.get("doc")?;
        Ok(Clip {
            id: r.get("id")?,
            task_id: r.get("task_id")?,
            idx: r.get("idx")?,
            doc: serde_json::from_str(&doc).unwrap_or_default(),
            file_path: r.get("file_path")?,
            thumb_path: r.get("thumb_path")?,
            duration: r.get("duration")?,
            status: r.get("status")?,
            error: r.get("error")?,
            created_at: r.get("created_at")?,
        })
    }

    pub fn list_clips(&self, task_id: &str) -> Result<Vec<Clip>> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare("SELECT * FROM clips WHERE task_id=?1 ORDER BY idx, created_at")?;
        let rows = st.query_map(params![task_id], Self::row_to_clip)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_clip(&self, id: &str) -> Result<Clip> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT * FROM clips WHERE id=?1", params![id], Self::row_to_clip)
            .optional()?
            .ok_or_else(|| anyhow!("Clip not found"))
    }

    pub fn upsert_clip(&self, c: &Clip) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO clips(id,task_id,idx,doc,file_path,thumb_path,duration,status,error,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(id) DO UPDATE SET idx=excluded.idx, doc=excluded.doc, file_path=excluded.file_path,
               thumb_path=excluded.thumb_path, duration=excluded.duration, status=excluded.status, error=excluded.error",
            params![c.id, c.task_id, c.idx, serde_json::to_string(&c.doc)?, c.file_path, c.thumb_path, c.duration,
                    c.status, c.error, c.created_at],
        )?;
        Ok(())
    }

    pub fn delete_clip(&self, id: &str) -> Result<()> {
        let db = self.db.lock().unwrap();
        db.execute("DELETE FROM clips WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn next_clip_idx(&self, task_id: &str) -> i64 {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT COALESCE(MAX(idx),-1)+1 FROM clips WHERE task_id=?1", params![task_id], |r| r.get(0))
            .unwrap_or(0)
    }
}
