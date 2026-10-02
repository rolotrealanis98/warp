use std::io::Write as _;

use chrono::TimeZone as _;
use serde_json::json;

use super::*;

fn at(seconds: u32) -> Option<DateTime<Utc>> {
    Some(Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, seconds).unwrap())
}

#[test]
fn user_string_content_becomes_user_message() {
    let line = r#"{"type":"user","message":{"role":"user","content":"Add a greeting"},"timestamp":"2026-01-01T10:00:00.000Z","isSidechain":false}"#;

    let events = parse_records(line.as_bytes());

    assert_eq!(
        events,
        vec![ChatEvent::UserMessage {
            text: "Add a greeting".to_owned(),
            at: at(0),
        }]
    );
}

#[test]
fn assistant_blocks_map_to_text_thinking_and_tool_calls() {
    let lines = concat!(
        r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"Plan the edit.","signature":"x"}],"stop_reason":null},"timestamp":"2026-01-01T10:00:01.000Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading the file."}],"stop_reason":null},"timestamp":"2026-01-01T10:00:02.000Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/tmp/example/lib.rs"},"caller":{"type":"direct"}}],"stop_reason":"tool_use"},"timestamp":"2026-01-01T10:00:03.000Z"}"#,
    );

    let events = parse_records(lines.as_bytes());

    assert_eq!(
        events,
        vec![
            ChatEvent::Thinking {
                text: "Plan the edit.".to_owned()
            },
            ChatEvent::AssistantText {
                text: "Reading the file.".to_owned(),
                at: at(2),
            },
            ChatEvent::ToolCall {
                id: "toolu_1".to_owned(),
                name: "Read".to_owned(),
                input: json!({"file_path": "/tmp/example/lib.rs"}),
                at: at(3),
            },
        ]
    );
}

#[test]
fn redacted_thinking_blocks_are_dropped() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"","signature":"x"}],"stop_reason":null}}"#;

    assert_eq!(parse_records(line.as_bytes()), vec![]);
}

#[test]
fn end_turn_stop_reason_ends_the_turn() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}],"stop_reason":"end_turn"},"timestamp":"2026-01-01T10:00:09.000Z"}"#;

    let events = parse_records(line.as_bytes());

    assert_eq!(
        events,
        vec![
            ChatEvent::AssistantText {
                text: "Done.".to_owned(),
                at: at(9),
            },
            ChatEvent::TurnEnded { at: at(9) },
        ]
    );
}

#[test]
fn tool_result_array_content_is_flattened_and_keeps_error_flag() {
    let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","content":[{"type":"text","text":"line one"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}},{"type":"text","text":"line two"}],"is_error":true}]},"timestamp":"2026-01-01T10:00:04.000Z"}"#;

    let events = parse_records(line.as_bytes());

    assert_eq!(
        events,
        vec![ChatEvent::ToolResult {
            tool_use_id: "toolu_2".to_owned(),
            content: "line one\n[image]\nline two".to_owned(),
            is_error: true,
            at: at(4),
        }]
    );
}

#[test]
fn tool_result_string_content_defaults_to_success() {
    let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_3","content":"ok"}]}}"#;

    let events = parse_records(line.as_bytes());

    assert_eq!(
        events,
        vec![ChatEvent::ToolResult {
            tool_use_id: "toolu_3".to_owned(),
            content: "ok".to_owned(),
            is_error: false,
            at: None,
        }]
    );
}

#[test]
fn prompt_with_image_block_keeps_text_and_marks_image() {
    let line = r#"{"type":"user","message":{"content":[{"type":"text","text":"What is this?"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}]}}"#;

    let events = parse_records(line.as_bytes());

    assert_eq!(
        events,
        vec![ChatEvent::UserMessage {
            text: "What is this?\n[image]".to_owned(),
            at: None,
        }]
    );
}

#[test]
fn meta_and_non_message_records_are_ignored() {
    let lines = concat!(
        r#"{"type":"user","isMeta":true,"message":{"content":"expanded skill text"}}"#,
        "\n",
        r#"{"type":"attachment","attachment":{"type":"file"}}"#,
        "\n",
        r#"{"type":"file-history-snapshot","messageId":"m1","snapshot":{}}"#,
        "\n",
        r#"{"type":"permission-mode","permissionMode":"default"}"#,
        "\n",
    );

    assert_eq!(parse_records(lines.as_bytes()), vec![]);
}

#[test]
fn slash_command_becomes_notice() {
    let line = r#"{"type":"user","message":{"content":"<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>fast</command-args>"}}"#;

    assert_eq!(
        parse_records(line.as_bytes()),
        vec![ChatEvent::Notice {
            text: "/model fast".to_owned()
        }]
    );
}

#[test]
fn shell_mode_input_becomes_notice_with_output() {
    let line = r#"{"type":"user","message":{"content":"<bash-input>ls</bash-input><bash-stdout>a.txt</bash-stdout><bash-stderr></bash-stderr>"}}"#;

    assert_eq!(
        parse_records(line.as_bytes()),
        vec![ChatEvent::Notice {
            text: "! ls\na.txt".to_owned()
        }]
    );
}

#[test]
fn other_tagged_user_text_becomes_untagged_notice() {
    let line = r#"{"type":"user","message":{"content":"<task-notification><status>completed</status> Background task finished</task-notification>"}}"#;

    assert_eq!(
        parse_records(line.as_bytes()),
        vec![ChatEvent::Notice {
            text: "completed Background task finished".to_owned()
        }]
    );
}

#[test]
fn interrupt_ends_the_turn() {
    let line = r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]},"timestamp":"2026-01-01T10:00:05.000Z"}"#;

    assert_eq!(
        parse_records(line.as_bytes()),
        vec![
            ChatEvent::Notice {
                text: "Interrupted".to_owned()
            },
            ChatEvent::TurnEnded { at: at(5) },
        ]
    );
}

#[test]
fn titles_are_read_from_title_records() {
    let lines = concat!(
        r#"{"type":"ai-title","aiTitle":"Add a greeting","sessionId":"s"}"#,
        "\n",
        r#"{"type":"custom-title","customTitle":"EXAMPLE-123 greeting","sessionId":"s"}"#,
    );

    assert_eq!(
        parse_records(lines.as_bytes()),
        vec![
            ChatEvent::Title {
                text: "Add a greeting".to_owned(),
                is_custom: false,
            },
            ChatEvent::Title {
                text: "EXAMPLE-123 greeting".to_owned(),
                is_custom: true,
            },
        ]
    );
}

#[test]
fn malformed_lines_are_skipped() {
    let lines = concat!(
        "{not json\n",
        r#"{"type":"user","message":{"content":"Still parsed"}}"#,
    );

    assert_eq!(
        parse_records(lines.as_bytes()),
        vec![ChatEvent::UserMessage {
            text: "Still parsed".to_owned(),
            at: None,
        }]
    );
}

#[test]
fn read_incremental_consumes_only_complete_lines() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(
        concat!(
            r#"{"type":"user","message":{"content":"first"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"sec"#,
        )
        .as_bytes(),
    )
    .unwrap();
    let mut transcript = ClaudeTranscript::new(file.path().to_path_buf());

    let first_read = transcript.read_incremental();
    file.write_all(concat!(r#"ond"}}"#, "\n").as_bytes())
        .unwrap();
    let second_read = transcript.read_incremental();
    let third_read = transcript.read_incremental();

    assert_eq!(
        first_read,
        vec![ChatEvent::UserMessage {
            text: "first".to_owned(),
            at: None,
        }]
    );
    assert_eq!(
        second_read,
        vec![ChatEvent::UserMessage {
            text: "second".to_owned(),
            at: None,
        }]
    );
    assert_eq!(third_read, vec![]);
}

#[test]
fn record_longer_than_the_read_budget_is_skipped() {
    let mut transcript = ClaudeTranscript::new(PathBuf::from("unused.jsonl"));
    let oversized = "x".repeat(16);
    let next = r#"{"type":"user","message":{"content":"after"}}"#;

    let first = transcript.consume(oversized.as_bytes(), 16);
    let second = transcript.consume(format!("tail\n{next}\n").as_bytes(), 16);

    assert_eq!(first, vec![]);
    assert_eq!(
        second,
        vec![ChatEvent::UserMessage {
            text: "after".to_owned(),
            at: None,
        }]
    );
    assert_eq!(transcript.offset, 16 + 5 + next.len() as u64 + 1);
}

#[test]
fn subagent_transcript_is_found_by_parent_tool_call_id() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir
        .path()
        .join("00000000-0000-4000-8000-000000000001.jsonl");
    let subagents = dir
        .path()
        .join("00000000-0000-4000-8000-000000000001")
        .join("subagents");
    fs::create_dir_all(&subagents).unwrap();
    fs::write(&session, "").unwrap();
    fs::write(
        subagents.join("agent-a1.meta.json"),
        r#"{"agentType":"Explore","description":"Find callers","toolUseId":"toolu_other"}"#,
    )
    .unwrap();
    fs::write(
        subagents.join("agent-a2.meta.json"),
        r#"{"agentType":"Explore","description":"Find tests","toolUseId":"toolu_agent"}"#,
    )
    .unwrap();
    fs::write(
        subagents.join("agent-a2.jsonl"),
        "{\"type\":\"user\",\"isSidechain\":true,\"message\":{\"content\":\"Find tests\"}}\n",
    )
    .unwrap();
    let transcript = ClaudeTranscript::new(session);

    let mut subagent = transcript.subagent("toolu_agent").unwrap();

    assert_eq!(
        subagent.read_incremental(),
        vec![ChatEvent::UserMessage {
            text: "Find tests".to_owned(),
            at: None,
        }]
    );
    assert!(transcript.subagent("toolu_missing").is_none());
}

#[cfg(not(target_family = "wasm"))]
#[test]
fn reported_transcript_path_is_used_when_it_names_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("00000000-0000-4000-8000-000000000001.jsonl");
    fs::write(&path, "").unwrap();

    let located = ClaudeTranscript::locate(
        path.to_str(),
        Some("00000000-0000-4000-8000-000000000001"),
        None,
    );

    assert_eq!(located, Some(path));
}

#[test]
fn session_title_uses_latest_generated_title() {
    let head = concat!(
        r#"{"type":"user","message":{"role":"user","content":"Add a greeting"}}"#,
        "\n",
        r#"{"type":"ai-title","aiTitle":"Greeting draft"}"#,
        "\n",
    );
    let tail = concat!(
        r#"{"type":"ai-title","aiTitle":"Add greeting banner"}"#,
        "\n"
    );

    let title = session_title(head.as_bytes(), tail.as_bytes());

    assert_eq!(title.as_deref(), Some("Add greeting banner"));
}

#[test]
fn session_title_prefers_custom_title_over_generated() {
    let head = concat!(
        r#"{"type":"custom-title","customTitle":"EXAMPLE-123 greeting"}"#,
        "\n",
        r#"{"type":"ai-title","aiTitle":"Add greeting banner"}"#,
        "\n",
    );

    let title = session_title(head.as_bytes(), b"");

    assert_eq!(title.as_deref(), Some("EXAMPLE-123 greeting"));
}

#[test]
fn session_title_falls_back_to_first_line_of_first_prompt() {
    let head = concat!(
        r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Caveat: meta"}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":"\n  Fix the build\nIt fails on CI."}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":"Also run the tests"}}"#,
        "\n",
    );

    let title = session_title(head.as_bytes(), b"");

    assert_eq!(title.as_deref(), Some("Fix the build"));
}

#[test]
fn session_title_is_none_without_a_prompt() {
    let head = concat!(
        r#"{"type":"file-history-snapshot","snapshot":{}}"#,
        "\n",
        r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Caveat: meta"}}"#,
        "\n",
    );

    assert_eq!(session_title(head.as_bytes(), b""), None);
}

#[test]
fn read_session_title_reads_the_end_of_a_long_transcript() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, r#"{{"type":"ai-title","aiTitle":"Early title"}}"#).unwrap();
    let filler = format!(r#"{{"type":"progress","data":"{}"}}"#, "x".repeat(1024));
    for _ in 0..600 {
        writeln!(file, "{filler}").unwrap();
    }
    writeln!(file, r#"{{"type":"ai-title","aiTitle":"Current title"}}"#).unwrap();

    let title = read_session_title(file.path());

    assert_eq!(title.as_deref(), Some("Current title"));
}
