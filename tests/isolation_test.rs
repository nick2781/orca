use std::path::PathBuf;

use orca::isolation::{IsolationDecision, IsolationManager};
use orca::types::{IsolationMode, TaskContext, TaskSpec};

fn make_task(id: &str, files: Vec<&str>, mode: IsolationMode) -> TaskSpec {
    TaskSpec {
        id: id.to_string(),
        title: format!("Task {}", id),
        description: String::new(),
        context: TaskContext {
            files: files.into_iter().map(String::from).collect(),
            references: Vec::new(),
            constraints: String::new(),
        },
        isolation: mode,
        depends_on: Vec::new(),
        priority: 0,
    }
}

fn make_manager() -> IsolationManager {
    IsolationManager::new(
        &PathBuf::from("/repo"),
        &PathBuf::from("/repo/.agent/worktree"),
    )
}

#[test]
fn test_auto_no_overlap_uses_worktree() {
    let mgr = make_manager();
    let task = make_task("t1", vec!["src/a.rs"], IsolationMode::Auto);
    let running = make_task("t2", vec!["src/b.rs"], IsolationMode::Auto);

    let decision = mgr.decide(&task, &[&running]);

    assert_eq!(
        decision,
        IsolationDecision::Worktree {
            path: PathBuf::from("/repo/.agent/worktree/task-t1"),
            branch: "orca/task-t1".to_string(),
        }
    );
}

#[test]
fn test_auto_with_overlap_uses_serial() {
    let mgr = make_manager();
    let task = make_task("t1", vec!["src/shared.rs", "src/a.rs"], IsolationMode::Auto);
    let running = make_task("t2", vec!["src/shared.rs", "src/b.rs"], IsolationMode::Auto);

    let decision = mgr.decide(&task, &[&running]);

    assert_eq!(
        decision,
        IsolationDecision::Serial {
            wait_for: "t2".to_string(),
        }
    );
}

#[test]
fn test_auto_no_files_asks_cc() {
    let mgr = make_manager();
    let task = make_task("t1", vec![], IsolationMode::Auto);
    let running = make_task("t2", vec!["src/b.rs"], IsolationMode::Auto);

    let decision = mgr.decide(&task, &[&running]);

    assert_eq!(decision, IsolationDecision::AskCc);
}

#[test]
fn test_explicit_worktree_mode() {
    let mgr = make_manager();
    let task = make_task("feature-x", vec!["src/main.rs"], IsolationMode::Worktree);

    let decision = mgr.decide(&task, &[]);

    assert_eq!(
        decision,
        IsolationDecision::Worktree {
            path: PathBuf::from("/repo/.agent/worktree/task-feature-x"),
            branch: "orca/task-feature-x".to_string(),
        }
    );
}
