use std::fs;
use std::time::{Duration, SystemTime};

use chrono::TimeZone as _;
use serde_json::json;

use super::*;

fn at(seconds: u32) -> Option<DateTime<Utc>> {
    Some(Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, seconds).unwrap())
}

fn lines(records: &[&str]) -> Vec<u8> {
    let mut bytes = records.join("\n").into_bytes();
    bytes.push(b'\n');
    bytes
}

fn parse(bytes: &[u8]) -> Vec<ChatEvent> {
    parse_records(bytes, &mut false)
}

#[test]
fn turn_maps_to_prompt_thinking_tool_call_and_answer() {
    let bytes = lines(&[
        r#"{"type":"session.start","data":{"sessionId":"s1","context":{"cwd":"/work/octo/repo"}},"timestamp":"2026-01-01T10:00:00.000Z"}"#,
        r#"{"type":"user.message","data":{"content":"Fix EXAMPLE-123","transformedContent":"<context/>Fix EXAMPLE-123"},"timestamp":"2026-01-01T10:00:01.000Z"}"#,
        r#"{"type":"assistant.message","data":{"content":"","reasoningText":"Check the file.","toolRequests":[{"toolCallId":"t1","name":"view"}]},"timestamp":"2026-01-01T10:00:02.000Z"}"#,
        r#"{"type":"tool.execution_start","data":{"toolCallId":"t1","toolName":"view","arguments":{"path":"/work/octo/repo/README.md"}},"timestamp":"2026-01-01T10:00:03.000Z"}"#,
        r#"{"type":"tool.execution_complete","data":{"toolCallId":"t1","success":true,"result":{"content":"repo readme","detailedContent":"repo readme"}},"timestamp":"2026-01-01T10:00:04.000Z"}"#,
        r#"{"type":"assistant.message","data":{"content":"Done.","toolRequests":[]},"timestamp":"2026-01-01T10:00:05.000Z"}"#,
        r#"{"type":"assistant.turn_end","data":{"turnId":"1"},"timestamp":"2026-01-01T10:00:06.000Z"}"#,
    ]);

    let events = parse(&bytes);

    assert_eq!(
        events,
        vec![
            ChatEvent::UserMessage {
                text: "Fix EXAMPLE-123".to_owned(),
                at: at(1),
            },
            ChatEvent::Thinking {
                text: "Check the file.".to_owned(),
            },
            ChatEvent::ToolCall {
                id: "t1".to_owned(),
                name: "view".to_owned(),
                input: json!({"path": "/work/octo/repo/README.md"}),
                at: at(3),
            },
            ChatEvent::ToolResult {
                tool_use_id: "t1".to_owned(),
                content: "repo readme".to_owned(),
                is_error: false,
                at: at(4),
            },
            ChatEvent::AssistantText {
                text: "Done.".to_owned(),
                at: at(5),
            },
            ChatEvent::TurnEnded { at: at(6) },
        ]
    );
}

#[test]
fn turn_end_after_a_tool_request_does_not_end_the_turn_across_reads() {
    let mut awaiting_tools = false;
    let first = lines(&[
        r#"{"type":"assistant.message","data":{"content":"Looking.","toolRequests":[{"toolCallId":"t1","name":"bash"}]}}"#,
    ]);
    let second = lines(&[r#"{"type":"assistant.turn_end","data":{"turnId":"1"}}"#]);

    parse_records(&first, &mut awaiting_tools);
    let events = parse_records(&second, &mut awaiting_tools);

    assert_eq!(events, vec![]);
}

#[test]
fn permission_request_is_an_attention_item_and_its_reply_a_notice() {
    let bytes = lines(&[
        r#"{"type":"permission.requested","data":{"requestId":"r1","permissionRequest":{"kind":"shell","toolCallId":"t2","intention":"Run tests","fullCommandText":"cargo test"}}}"#,
        r#"{"type":"permission.completed","data":{"requestId":"r1","toolCallId":"t2","result":{"kind":"approved"}}}"#,
        r#"{"type":"permission.completed","data":{"requestId":"r2","toolCallId":"t3","result":{"kind":"denied-interactively-by-user"}}}"#,
    ]);

    let events = parse(&bytes);

    assert_eq!(
        events,
        vec![
            ChatEvent::Attention {
                text: "Permission: Run tests\n$ cargo test".to_owned(),
            },
            ChatEvent::Notice {
                text: "Permission granted".to_owned(),
            },
            ChatEvent::Notice {
                text: "Permission denied".to_owned(),
            },
        ]
    );
}

#[test]
fn failed_tool_execution_is_an_error_result_with_its_message() {
    let bytes = lines(&[
        r#"{"type":"tool.execution_complete","data":{"toolCallId":"t1","success":false,"error":{"code":"denied","message":"Permission denied"}}}"#,
    ]);

    assert_eq!(
        parse(&bytes),
        vec![ChatEvent::ToolResult {
            tool_use_id: "t1".to_owned(),
            content: "Permission denied".to_owned(),
            is_error: true,
            at: None,
        }]
    );
}

#[test]
fn hooks_usage_and_system_records_are_ignored() {
    let bytes = lines(&[
        r#"{"type":"hook.start","data":{"hookType":"preToolUse"}}"#,
        r#"{"type":"session.usage_checkpoint","data":{}}"#,
        r#"{"type":"system.message","data":{"content":"You are Copilot.","role":"system"}}"#,
    ]);

    assert_eq!(parse(&bytes), vec![]);
}

/// Writes a session directory with a `workspace.yaml` for `cwd` and an
/// `events.jsonl` last modified at `modified`.
fn write_session(root: &Path, id: &str, cwd: &str, modified: SystemTime) -> PathBuf {
    let dir = root.join("session-state").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("workspace.yaml"),
        format!("id: {id}\ncwd: {cwd}\nsummary_count: 0\n"),
    )
    .unwrap();
    let events = dir.join("events.jsonl");
    fs::write(&events, "").unwrap();
    fs::File::options()
        .write(true)
        .open(&events)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    events
}

#[test]
fn locate_prefers_an_open_session_in_the_cwd_over_a_newer_closed_one() {
    let root = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let open = write_session(
        root.path(),
        "s-open",
        "/work/octo/repo",
        now - Duration::from_secs(20),
    );
    write_session(
        root.path(),
        "s-closed",
        "/work/octo/repo",
        now - Duration::from_secs(10),
    );
    write_session(root.path(), "s-elsewhere", "/work/octo/other", now);
    fs::write(
        root.path().join("open-sessions-state.json"),
        r#"{"s-open":{"openedAt":"2026-01-01T10:00:00Z","working":true}}"#,
    )
    .unwrap();

    let located = newest_session_for_cwd(
        root.path(),
        Path::new("/work/octo/repo"),
        Utc::now() - chrono::Duration::minutes(1),
    );

    assert_eq!(located, Some(open));
}

#[test]
fn locate_takes_the_newest_session_in_the_cwd_written_since_the_agent_started() {
    let root = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    write_session(
        root.path(),
        "s-old",
        "/work/octo/repo",
        now - Duration::from_secs(3600),
    );
    let newest = write_session(
        root.path(),
        "s-new",
        "/work/octo/repo",
        now - Duration::from_secs(5),
    );

    let located = newest_session_for_cwd(
        root.path(),
        Path::new("/work/octo/repo"),
        Utc::now() - chrono::Duration::minutes(1),
    );

    assert_eq!(located, Some(newest));
}
