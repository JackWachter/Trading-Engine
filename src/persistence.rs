use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::{EngineState, MatchingEngine};
use crate::events::Event;
use crate::types::EngineCommand;

#[derive(Debug, Clone)]
pub struct EngineStore {
    root: PathBuf,
    log_path: PathBuf,
    snapshot_path: PathBuf,
}

#[derive(Debug)]
pub struct PersistentMatchingEngine {
    engine: MatchingEngine,
    store: EngineStore,
    last_log_index: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommandRecord {
    index: u64,
    command: EngineCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotRecord {
    last_log_index: u64,
    state: EngineState,
}

impl EngineStore {
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self {
            log_path: root.join("engine.log"),
            snapshot_path: root.join("engine.snapshot.json"),
            root,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn append(&self, index: u64, command: &EngineCommand) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;
        let record = CommandRecord {
            index,
            command: *command,
        };
        serde_json::to_writer(&mut file, &record)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    pub fn snapshot(&self, last_log_index: u64, engine: &MatchingEngine) -> io::Result<()> {
        let temp_path = self.root.join("engine.snapshot.tmp");
        let mut file = File::create(&temp_path)?;
        let snapshot = SnapshotRecord {
            last_log_index,
            state: engine.export_state(),
        };
        serde_json::to_writer_pretty(&mut file, &snapshot)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        file.flush()?;
        fs::rename(temp_path, &self.snapshot_path)?;
        Ok(())
    }

    pub fn load_engine(&self) -> io::Result<(MatchingEngine, u64)> {
        let mut engine = MatchingEngine::new();
        let mut last_log_index = 0;

        if self.snapshot_path.exists() {
            let file = File::open(&self.snapshot_path)?;
            let snapshot: SnapshotRecord = serde_json::from_reader(file)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            engine = MatchingEngine::from_state(snapshot.state);
            last_log_index = snapshot.last_log_index;
        }

        if self.log_path.exists() {
            let file = File::open(&self.log_path)?;
            for line in BufReader::new(file).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let record: CommandRecord = serde_json::from_str(&line)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                if record.index > last_log_index {
                    let _ = engine.apply(record.command);
                    last_log_index = record.index;
                }
            }
        }

        Ok((engine, last_log_index))
    }
}

impl PersistentMatchingEngine {
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let store = EngineStore::new(root)?;
        let (engine, last_log_index) = store.load_engine()?;
        Ok(Self {
            engine,
            store,
            last_log_index,
        })
    }

    pub fn apply(&mut self, command: EngineCommand) -> io::Result<Vec<Event>> {
        self.last_log_index += 1;
        self.store.append(self.last_log_index, &command)?;
        Ok(self.engine.apply(command))
    }

    pub fn snapshot(&mut self) -> io::Result<()> {
        self.store.snapshot(self.last_log_index, &self.engine)
    }

    pub fn engine(&self) -> &MatchingEngine {
        &self.engine
    }
}
