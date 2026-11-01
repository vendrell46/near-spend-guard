//! NEP-297 events. Each is emitted only from a call that succeeds
//! (decision 9), so every event a monitor reads describes state that stuck.

use near_sdk::{AccountId, near};

use crate::types::{ConfigChange, Token};

#[near(event_json(standard = "near-spend-guard"))]
pub enum GuardEvent {
    #[event_version("1.0.0")]
    ConfigChanged {
        change: ConfigChange,
        token: Option<Token>,
        account_id: Option<AccountId>,
    },
    #[event_version("1.0.0")]
    Paused { by: AccountId },
    #[event_version("1.0.0")]
    Unpaused { by: AccountId },
}
