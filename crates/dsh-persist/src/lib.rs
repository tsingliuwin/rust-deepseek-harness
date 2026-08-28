//! dsh-persist — JSONL session persistence.
//!
//! Mirrors the reference's append-only session store: one JSONL file per
//! session (`{sessions_dir}/{id}.jsonl`), one serialized `SessionEvent` per
//! line. The model-visible history is always re-derived from these events,
//! never stored separately.

use dsh_llm::SessionId;
use dsh_session::{Session, SessionEvent};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::SystemTime;

pub struct SessionRecorder {
    sessions_dir: PathBuf,
}

impl SessionRecorder {
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    /// The JSONL path for one session id.
    pub fn path_for(&self, id: &SessionId) -> PathBuf {
        self.sessions_dir.join(format!("{}.jsonl", id.as_str()))
    }

    /// Append one event to the session's JSONL file (creating it as needed).
    pub fn append(&self, id: &SessionId, event: &SessionEvent) -> io::Result<()> {
        fs::create_dir_all(&self.sessions_dir)?;
        let mut file = OpenOptions::new().create(true).append(true).open(self.path_for(id))?;
        let line = serde_json::to_string(event)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Load a session by rebuilding it from its JSONL log.
    pub fn load(&self, id: &SessionId) -> io::Result<Session> {
        let path = self.path_for(id);
        let file = File::open(&path)?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<SessionEvent>(line) {
                events.push(event);
            }
        }
        Ok(Session::from_events(id.clone(), events))
    }

    /// List known sessions, most recently modified first.
    pub fn list(&self) -> io::Result<Vec<SessionId>> {
        if !self.sessions_dir.exists() {
            return Ok(Vec::new());
        }
        let mut metas: Vec<(SystemTime, String)> = Vec::new();
        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".jsonl") {
                let modified = entry.metadata()?.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                metas.push((modified, stem.to_string()));
            }
        }
        metas.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(metas.into_iter().map(|(_, id)| SessionId::new(id)).collect())
    }
}