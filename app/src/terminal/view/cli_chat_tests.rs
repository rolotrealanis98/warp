use warpui::{App, EntityId, TypedActionView as _, ViewHandle};

use super::*;
use crate::features::FeatureFlag;
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::{
    CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext, CLIAgentSessionStatus,
};
use crate::terminal::view::TerminalAction;
use crate::test_util::add_window_with_terminal;
use crate::test_util::terminal::initialize_app_for_terminal_view;

fn start_session(app: &mut App, view_id: EntityId, agent: CLIAgent) {
    CLIAgentSessionsModel::handle(app).update(app, |sessions, ctx| {
        sessions.set_session(
            view_id,
            CLIAgentSession {
                agent,
                status: CLIAgentSessionStatus::InProgress,
                session_context: CLIAgentSessionContext::default(),
                input_state: CLIAgentInputState::Closed,
                should_auto_toggle_input: false,
                listener: None,
                plugin_version: None,
                remote_host: None,
                draft_text: None,
                custom_command_prefix: None,
                received_rich_notification: false,
            },
            ctx,
        );
    });
}

fn toggle(app: &mut App, terminal: &ViewHandle<TerminalView>) -> bool {
    terminal.update(app, |view, ctx| {
        view.handle_action(&TerminalAction::ToggleCliChatView, ctx);
        view.is_cli_chat_view_shown()
    })
}

#[test]
fn toggle_is_a_no_op_without_a_supported_session() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let _flag = FeatureFlag::CliAgentChatView.override_enabled(true);
        let terminal = add_window_with_terminal(&mut app, None);
        let view_id = terminal.read(&app, |view, _| view.view_id);

        let without_session = toggle(&mut app, &terminal);
        start_session(&mut app, view_id, CLIAgent::Gemini);
        let with_other_agent = toggle(&mut app, &terminal);

        assert!(!without_session);
        assert!(!with_other_agent);
    })
}

#[test]
fn toggle_opens_chat_for_a_command_detected_copilot_session() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let _flag = FeatureFlag::CliAgentChatView.override_enabled(true);
        let terminal = add_window_with_terminal(&mut app, None);
        let view_id = terminal.read(&app, |view, _| view.view_id);
        start_session(&mut app, view_id, CLIAgent::Copilot);

        let shown = toggle(&mut app, &terminal);

        assert!(shown);
    })
}

#[test]
fn toggle_round_trips_between_terminal_and_chat() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let _flag = FeatureFlag::CliAgentChatView.override_enabled(true);
        let terminal = add_window_with_terminal(&mut app, None);
        let view_id = terminal.read(&app, |view, _| view.view_id);
        start_session(&mut app, view_id, CLIAgent::Claude);

        let after_first_toggle = toggle(&mut app, &terminal);
        let after_second_toggle = toggle(&mut app, &terminal);

        assert!(after_first_toggle);
        assert!(!after_second_toggle);
    })
}

#[test]
fn chat_view_closes_when_the_session_ends() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let _flag = FeatureFlag::CliAgentChatView.override_enabled(true);
        let terminal = add_window_with_terminal(&mut app, None);
        let view_id = terminal.read(&app, |view, _| view.view_id);
        start_session(&mut app, view_id, CLIAgent::Claude);
        toggle(&mut app, &terminal);

        CLIAgentSessionsModel::handle(&app).update(&mut app, |sessions, ctx| {
            sessions.remove_session(view_id, ctx);
        });

        terminal.read(&app, |view, _| assert!(!view.is_cli_chat_view_shown()));
    })
}
