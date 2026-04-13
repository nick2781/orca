use std::collections::{HashMap, HashSet};

use orca::daemon::scheduler::{DependencyGraph, Scheduler};
use orca::types::*;

fn make_spec(id: &str) -> TaskSpec {
    TaskSpec {
        id: id.to_string(),
        title: id.to_string(),
        description: String::new(),
        context: TaskContext::default(),
        isolation: IsolationMode::Auto,
        depends_on: Vec::new(),
        priority: 0,
    }
}

fn make_spec_with_files(id: &str, files: Vec<&str>) -> TaskSpec {
    TaskSpec {
        id: id.to_string(),
        title: id.to_string(),
        description: String::new(),
        context: TaskContext {
            files: files.into_iter().map(String::from).collect(),
            references: Vec::new(),
            constraints: String::new(),
        },
        isolation: IsolationMode::Auto,
        depends_on: Vec::new(),
        priority: 0,
    }
}

fn make_task_map(specs: &[TaskSpec]) -> HashMap<String, Task> {
    specs
        .iter()
        .map(|s| (s.id.clone(), Task::new(s.clone())))
        .collect()
}

#[test]
fn test_independent_tasks_all_ready() {
    let tasks = vec![make_spec("t1"), make_spec("t2"), make_spec("t3")];
    let graph = DependencyGraph::new(&tasks, &[]).unwrap();
    let ready = graph.ready_tasks(&HashSet::new());

    assert_eq!(ready.len(), 3);
    assert!(ready.contains(&"t1".to_string()));
    assert!(ready.contains(&"t2".to_string()));
    assert!(ready.contains(&"t3".to_string()));
}

#[test]
fn test_linear_dependency_chain() {
    // t1 -> t2 -> t3: only t1 ready initially
    let tasks = vec![make_spec("t1"), make_spec("t2"), make_spec("t3")];
    let edges = vec![
        Edge {
            from: "t1".into(),
            to: "t2".into(),
        },
        Edge {
            from: "t2".into(),
            to: "t3".into(),
        },
    ];
    let graph = DependencyGraph::new(&tasks, &edges).unwrap();

    // Initially only t1 is ready
    let ready = graph.ready_tasks(&HashSet::new());
    assert_eq!(ready, vec!["t1".to_string()]);

    // After t1 completes, t2 becomes ready
    let mut completed: HashSet<String> = HashSet::new();
    completed.insert("t1".into());
    let ready = graph.ready_tasks(&completed);
    assert_eq!(ready, vec!["t2".to_string()]);

    // After t2 completes, t3 becomes ready
    completed.insert("t2".into());
    let ready = graph.ready_tasks(&completed);
    assert_eq!(ready, vec!["t3".to_string()]);
}

#[test]
fn test_diamond_dependency() {
    // t1 -> {t2, t3} -> t4
    let tasks = vec![
        make_spec("t1"),
        make_spec("t2"),
        make_spec("t3"),
        make_spec("t4"),
    ];
    let edges = vec![
        Edge {
            from: "t1".into(),
            to: "t2".into(),
        },
        Edge {
            from: "t1".into(),
            to: "t3".into(),
        },
        Edge {
            from: "t2".into(),
            to: "t4".into(),
        },
        Edge {
            from: "t3".into(),
            to: "t4".into(),
        },
    ];
    let graph = DependencyGraph::new(&tasks, &edges).unwrap();

    // Initially only t1 is ready
    let ready = graph.ready_tasks(&HashSet::new());
    assert_eq!(ready, vec!["t1".to_string()]);

    // After t1, both t2 and t3 are ready
    let mut completed: HashSet<String> = HashSet::new();
    completed.insert("t1".into());
    let ready = graph.ready_tasks(&completed);
    assert_eq!(ready.len(), 2);
    assert!(ready.contains(&"t2".to_string()));
    assert!(ready.contains(&"t3".to_string()));

    // After only t2, t4 is NOT yet ready (t3 still pending)
    completed.insert("t2".into());
    let ready = graph.ready_tasks(&completed);
    assert_eq!(ready, vec!["t3".to_string()]);

    // After both t2 and t3, t4 becomes ready
    completed.insert("t3".into());
    let ready = graph.ready_tasks(&completed);
    assert_eq!(ready, vec!["t4".to_string()]);
}

#[test]
fn test_cycle_detection() {
    let tasks = vec![make_spec("t1"), make_spec("t2")];
    let edges = vec![
        Edge {
            from: "t1".into(),
            to: "t2".into(),
        },
        Edge {
            from: "t2".into(),
            to: "t1".into(),
        },
    ];
    let result = DependencyGraph::new(&tasks, &edges);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("cycle"),
        "expected error mentioning 'cycle', got: {}",
        err
    );
}

#[test]
fn test_file_overlap_detection() {
    let task_a = make_spec_with_files("a", vec!["src/main.rs", "src/lib.rs"]);
    let task_b = make_spec_with_files("b", vec!["src/lib.rs", "src/utils.rs"]);
    let task_c = make_spec_with_files("c", vec!["tests/test.rs"]);

    // a and b share src/lib.rs
    assert!(Scheduler::has_file_overlap(&task_a, &task_b));

    // a and c have no overlap
    assert!(!Scheduler::has_file_overlap(&task_a, &task_c));

    // b and c have no overlap
    assert!(!Scheduler::has_file_overlap(&task_b, &task_c));
}

#[test]
fn test_assignable_respects_max_workers() {
    let specs = vec![make_spec("t1"), make_spec("t2"), make_spec("t3")];
    let scheduler = Scheduler::new(&specs, &[]).unwrap();
    let tasks = make_task_map(&specs);

    // max_workers=2, active_count=0 -> only 2 assignable
    let assignable = scheduler.assignable_tasks(&tasks, 2, 0);
    assert_eq!(assignable.len(), 2);

    // max_workers=2, active_count=1 -> only 1 assignable
    let assignable = scheduler.assignable_tasks(&tasks, 2, 1);
    assert_eq!(assignable.len(), 1);

    // max_workers=2, active_count=2 -> none assignable
    let assignable = scheduler.assignable_tasks(&tasks, 2, 2);
    assert_eq!(assignable.len(), 0);

    // max_workers=5, active_count=0 -> all 3 assignable
    let assignable = scheduler.assignable_tasks(&tasks, 5, 0);
    assert_eq!(assignable.len(), 3);
}
