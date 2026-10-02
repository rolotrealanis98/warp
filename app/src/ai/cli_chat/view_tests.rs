use serde_json::json;
use warpui::geometry::vector::vec2f;
use warpui::platform::WindowStyle;
use warpui::{App, EntityIdSet, Presenter, WindowInvalidation};

use super::*;
use crate::ai::cli_chat::source::ChatEvent;
use crate::test_util::terminal::initialize_app_for_terminal_view;

fn transcript() -> Vec<ChatEvent> {
    let call = |id: &str, name: &str, input: Value| ChatEvent::ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        input,
        at: None,
    };
    let result = |id: &str, is_error: bool| ChatEvent::ToolResult {
        tool_use_id: id.to_owned(),
        content: "line one\nline two".to_owned(),
        is_error,
        at: None,
    };
    vec![
        ChatEvent::Title {
            text: "EXAMPLE-123 short title".to_owned(),
            is_custom: true,
        },
        ChatEvent::UserMessage {
            text: "Add a greeting".to_owned(),
            at: None,
        },
        ChatEvent::Thinking {
            text: "Plan the edit.".to_owned(),
        },
        call("t1", "Read", json!({"file_path": "/tmp/example/lib.rs"})),
        result("t1", false),
        call("t2", "Grep", json!({"pattern": "greet"})),
        result("t2", false),
        call(
            "t3",
            "Edit",
            json!({"file_path": "/tmp/example/lib.rs", "old_string": "a\nb\n", "new_string": "a\nc\n"}),
        ),
        result("t3", false),
        call("t4", "Bash", json!({"command": "cargo test"})),
        result("t4", true),
        call("t5", "mcp__docs__search", json!({"query": "greeting"})),
        call("t6", "Agent", json!({"description": "Find tests"})),
        ChatEvent::AssistantText {
            text: "Done. See `lib.rs`:\n\n- one\n- two".to_owned(),
            at: None,
        },
        ChatEvent::TurnEnded { at: None },
        ChatEvent::Notice {
            text: "/model fast".to_owned(),
        },
    ]
}

#[test]
fn populated_transcript_lays_out_with_cards_expanded() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let (window_id, chat) = app.add_window(WindowStyle::NotStealFocus, |ctx| {
            CliChatView::new(EntityId::new(), CLIAgent::Claude, None, ctx)
        });
        chat.update(&mut app, |view, ctx| {
            view.model.update(ctx, |model, _| {
                model.apply_for_test(None, transcript());
                model.apply_for_test(
                    Some("t6"),
                    vec![ChatEvent::UserMessage {
                        text: "Find tests".to_owned(),
                        at: None,
                    }],
                );
            });
            for key in ["group:t1", "tool:t3", "tool:t6", "thinking:1"] {
                view.toggle(key);
            }
        });

        let mut updated = EntityIdSet::default();
        updated.insert(app.root_view_id(window_id).unwrap());
        let invalidation = WindowInvalidation {
            updated,
            ..Default::default()
        };
        let mut presenter = Presenter::new(window_id);
        app.update(move |ctx| {
            presenter.invalidate(invalidation, ctx);
            presenter.build_scene(vec2f(900., 700.), 1., None, ctx);
        });

        chat.read(&app, |view, ctx| {
            let thread = view.model.as_ref(ctx).thread();
            assert_eq!(thread.title(), Some("EXAMPLE-123 short title"));
            assert!(view.model.as_ref(ctx).subagent_thread("t6").is_some());
        });
    })
}
