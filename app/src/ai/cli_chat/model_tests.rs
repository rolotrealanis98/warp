use chrono::TimeZone as _;
use serde_json::json;

use super::*;

fn at(seconds: u32) -> Option<DateTime<Utc>> {
    Some(Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, seconds).unwrap())
}

fn prompt(text: &str) -> ChatEvent {
    ChatEvent::UserMessage {
        text: text.to_owned(),
        at: at(0),
    }
}

fn call(id: &str, name: &str, input: Value) -> ChatEvent {
    ChatEvent::ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        input,
        at: at(1),
    }
}

fn result(id: &str, is_error: bool) -> ChatEvent {
    ChatEvent::ToolResult {
        tool_use_id: id.to_owned(),
        content: "output".to_owned(),
        is_error,
        at: at(3),
    }
}

fn thread(events: Vec<ChatEvent>) -> Thread {
    let mut thread = Thread::default();
    thread.apply(events);
    thread
}

#[test]
fn tool_result_attaches_to_its_call() {
    let thread = thread(vec![
        call("t1", "Bash", json!({"command": "cargo test"})),
        result("t1", true),
    ]);

    let ChatItem::Tool(tool) = &thread.items[0] else {
        panic!("expected a tool item, got {:?}", thread.items[0]);
    };
    assert_eq!(thread.items.len(), 1);
    assert!(tool.is_error());
    assert_eq!(tool.duration(), Some(chrono::Duration::seconds(2)));
}

#[test]
fn consecutive_successful_tools_collapse_into_a_group() {
    let thread = thread(vec![
        prompt("go"),
        call("t1", "Read", json!({})),
        result("t1", false),
        call("t2", "Grep", json!({})),
        result("t2", false),
        call("t3", "Edit", json!({})),
        result("t3", false),
    ]);

    assert_eq!(rows(&thread.items), vec![Row::Item(0), Row::ToolGroup(1..4)]);
}

#[test]
fn failed_and_pending_tools_stay_out_of_groups() {
    let thread = thread(vec![
        call("t1", "Read", json!({})),
        result("t1", false),
        call("t2", "Read", json!({})),
        result("t2", false),
        call("t3", "Bash", json!({})),
        result("t3", true),
        call("t4", "Read", json!({})),
        result("t4", false),
        call("t5", "Read", json!({})),
    ]);

    assert_eq!(
        rows(&thread.items),
        vec![
            Row::ToolGroup(0..2),
            Row::Item(2),
            Row::Item(3),
            Row::Item(4),
        ]
    );
}

#[test]
fn subagent_calls_are_never_grouped() {
    let thread = thread(vec![
        call("t1", "Agent", json!({"description": "Find tests"})),
        result("t1", false),
        call("t2", "Read", json!({})),
        result("t2", false),
    ]);

    assert_eq!(rows(&thread.items), vec![Row::Item(0), Row::Item(1)]);
}

#[test]
fn group_label_counts_kinds_in_order_of_appearance() {
    let thread = thread(vec![
        call("t1", "Read", json!({})),
        call("t2", "Read", json!({})),
        call("t3", "Bash", json!({})),
        call("t4", "mcp__docs__search", json!({})),
        call("t5", "Read", json!({})),
    ]);
    let tools = thread.items.iter().filter_map(|item| match item {
        ChatItem::Tool(tool) => Some(tool),
        _ => None,
    });

    assert_eq!(group_label(tools), "3 reads · 1 command · 1 MCP call");
}

#[test]
fn turn_summary_lists_edited_files_commands_and_errors() {
    let thread = thread(vec![
        prompt("go"),
        call("t1", "Edit", json!({"file_path": "/tmp/example/a.rs"})),
        result("t1", false),
        call("t2", "Write", json!({"file_path": "/tmp/example/b.rs"})),
        result("t2", false),
        call("t3", "Edit", json!({"file_path": "/tmp/example/a.rs"})),
        result("t3", false),
        call("t4", "Bash", json!({"command": "cargo test"})),
        result("t4", true),
        ChatEvent::TurnEnded { at: at(5) },
    ]);
    let end = thread.items.len() - 1;

    assert_eq!(
        turn_summary(&thread.items, end),
        Some(TurnSummary {
            files: vec![
                ("/tmp/example/a.rs".to_owned(), "t1".to_owned()),
                ("/tmp/example/b.rs".to_owned(), "t2".to_owned()),
            ],
            commands: 1,
            errors: vec!["t4".to_owned()],
        })
    );
}

#[test]
fn turn_summary_only_covers_the_last_turn() {
    let thread = thread(vec![
        prompt("first"),
        call("t1", "Bash", json!({})),
        result("t1", false),
        ChatEvent::TurnEnded { at: at(5) },
        prompt("second"),
        ChatEvent::AssistantText {
            text: "No tools needed.".to_owned(),
            at: at(6),
        },
        ChatEvent::TurnEnded { at: at(7) },
    ]);
    let end = thread.items.len() - 1;

    assert_eq!(turn_summary(&thread.items, end), None);
}

#[test]
fn active_turn_reports_last_tool_and_running_subagents() {
    let thread = thread(vec![
        prompt("go"),
        call("t1", "Agent", json!({"description": "Find tests"})),
        call("t2", "Agent", json!({"description": "Find callers"})),
        result("t2", false),
        call("t3", "Read", json!({})),
    ]);

    assert_eq!(
        thread.active_turn(),
        Some(ActiveTurn {
            started_at: at(0),
            last_tool: Some("Read".to_owned()),
            running_subagents: 1,
        })
    );
}

#[test]
fn active_turn_is_none_once_the_turn_ends() {
    let thread = thread(vec![prompt("go"), ChatEvent::TurnEnded { at: at(2) }]);

    assert_eq!(thread.active_turn(), None);
}

#[test]
fn repeated_turn_end_events_add_one_marker() {
    let thread = thread(vec![
        prompt("go"),
        ChatEvent::TurnEnded { at: at(2) },
        ChatEvent::TurnEnded { at: at(3) },
    ]);

    assert_eq!(thread.items.len(), 2);
}

#[test]
fn custom_title_wins_over_generated_title() {
    let thread = thread(vec![
        ChatEvent::Title {
            text: "EXAMPLE-123 short title".to_owned(),
            is_custom: true,
        },
        ChatEvent::Title {
            text: "Generated title".to_owned(),
            is_custom: false,
        },
    ]);

    assert_eq!(thread.title(), Some("EXAMPLE-123 short title"));
}

#[test]
fn mcp_tool_name_splits_into_server_and_tool() {
    let thread = thread(vec![call("t1", "mcp__issue_tracker__get_issue", json!({}))]);
    let ChatItem::Tool(tool) = &thread.items[0] else {
        panic!("expected a tool item");
    };

    assert_eq!(
        tool.mcp_server_and_tool(),
        Some(("issue_tracker", "get_issue"))
    );
}

#[test]
fn tool_summary_uses_the_most_telling_input_field() {
    let thread = thread(vec![
        call("t1", "Bash", json!({"command": "cargo test\n--quiet", "description": "Run tests"})),
        call("t2", "TodoWrite", json!({"todos": [{}, {}]})),
        call("t3", "mcp__docs__search", json!({"query": "flags"})),
    ]);
    let summaries: Vec<String> = thread
        .items
        .iter()
        .filter_map(|item| match item {
            ChatItem::Tool(tool) => Some(tool.summary()),
            _ => None,
        })
        .collect();

    assert_eq!(summaries, vec!["cargo test", "2 todos", "flags"]);
}
