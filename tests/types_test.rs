use orca::types::*;

#[test]
fn test_task_creation() {
    let spec = TaskSpec::new("Test task", "A test task description");
    let task = Task::new(spec);

    assert_eq!(task.state, TaskState::Pending);
    assert!(task.worker_id.is_none());
    assert!(task.escalation_id.is_none());
    assert!(task.output.is_none());
    assert!(task.worktree_path.is_none());
    assert!(task.branch_name.is_none());
}

#[test]
fn test_valid_state_transitions() {
    let spec = TaskSpec::new("Test task", "Walk the happy path");
    let mut task = Task::new(spec);

    // Pending -> Assigned
    assert!(task.transition_to(TaskState::Assigned).is_ok());
    assert_eq!(task.state, TaskState::Assigned);

    // Assigned -> Running
    assert!(task.transition_to(TaskState::Running).is_ok());
    assert_eq!(task.state, TaskState::Running);

    // Running -> Done
    assert!(task.transition_to(TaskState::Done).is_ok());
    assert_eq!(task.state, TaskState::Done);

    // Done -> Review
    assert!(task.transition_to(TaskState::Review).is_ok());
    assert_eq!(task.state, TaskState::Review);

    // Review -> Accepted
    assert!(task.transition_to(TaskState::Accepted).is_ok());
    assert_eq!(task.state, TaskState::Accepted);

    // Accepted -> Completed
    assert!(task.transition_to(TaskState::Completed).is_ok());
    assert_eq!(task.state, TaskState::Completed);
}

#[test]
fn test_invalid_state_transitions() {
    let spec = TaskSpec::new("Test task", "Try invalid transitions");
    let mut task = Task::new(spec);

    // Pending -> Running should fail (must go through Assigned)
    assert!(task.transition_to(TaskState::Running).is_err());
    assert_eq!(task.state, TaskState::Pending);

    // Pending -> Completed should fail
    assert!(task.transition_to(TaskState::Completed).is_err());
    assert_eq!(task.state, TaskState::Pending);
}

#[test]
fn test_blocked_resume_cycle() {
    let spec = TaskSpec::new("Test task", "Block and resume");
    let mut task = Task::new(spec);

    task.transition_to(TaskState::Assigned).unwrap();
    task.transition_to(TaskState::Running).unwrap();

    // Running -> Blocked
    assert!(task.transition_to(TaskState::Blocked).is_ok());
    assert_eq!(task.state, TaskState::Blocked);

    // Blocked -> Running (resume)
    assert!(task.transition_to(TaskState::Running).is_ok());
    assert_eq!(task.state, TaskState::Running);
}

#[test]
fn test_rejected_rework_cycle() {
    let spec = TaskSpec::new("Test task", "Reject and rework");
    let mut task = Task::new(spec);

    task.transition_to(TaskState::Assigned).unwrap();
    task.transition_to(TaskState::Running).unwrap();
    task.transition_to(TaskState::Done).unwrap();
    task.transition_to(TaskState::Review).unwrap();

    // Review -> Rejected
    assert!(task.transition_to(TaskState::Rejected).is_ok());
    assert_eq!(task.state, TaskState::Rejected);

    // Rejected -> Pending (rework)
    assert!(task.transition_to(TaskState::Pending).is_ok());
    assert_eq!(task.state, TaskState::Pending);
}

#[test]
fn test_taskspec_json_roundtrip() {
    let spec = TaskSpec {
        id: "task-001".to_string(),
        title: "Implement feature".to_string(),
        description: "Build the thing".to_string(),
        context: TaskContext {
            files: vec!["src/main.rs".to_string()],
            references: vec!["docs/spec.md".to_string()],
            constraints: vec!["no unsafe".to_string()],
        },
        isolation: IsolationMode::Worktree,
        depends_on: vec!["task-000".to_string()],
        priority: 5,
    };

    let json = serde_json::to_string(&spec).expect("serialize");
    let deserialized: TaskSpec = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(deserialized.id, spec.id);
    assert_eq!(deserialized.title, spec.title);
    assert_eq!(deserialized.description, spec.description);
    assert_eq!(deserialized.isolation, IsolationMode::Worktree);
    assert_eq!(deserialized.depends_on, vec!["task-000"]);
    assert_eq!(deserialized.priority, 5);
    assert_eq!(deserialized.context.files, vec!["src/main.rs"]);
    assert_eq!(deserialized.context.references, vec!["docs/spec.md"]);
    assert_eq!(deserialized.context.constraints, vec!["no unsafe"]);
}
