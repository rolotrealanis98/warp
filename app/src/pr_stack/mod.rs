//! PR stack view: a left-panel ledger of the branch stack under the focused
//! repo, with stack-relative stats, PR status, PR creation and restacking.
//! Gated by `FeatureFlag::PrStackView`.

pub mod restack;
pub mod settings;
pub mod stack;
pub mod stats;
pub mod status;
pub mod submit;

pub use settings::PrStackSettings;
