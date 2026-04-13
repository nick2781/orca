use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::escalation::EscalationRequest;
use crate::types::{Task, WorkerInfo};

/// Persistent daemon state containing tasks, workers, and escalations.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DaemonState {
    pub tasks: HashMap<String, Task>,
    pub workers: HashMap<String, WorkerInfo>,
    pub escalations: HashMap<String, EscalationRequest>,
    pub active_plan_id: Option<String>,
}

/// A single entry in the append-only ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub timestamp: String,
    pub event: String,
    pub data: serde_json::Value,
}

/// File-backed state store with an append-only event ledger.
pub struct StateStore {
    state_path: PathBuf,
    ledger_path: PathBuf,
    state: DaemonState,
}

impl StateStore {
    /// Create a new StateStore, loading existing state from disk or starting fresh.
    pub fn new(orca_dir: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(orca_dir)?;

        let state_path = orca_dir.join("state.json");
        let ledger_path = orca_dir.join("ledger.jsonl");

        let state = if state_path.exists() {
            let contents = fs::read_to_string(&state_path)?;
            serde_json::from_str(&contents)?
        } else {
            DaemonState::default()
        };

        Ok(Self {
            state_path,
            ledger_path,
            state,
        })
    }

    /// Return an immutable reference to the current state.
    pub fn state(&self) -> &DaemonState {
        &self.state
    }

    /// Return a mutable reference to the current state.
    pub fn state_mut(&mut self) -> &mut DaemonState {
        &mut self.state
    }

    /// Persist the current state to state.json.
    pub fn save(&self) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(&self.state)?;
        fs::write(&self.state_path, json)?;
        Ok(())
    }

    /// Append an event to the ledger.jsonl file.
    pub fn log_event(
        &self,
        event: impl Into<String>,
        data: serde_json::Value,
    ) -> anyhow::Result<()> {
        let entry = LedgerEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event: event.into(),
            data,
        };
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.ledger_path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    }

    // -- Task CRUD --

    /// Add a task to the store, keyed by its spec id.
    pub fn add_task(&mut self, task: Task) {
        self.state.tasks.insert(task.spec.id.clone(), task);
    }

    /// Get an immutable reference to a task by id.
    pub fn get_task(&self, id: &str) -> Option<&Task> {
        self.state.tasks.get(id)
    }

    /// Get a mutable reference to a task by id.
    pub fn get_task_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.state.tasks.get_mut(id)
    }

    /// Return all tasks as a slice of (id, task) pairs.
    pub fn all_tasks(&self) -> Vec<(&String, &Task)> {
        self.state.tasks.iter().collect()
    }

    // -- Worker CRUD --

    /// Register a worker in the store.
    pub fn register_worker(&mut self, worker: WorkerInfo) {
        self.state.workers.insert(worker.id.clone(), worker);
    }

    /// Get an immutable reference to a worker by id.
    pub fn get_worker(&self, id: &str) -> Option<&WorkerInfo> {
        self.state.workers.get(id)
    }

    /// Get a mutable reference to a worker by id.
    pub fn get_worker_mut(&mut self, id: &str) -> Option<&mut WorkerInfo> {
        self.state.workers.get_mut(id)
    }

    // -- Escalation CRUD --

    /// Add an escalation to the store.
    pub fn add_escalation(&mut self, escalation: EscalationRequest) {
        self.state
            .escalations
            .insert(escalation.id.clone(), escalation);
    }

    /// Remove and return an escalation by id.
    pub fn remove_escalation(&mut self, id: &str) -> Option<EscalationRequest> {
        self.state.escalations.remove(id)
    }

    /// Return all pending escalations.
    pub fn pending_escalations(&self) -> Vec<&EscalationRequest> {
        self.state.escalations.values().collect()
    }
}
