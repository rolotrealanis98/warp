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

#[test]
fn turn_maps_to_prompt_reasoning_tool_call_answer_and_turn_end() {
    let bytes = lines(&[
        r#"{"timestamp":"2026-01-01T10:00:00Z","type":"session_meta","payload":{"id":"00000000-0000-4000-8000-000000000001","cwd":"/work/octo/repo","cli_version":"0.1.0"}}"#,
        r#"{"timestamp":"2026-01-01T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/work/octo/repo</cwd>\n</environment_context>"},{"type":"input_text","text":"Fix EXAMPLE-123"}]}}"#,
        r#"{"timestamp":"2026-01-01T10:00:02Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"Run the tests."}],"encrypted_content":"x"}}"#,
        r#"{"timestamp":"2026-01-01T10:00:03Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo test\",\"workdir\":\"/work/octo/repo\"}","call_id":"call_1"}}"#,
        r#"{"timestamp":"2026-01-01T10:00:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"Wall time: 1.0 seconds\nProcess exited with code 0\nOutput:\nok"}}"#,
        r#"{"timestamp":"2026-01-01T10:00:05Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Tests pass."}]}}"#,
        r#"{"timestamp":"2026-01-01T10:00:05Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#,
        r#"{"timestamp":"2026-01-01T10:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"1"}}"#,
    ]);

    let events = parse_records(&bytes);

    assert_eq!(
        events,
        vec![
            ChatEvent::UserMessage {
                text: "Fix EXAMPLE-123".to_owned(),
                at: at(1),
            },
            ChatEvent::Thinking {
                text: "Run the tests.".to_owned(),
            },
            ChatEvent::ToolCall {
                id: "call_1".to_owned(),
                name: "exec_command".to_owned(),
                input: json!({"cmd": "cargo test", "command": "cargo test", "workdir": "/work/octo/repo"}),
                at: at(3),
            },
            ChatEvent::ToolResult {
                tool_use_id: "call_1".to_owned(),
                content: "Wall time: 1.0 seconds\nProcess exited with code 0\nOutput:\nok"
                    .to_owned(),
                is_error: false,
                at: at(4),
            },
            ChatEvent::AssistantText {
                text: "Tests pass.".to_owned(),
                at: at(5),
            },
            ChatEvent::TurnEnded { at: at(6) },
        ]
    );
}

#[test]
fn context_only_user_and_developer_messages_are_dropped() {
    let bytes = lines(&[
        r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /work/octo/repo\n\nBe brief."}]}}"##,
        r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"You are Codex."}]}}"#,
    ]);

    assert_eq!(parse_records(&bytes), vec![]);
}

#[test]
fn nonzero_exit_code_marks_the_result_as_an_error() {
    let bytes = lines(&[
        r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"Process exited with code 101\nOutput:\nfailed"}}"#,
    ]);

    let events = parse_records(&bytes);

    assert!(
        matches!(&events[..], [ChatEvent::ToolResult { is_error: true, .. }]),
        "{events:?}"
    );
}

#[test]
fn code_mode_exec_shows_its_script_as_the_command_and_flags_script_failures() {
    let bytes = lines(&[
        r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"text(await tools.exec_command({cmd: \"ls\"}))","call_id":"call_2","status":"completed"}}"#,
        r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_2","output":[{"type":"input_text","text":"Script failed\nWall time: 0.1 seconds"},{"type":"input_text","text":"boom"}]}}"#,
    ]);

    let events = parse_records(&bytes);

    assert_eq!(
        events,
        vec![
            ChatEvent::ToolCall {
                id: "call_2".to_owned(),
                name: "exec".to_owned(),
                input: json!({"command": "text(await tools.exec_command({cmd: \"ls\"}))"}),
                at: None,
            },
            ChatEvent::ToolResult {
                tool_use_id: "call_2".to_owned(),
                content: "Script failed\nWall time: 0.1 seconds\nboom".to_owned(),
                is_error: true,
                at: None,
            },
        ]
    );
}

#[test]
fn apply_patch_keeps_the_patch_and_names_its_first_file() {
    let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
    let record = json!({
        "type": "response_item",
        "payload": {"type": "custom_tool_call", "name": "apply_patch", "input": patch, "call_id": "call_3"},
    });

    let events = parse_records(&lines(&[&record.to_string()]));

    assert_eq!(
        events,
        vec![ChatEvent::ToolCall {
            id: "call_3".to_owned(),
            name: "apply_patch".to_owned(),
            input: json!({"file_path": "src/lib.rs", "patch": patch}),
            at: None,
        }]
    );
}

#[test]
fn shell_argv_becomes_a_command_string() {
    let bytes = lines(&[
        r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"git status\"]}","call_id":"call_4"}}"#,
    ]);

    let events = parse_records(&bytes);

    assert_eq!(
        events,
        vec![ChatEvent::ToolCall {
            id: "call_4".to_owned(),
            name: "shell".to_owned(),
            input: json!({"command": "git status"}),
            at: None,
        }]
    );
}

#[test]
fn namespaced_function_call_gets_an_mcp_style_name() {
    let bytes = lines(&[
        r#"{"type":"response_item","payload":{"type":"function_call","name":"search","namespace":"mcp__docs","arguments":"{\"query\":\"greeting\"}","call_id":"call_5"}}"#,
    ]);

    let events = parse_records(&bytes);

    assert!(
        matches!(&events[..], [ChatEvent::ToolCall { name, .. }] if name == "mcp__docs__search"),
        "{events:?}"
    );
}

#[test]
fn aborted_turn_is_an_interrupt_that_ends_the_turn() {
    let bytes = lines(&[
        r#"{"timestamp":"2026-01-01T10:00:09Z","type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}"#,
    ]);

    assert_eq!(
        parse_records(&bytes),
        vec![
            ChatEvent::Notice {
                text: "Interrupted".to_owned(),
            },
            ChatEvent::TurnEnded { at: at(9) },
        ]
    );
}

#[cfg(not(target_family = "wasm"))]
mod locate {
    use std::fs;
    use std::time::{Duration, SystemTime};

    use super::*;

    /// Writes a rollout for a session in `cwd` under today's day directory.
    fn write_rollout(root: &Path, name: &str, cwd: &str, modified: SystemTime) -> PathBuf {
        let today = Utc::now();
        let dir = root
            .join(today.format("%Y").to_string())
            .join(today.format("%m").to_string())
            .join(today.format("%d").to_string());
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let meta = json!({"type": "session_meta", "payload": {"id": "x", "cwd": cwd}});
        fs::write(&path, format!("{meta}\n")).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        path
    }

    #[test]
    fn newest_rollout_in_the_cwd_wins() {
        let root = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        write_rollout(
            root.path(),
            "rollout-a-1.jsonl",
            "/work/octo/repo",
            now - Duration::from_secs(20),
        );
        let newest = write_rollout(
            root.path(),
            "rollout-b-2.jsonl",
            "/work/octo/repo",
            now - Duration::from_secs(10),
        );
        write_rollout(root.path(), "rollout-c-3.jsonl", "/work/octo/other", now);

        let located = newest_rollout_for_cwd(
            root.path(),
            Path::new("/work/octo/repo"),
            Utc::now() - chrono::Duration::minutes(1),
        );

        assert_eq!(located, Some(newest));
    }

    #[test]
    fn rollouts_last_written_before_the_agent_started_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        write_rollout(
            root.path(),
            "rollout-a-1.jsonl",
            "/work/octo/repo",
            hour_ago,
        );

        let located = newest_rollout_for_cwd(
            root.path(),
            Path::new("/work/octo/repo"),
            Utc::now() - chrono::Duration::minutes(1),
        );

        assert_eq!(located, None);
    }

    #[test]
    fn reported_transcript_path_is_used_when_it_names_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("rollout-2026-01-01T10-00-00-00000000-0000-4000-8000-000000000001.jsonl");
        fs::write(&path, "").unwrap();

        let located = CodexTranscript::locate(
            path.to_str(),
            Some("00000000-0000-4000-8000-000000000001"),
            None,
            Utc::now(),
        );

        assert_eq!(located, Some(path));
    }
}
