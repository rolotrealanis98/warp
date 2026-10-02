use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud, ToggleableSetting};
use warp_errors::report_if_error;
use warpui::{AppContext, ModelContext, SingletonEntity};

define_settings_group!(CliChatViewSettings, settings: [
    open_on_session_start: OpenCliChatViewOnSessionStart {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "cli_chat_view.open_on_session_start",
        description: "Whether a pane switches to the chat view when a Claude Code session starts.",
    },
    collapse_thinking: CliChatViewCollapseThinking {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "cli_chat_view.collapse_thinking",
        description: "Whether thinking blocks start collapsed in the chat view.",
    },
    collapse_tool_output: CliChatViewCollapseToolOutput {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "cli_chat_view.collapse_tool_output",
        description: "Whether successful tool calls start collapsed in the chat view.",
    },
    show_timestamps: CliChatViewShowTimestamps {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "cli_chat_view.show_timestamps",
        description: "Whether messages in the chat view show their time.",
    },
]);

/// The switches the chat view exposes on the CLI agents settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliChatViewToggle {
    OpenOnSessionStart,
    CollapseThinking,
    CollapseToolOutput,
    ShowTimestamps,
}

impl CliChatViewToggle {
    pub(crate) fn value(self, app: &AppContext) -> bool {
        let settings = CliChatViewSettings::as_ref(app);
        match self {
            CliChatViewToggle::OpenOnSessionStart => *settings.open_on_session_start,
            CliChatViewToggle::CollapseThinking => *settings.collapse_thinking,
            CliChatViewToggle::CollapseToolOutput => *settings.collapse_tool_output,
            CliChatViewToggle::ShowTimestamps => *settings.show_timestamps,
        }
    }
}

impl CliChatViewSettings {
    /// Flips one switch and persists it.
    pub(crate) fn toggle(&mut self, toggle: CliChatViewToggle, ctx: &mut ModelContext<Self>) {
        report_if_error!(match toggle {
            CliChatViewToggle::OpenOnSessionStart => {
                self.open_on_session_start.toggle_and_save_value(ctx)
            }
            CliChatViewToggle::CollapseThinking =>
                self.collapse_thinking.toggle_and_save_value(ctx),
            CliChatViewToggle::CollapseToolOutput => {
                self.collapse_tool_output.toggle_and_save_value(ctx)
            }
            CliChatViewToggle::ShowTimestamps => self.show_timestamps.toggle_and_save_value(ctx),
        });
    }
}
