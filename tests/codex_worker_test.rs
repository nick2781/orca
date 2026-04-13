use orca::types::{TaskContext, TaskSpec};
use orca::worker::codex::{
    generate_agents_md, generate_prompt, parse_worker_line, MARKER_BLOCKED, MARKER_DONE,
    MARKER_ESCALATE, MARKER_PROGRESS,
};
use orca::worker::WorkerMessage;

#[test]
fn test_parse_done_marker() {
    let json = r#"{"files_changed":["src/main.rs"],"tests_passed":true,"diff_summary":"added feature","stdout":"ok"}"#;
    let line = format!("{} {}", MARKER_DONE, json);
    let msg = parse_worker_line(&line);

    match msg {
        WorkerMessage::Done(output) => {
            assert_eq!(output.files_changed, vec!["src/main.rs"]);
            assert!(output.tests_passed);
            assert_eq!(output.diff_summary, "added feature");
            assert_eq!(output.stdout, "ok");
        }
        other => panic!("expected Done, got {:?}", other),
    }
}

#[test]
fn test_parse_done_marker_plain_text() {
    let line = format!("{} task completed successfully", MARKER_DONE);
    let msg = parse_worker_line(&line);

    match msg {
        WorkerMessage::Done(output) => {
            assert_eq!(output.stdout, "task completed successfully");
            assert!(output.files_changed.is_empty());
            assert!(!output.tests_passed);
            assert!(output.diff_summary.is_empty());
        }
        other => panic!("expected Done with plain text, got {:?}", other),
    }
}

#[test]
fn test_parse_escalate_marker() {
    let json = r#"{"reason":"architecture decision needed","options":["A","B"]}"#;
    let line = format!("{} {}", MARKER_ESCALATE, json);
    let msg = parse_worker_line(&line);

    match msg {
        WorkerMessage::Escalate(value) => {
            assert_eq!(value["reason"], "architecture decision needed");
            let options = value["options"].as_array().unwrap();
            assert_eq!(options.len(), 2);
        }
        other => panic!("expected Escalate, got {:?}", other),
    }
}

#[test]
fn test_parse_blocked_marker() {
    let json = r#"{"blocker":"missing dependency","task_id":"task-001"}"#;
    let line = format!("{} {}", MARKER_BLOCKED, json);
    let msg = parse_worker_line(&line);

    match msg {
        WorkerMessage::Blocked(value) => {
            assert_eq!(value["blocker"], "missing dependency");
            assert_eq!(value["task_id"], "task-001");
        }
        other => panic!("expected Blocked, got {:?}", other),
    }
}

#[test]
fn test_parse_progress_marker() {
    let line = format!("{} compiling module 3/5", MARKER_PROGRESS);
    let msg = parse_worker_line(&line);

    assert_eq!(
        msg,
        WorkerMessage::Progress("compiling module 3/5".to_string())
    );
}

#[test]
fn test_parse_regular_output() {
    let line = "just a normal log line from the worker";
    let msg = parse_worker_line(line);

    assert_eq!(msg, WorkerMessage::Output(line.to_string()));
}

#[test]
fn test_generate_prompt() {
    let task = TaskSpec {
        id: "task-42".to_string(),
        title: "Add login endpoint".to_string(),
        description: "Implement POST /api/login with JWT".to_string(),
        context: TaskContext {
            files: vec!["src/auth.rs".to_string(), "src/handler.rs".to_string()],
            references: vec!["docs/auth-spec.md".to_string()],
            constraints: vec!["no unsafe code".to_string()],
        },
        isolation: orca::types::IsolationMode::Auto,
        depends_on: vec![],
        priority: 1,
    };

    let prompt = generate_prompt(&task, "/tmp/workdir");

    // Verify prompt contains task title and description
    assert!(
        prompt.contains("Add login endpoint"),
        "should contain task title"
    );
    assert!(
        prompt.contains("Implement POST /api/login with JWT"),
        "should contain task description"
    );

    // Verify prompt contains working directory
    assert!(prompt.contains("/tmp/workdir"), "should contain work dir");

    // Verify prompt contains files
    assert!(prompt.contains("src/auth.rs"), "should contain file");
    assert!(prompt.contains("src/handler.rs"), "should contain file");

    // Verify prompt contains constraints
    assert!(
        prompt.contains("no unsafe code"),
        "should contain constraint"
    );

    // Verify prompt contains references
    assert!(
        prompt.contains("docs/auth-spec.md"),
        "should contain reference"
    );

    // Verify prompt contains all markers in the rules section
    assert!(prompt.contains(MARKER_DONE), "should explain DONE marker");
    assert!(
        prompt.contains(MARKER_ESCALATE),
        "should explain ESCALATE marker"
    );
    assert!(
        prompt.contains(MARKER_BLOCKED),
        "should explain BLOCKED marker"
    );
    assert!(
        prompt.contains(MARKER_PROGRESS),
        "should explain PROGRESS marker"
    );
}

#[test]
fn test_parse_codex_json_message() {
    let line = r#"{"type":"message","content":"I'll start by reading the file"}"#;
    let msg = parse_worker_line(line);

    assert_eq!(
        msg,
        WorkerMessage::Output("I'll start by reading the file".to_string())
    );
}

#[test]
fn test_parse_codex_json_result() {
    let line = r#"{"type":"result","output":"file contents here"}"#;
    let msg = parse_worker_line(line);

    assert_eq!(
        msg,
        WorkerMessage::Output("file contents here".to_string())
    );
}

#[test]
fn test_parse_codex_json_unknown_type() {
    let line = r#"{"type":"command","command":"cat file.rs"}"#;
    let msg = parse_worker_line(line);

    // Unknown Codex JSON types are still captured as Output with the raw JSON.
    match msg {
        WorkerMessage::Output(text) => {
            assert!(
                text.contains("command"),
                "should contain the raw JSON content"
            );
        }
        other => panic!("expected Output, got {:?}", other),
    }
}

#[test]
fn test_marker_takes_priority() {
    // Even if the payload looks like Codex JSON, the ORCA marker wins.
    let json = r#"{"files_changed":["src/main.rs"],"tests_passed":true,"diff_summary":"done","stdout":"ok"}"#;
    let line = format!("{} {}", MARKER_DONE, json);
    let msg = parse_worker_line(&line);

    match msg {
        WorkerMessage::Done(output) => {
            assert_eq!(output.files_changed, vec!["src/main.rs"]);
            assert!(output.tests_passed);
        }
        other => panic!("expected Done, got {:?}", other),
    }
}

#[test]
fn test_non_codex_json_is_plain_output() {
    // JSON without a "type" field should NOT be parsed as Codex JSON.
    let line = r#"{"key":"value","number":42}"#;
    let msg = parse_worker_line(line);

    assert_eq!(msg, WorkerMessage::Output(line.to_string()));
}

#[test]
fn test_generate_agents_md() {
    let task = TaskSpec {
        id: "task-99".to_string(),
        title: "Refactor auth module".to_string(),
        description: "Extract JWT validation into a shared helper".to_string(),
        context: TaskContext {
            files: vec!["src/auth.rs".to_string(), "src/jwt.rs".to_string()],
            references: vec![],
            constraints: vec!["no breaking changes".to_string()],
        },
        isolation: orca::types::IsolationMode::Auto,
        depends_on: vec![],
        priority: 1,
    };

    let content = generate_agents_md(&task);

    // Should contain all ORCA markers from the template.
    assert!(
        content.contains("[ORCA:DONE]"),
        "should contain DONE marker"
    );
    assert!(
        content.contains("[ORCA:PROGRESS]"),
        "should contain PROGRESS marker"
    );
    assert!(
        content.contains("[ORCA:ESCALATE]"),
        "should contain ESCALATE marker"
    );
    assert!(
        content.contains("[ORCA:BLOCKED]"),
        "should contain BLOCKED marker"
    );

    // Should contain task-specific information.
    assert!(
        content.contains("Refactor auth module"),
        "should contain task title"
    );
    assert!(
        content.contains("Extract JWT validation"),
        "should contain task description"
    );
    assert!(
        content.contains("src/auth.rs"),
        "should list scoped files"
    );
    assert!(
        content.contains("src/jwt.rs"),
        "should list scoped files"
    );
    assert!(
        content.contains("no breaking changes"),
        "should list constraints"
    );
}
