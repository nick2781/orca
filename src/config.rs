use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level configuration for Orca.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub terminal: TerminalConfig,
    pub worker: WorkerConfigs,
    pub isolation: IsolationConfig,
    pub escalation: EscalationConfig,
    pub notification: NotificationConfig,
}

/// Daemon process settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub socket_path: String,
    pub max_workers: usize,
    pub log_level: String,
}

/// Terminal multiplexer settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    pub provider: String,
    pub layout: String,
    pub auto_open: bool,
}

/// Container for per-worker-type configurations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerConfigs {
    pub codex: Option<WorkerConfig>,
}

/// Configuration for a single worker type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub timeout_secs: u64,
    pub max_retries: u32,
}

/// Git worktree and branch isolation settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IsolationConfig {
    pub worktree_dir: String,
    pub default_strategy: String,
    pub target_branch: String,
}

/// Escalation routing and retry settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EscalationConfig {
    pub auto_approve: Vec<String>,
    pub always_user: Vec<String>,
    pub cc_first: Vec<String>,
    pub worker_timeout_secs: u64,
    pub max_retries: u32,
}

/// Desktop/terminal notification settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationConfig {
    pub terminal_bell: bool,
    pub system_notification: bool,
}

// --- Default implementations ---

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: ".orca/orca.sock".to_string(),
            max_workers: 4,
            log_level: "info".to_string(),
        }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            provider: "manual".to_string(),
            layout: "tabs".to_string(),
            auto_open: true,
        }
    }
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            command: "codex".to_string(),
            args: vec!["--full-auto".to_string()],
            timeout_secs: 300,
            max_retries: 2,
        }
    }
}

impl Default for IsolationConfig {
    fn default() -> Self {
        Self {
            worktree_dir: ".agents/worktree".to_string(),
            default_strategy: "auto".to_string(),
            target_branch: "main".to_string(),
        }
    }
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            auto_approve: vec!["implementation_choice".to_string()],
            always_user: vec![
                "architecture_change".to_string(),
                "destructive_operation".to_string(),
                "scope_exceeded".to_string(),
            ],
            cc_first: vec![
                "test_failure".to_string(),
                "timeout".to_string(),
                "conflict".to_string(),
            ],
            worker_timeout_secs: 300,
            max_retries: 2,
        }
    }
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            terminal_bell: true,
            system_notification: true,
        }
    }
}

// --- Config loading and helpers ---

impl Config {
    /// Load configuration by merging global (~/.orca/config.toml) and
    /// project-level (orca.toml) files. Missing files are silently ignored
    /// and defaults are used instead.
    pub fn load(project_dir: &Path) -> Result<Self> {
        let mut config = Config::default();

        // Layer 1: global config
        if let Some(home) = dirs::home_dir() {
            let global_path = home.join(".orca").join("config.toml");
            if global_path.exists() {
                let contents = std::fs::read_to_string(&global_path)
                    .with_context(|| format!("reading {}", global_path.display()))?;
                config = toml::from_str(&contents)
                    .with_context(|| format!("parsing {}", global_path.display()))?;
            }
        }

        // Layer 2: project config (overrides global)
        let project_path = project_dir.join("orca.toml");
        if project_path.exists() {
            let contents = std::fs::read_to_string(&project_path)
                .with_context(|| format!("reading {}", project_path.display()))?;
            let project_config: Config = toml::from_str(&contents)
                .with_context(|| format!("parsing {}", project_path.display()))?;
            config = config.merge(project_config);
        }

        Ok(config)
    }

    /// Resolve the daemon socket path relative to the project directory.
    pub fn socket_path(&self, project_dir: &Path) -> PathBuf {
        let p = Path::new(&self.daemon.socket_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            project_dir.join(p)
        }
    }

    /// Resolve the worktree directory relative to the project directory.
    pub fn worktree_dir(&self, project_dir: &Path) -> PathBuf {
        let p = Path::new(&self.isolation.worktree_dir);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            project_dir.join(p)
        }
    }

    /// Return the Codex worker configuration, falling back to defaults.
    pub fn codex_worker_config(&self) -> WorkerConfig {
        self.worker.codex.clone().unwrap_or_default()
    }

    /// Merge another config on top of self. The `other` config's explicitly
    /// set values take precedence. Because TOML deserialization fills in
    /// defaults for missing sections, we treat the project config as a
    /// full overlay.
    fn merge(self, other: Config) -> Config {
        // Project-level config wins entirely when present.
        // Since serde fills defaults for missing sections we do a
        // field-level merge where project values override global.
        Config {
            daemon: DaemonConfig {
                socket_path: other.daemon.socket_path,
                max_workers: other.daemon.max_workers,
                log_level: other.daemon.log_level,
            },
            terminal: other.terminal,
            worker: WorkerConfigs {
                codex: other.worker.codex.or(self.worker.codex),
            },
            isolation: other.isolation,
            escalation: other.escalation,
            notification: other.notification,
        }
    }
}
