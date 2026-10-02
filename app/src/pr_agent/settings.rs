//! Settings for the PR review agent (`pr_agent.*` in the settings file).

use std::time::Duration;

use settings::macros::define_settings_group;
use settings::{SupportedPlatforms, SyncToCloud};

use super::CheckoutMode;

pub(crate) const DEFAULT_POLL_INTERVAL_SECS: usize = 90;
/// Floor for the poll interval, so a typo cannot hammer the GitHub API.
const MIN_POLL_INTERVAL_SECS: usize = 15;

pub(crate) const DEFAULT_REVIEW_OTHER_TEMPLATE: &str = "\
Review pull request #{{number}} in {{repo}}: {{title}}
{{url}}

The pull request (by {{author}}, into {{base}}) is checked out in {{checkout_path}}. Read the \
change against {{base}} and make sure you understand it before giving feedback.

Warp may send you updates about this pull request: new commits, review comments, reviews and \
check results. Read and understand them, but do not act on them unless asked.";

pub(crate) const DEFAULT_WATCH_OWN_TEMPLATE: &str = "\
Watch my pull request #{{number}} in {{repo}}: {{title}}
{{url}}

It is checked out in {{checkout_path}} (base {{base}}). Get familiar with the change.

Warp will send you updates about this pull request: new commits, review comments, reviews and \
check results. Read and understand the review feedback and summarize it for me, but do not \
change code, push, or reply on GitHub unless I ask.";

define_settings_group!(PrAgentSettings, settings: [
    poll_interval_secs: PrAgentPollIntervalSecs {
        type: usize,
        default: DEFAULT_POLL_INTERVAL_SECS,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_agent.poll_interval_secs",
        description: "Seconds between checks of a watched pull request for new commits, reviews, comments and check results.",
    },
    auto_forward_events: PrAgentAutoForwardEvents {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_agent.auto_forward_events",
        description: "Whether pull request updates are sent to the agent as a message when it is idle.",
    },
    review_other_template: PrAgentReviewOtherTemplate {
        type: String,
        default: DEFAULT_REVIEW_OTHER_TEMPLATE.to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_agent.review_other_template",
        description: "Initial prompt for reviewing someone else's pull request. Variables: number, title, url, author, base, repo, checkout_path, branch.",
    },
    watch_own_template: PrAgentWatchOwnTemplate {
        type: String,
        default: DEFAULT_WATCH_OWN_TEMPLATE.to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_agent.watch_own_template",
        description: "Initial prompt for watching your own pull request. Variables: number, title, url, author, base, repo, checkout_path, branch.",
    },
    default_checkout: PrAgentDefaultCheckout {
        type: String,
        default: "worktree".to_string(),
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "pr_agent.default_checkout",
        description: "Where a pull request is checked out by default: worktree, branch or here.",
    },
]);

impl PrAgentSettings {
    pub(crate) fn poll_interval(&self) -> Duration {
        Duration::from_secs((*self.poll_interval_secs).max(MIN_POLL_INTERVAL_SECS) as u64)
    }

    pub(crate) fn default_checkout_mode(&self) -> CheckoutMode {
        CheckoutMode::from_setting(&self.default_checkout)
    }
}
