use warpui::{App, EntityId};

use super::*;
use crate::features::FeatureFlag;
use crate::terminal::CLIAgent;
use crate::terminal::cli_agent_sessions::{
    CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext, CLIAgentSessionStatus,
};
use crate::test_util::add_window_with_terminal;
use crate::test_util::terminal::initialize_app_for_terminal_view;

fn start_claude_session(app: &mut App, view_id: EntityId) {
    CLIAgentSessionsModel::handle(app).update(app, |sessions, ctx| {
        sessions.set_session(
            view_id,
            CLIAgentSession {
                agent: CLIAgent::Claude,
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

#[test]
fn new_agent_conversation_toggles_the_chat_view_of_a_running_claude_session() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let _flag = FeatureFlag::CliAgentChatView.override_enabled(true);
        let terminal = add_window_with_terminal(&mut app, None);
        let view_id = terminal.read(&app, |view, _| view.view_id);
        start_claude_session(&mut app, view_id);

        let shown = terminal.update(&mut app, |view, ctx| {
            view.start_or_show_cli_agent(None, ctx);
            view.is_cli_chat_view_shown()
        });
        let hidden = terminal.update(&mut app, |view, ctx| {
            view.start_or_show_cli_agent(None, ctx);
            !view.is_cli_chat_view_shown()
        });

        assert!(shown);
        assert!(hidden);
    })
}

#[test]
fn running_command_prompt_quotes_the_command_and_its_output() {
    let prompt = active_command_prompt("  npm run dev ", "listening on 3000\nerror: boom\n\n");

    assert_eq!(
        prompt,
        "This command is running in my terminal. Help me with it.\n\n```\n$ npm run dev\nlistening on 3000\nerror: boom\n```"
    );
}
