use std::fs;

use orca::config::{Config, WorkerConfig};

#[test]
fn test_default_config() {
    let config = Config::default();

    assert_eq!(config.daemon.socket_path, ".orca/orca.sock");
    assert_eq!(config.daemon.max_workers, 4);
    assert_eq!(config.daemon.log_level, "info");

    assert_eq!(config.terminal.provider, "manual");
    assert_eq!(config.terminal.layout, "tabs");
    assert!(config.terminal.auto_open);

    assert!(config.worker.codex.is_none());

    assert_eq!(config.isolation.worktree_dir, ".agents/worktree");
    assert_eq!(config.isolation.default_strategy, "auto");
    assert_eq!(config.isolation.target_branch, "main");

    assert_eq!(
        config.escalation.auto_approve,
        vec!["implementation_choice"]
    );
    assert_eq!(
        config.escalation.always_user,
        vec![
            "architecture_change",
            "destructive_operation",
            "scope_exceeded"
        ]
    );
    assert_eq!(
        config.escalation.cc_first,
        vec!["test_failure", "timeout", "conflict"]
    );
    assert_eq!(config.escalation.worker_timeout_secs, 300);
    assert_eq!(config.escalation.max_retries, 2);

    assert!(config.notification.terminal_bell);
    assert!(config.notification.system_notification);
}

#[test]
fn test_load_project_config() {
    let dir = tempfile::tempdir().unwrap();
    let toml_content = r#"
[daemon]
max_workers = 8
log_level = "debug"

[isolation]
target_branch = "develop"

[worker.codex]
command = "codex-rs"
args = ["--sandbox"]
timeout_secs = 600
max_retries = 5
"#;
    fs::write(dir.path().join("orca.toml"), toml_content).unwrap();

    let config = Config::load(dir.path()).unwrap();

    assert_eq!(config.daemon.max_workers, 8);
    assert_eq!(config.daemon.log_level, "debug");
    assert_eq!(config.isolation.target_branch, "develop");

    let codex = config.worker.codex.expect("codex config should be set");
    assert_eq!(codex.command, "codex-rs");
    assert_eq!(codex.args, vec!["--sandbox"]);
    assert_eq!(codex.timeout_secs, 600);
    assert_eq!(codex.max_retries, 5);
}

#[test]
fn test_no_config_uses_defaults() {
    let dir = tempfile::tempdir().unwrap();
    // No orca.toml written — should fall back to defaults.
    let config = Config::load(dir.path()).unwrap();

    assert_eq!(config.daemon.max_workers, 4);
    assert_eq!(config.daemon.log_level, "info");
    assert_eq!(config.terminal.provider, "manual");
    assert!(config.terminal.auto_open);
    assert_eq!(config.isolation.worktree_dir, ".agents/worktree");
}

#[test]
fn test_socket_path_resolution() {
    let config = Config::default();
    let project = std::path::Path::new("/home/user/project");

    // Relative path is joined to project dir.
    let resolved = config.socket_path(project);
    assert_eq!(resolved, project.join(".orca/orca.sock"));

    // Absolute path is returned as-is.
    let mut abs_config = Config::default();
    abs_config.daemon.socket_path = "/tmp/orca.sock".to_string();
    let resolved = abs_config.socket_path(project);
    assert_eq!(resolved, std::path::PathBuf::from("/tmp/orca.sock"));
}

#[test]
fn test_toml_roundtrip() {
    let original = Config::default();
    let serialized = toml::to_string_pretty(&original).expect("serialize");
    let deserialized: Config = toml::from_str(&serialized).expect("deserialize");

    assert_eq!(original.daemon.socket_path, deserialized.daemon.socket_path);
    assert_eq!(original.daemon.max_workers, deserialized.daemon.max_workers);
    assert_eq!(original.daemon.log_level, deserialized.daemon.log_level);
    assert_eq!(original.terminal.provider, deserialized.terminal.provider);
    assert_eq!(original.terminal.layout, deserialized.terminal.layout);
    assert_eq!(original.terminal.auto_open, deserialized.terminal.auto_open);
    assert_eq!(
        original.isolation.worktree_dir,
        deserialized.isolation.worktree_dir
    );
    assert_eq!(
        original.isolation.default_strategy,
        deserialized.isolation.default_strategy
    );
    assert_eq!(
        original.isolation.target_branch,
        deserialized.isolation.target_branch
    );
    assert_eq!(
        original.escalation.auto_approve,
        deserialized.escalation.auto_approve
    );
    assert_eq!(
        original.escalation.worker_timeout_secs,
        deserialized.escalation.worker_timeout_secs
    );
    assert_eq!(
        original.notification.terminal_bell,
        deserialized.notification.terminal_bell
    );
    assert_eq!(
        original.notification.system_notification,
        deserialized.notification.system_notification
    );
}

#[test]
fn test_codex_worker_config_defaults() {
    let config = Config::default();
    let codex = config.codex_worker_config();

    assert_eq!(codex.command, "codex");
    assert_eq!(codex.args, vec!["--full-auto"]);
    assert_eq!(codex.timeout_secs, 300);
    assert_eq!(codex.max_retries, 2);
}

#[test]
fn test_codex_worker_config_from_file() {
    let mut config = Config::default();
    config.worker.codex = Some(WorkerConfig {
        command: "my-codex".to_string(),
        args: vec!["--fast".to_string()],
        timeout_secs: 60,
        max_retries: 0,
    });

    let codex = config.codex_worker_config();
    assert_eq!(codex.command, "my-codex");
    assert_eq!(codex.args, vec!["--fast"]);
    assert_eq!(codex.timeout_secs, 60);
    assert_eq!(codex.max_retries, 0);
}

#[test]
fn test_worktree_dir_resolution() {
    let config = Config::default();
    let project = std::path::Path::new("/home/user/project");

    let resolved = config.worktree_dir(project);
    assert_eq!(resolved, project.join(".agents/worktree"));

    let mut abs_config = Config::default();
    abs_config.isolation.worktree_dir = "/var/worktrees".to_string();
    let resolved = abs_config.worktree_dir(project);
    assert_eq!(resolved, std::path::PathBuf::from("/var/worktrees"));
}
