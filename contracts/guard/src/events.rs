//! NEP-297 events. Each is emitted only from a call that succeeds
//! (decision 9), so every event a monitor reads describes state that stuck.

use near_sdk::json_types::{U64, U128};
use near_sdk::{AccountId, near};

use crate::types::{
    ConfigChange, Method, Mode, PaymentPayload, PendingReason, RefusalReason, RejectionReason,
    Settlement, Token,
};

#[near(event_json(standard = "near-spend-guard"))]
pub enum GuardEvent {
    #[event_version("1.0.0")]
    ConfigChanged {
        change: ConfigChange,
        token: Option<Token>,
        account_id: Option<AccountId>,
    },
    /// `window_spent` is the autonomous spending counted against
    /// `daily_max`, including this payment when `mode` is `autonomous`.
    #[event_version("1.0.0")]
    PaymentDispatched {
        request_id: U64,
        mode: Mode,
        token: Token,
        receiver_id: AccountId,
        amount: U128,
        method: Method,
        payload_hash: String,
        window_spent: U128,
        daily_max: U128,
    },
    /// `sent` is `null` when it can't be known (`uncertain`); `released` is
    /// what the callback actually gave back to the window.
    #[event_version("1.0.0")]
    PaymentSettled {
        request_id: U64,
        outcome: Settlement,
        sent: Option<U128>,
        released: U128,
    },
    #[event_version("1.0.0")]
    PaymentRefused {
        request_id: U64,
        reason: RefusalReason,
        token: Token,
        receiver_id: AccountId,
        amount: U128,
    },
    /// `window_spent` is before this payment, which reserves nothing yet.
    #[event_version("1.0.0")]
    ApprovalRequested {
        request_id: U64,
        reason: PendingReason,
        payload: PaymentPayload,
        payload_hash: String,
        window_spent: U128,
        daily_max: U128,
    },
    #[event_version("1.0.0")]
    RequestRejected {
        request_id: U64,
        by: AccountId,
        reason: RejectionReason,
    },
    #[event_version("1.0.0")]
    RequestExpired { request_id: U64 },
    #[event_version("1.0.0")]
    Withdrawal {
        token: Token,
        amount: U128,
        receiver_id: AccountId,
    },
    #[event_version("1.0.0")]
    Paused { by: AccountId },
    #[event_version("1.0.0")]
    Unpaused { by: AccountId },
}
