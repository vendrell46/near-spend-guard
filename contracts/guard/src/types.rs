//! Types from INTERFACE.md. JSON is what the agent sends and the approver's
//! wallet shows; borsh is what the guard stores.

use near_sdk::json_types::{U64, U128};
use near_sdk::{AccountId, near};

use crate::WINDOW_SLOTS;

/// What a payment moves. An enum rather than a string such as `"near"`, which
/// could also be read as an account id.
#[near(serializers = [borsh, json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Token {
    Native,
    Ft(AccountId),
}

/// The exact payment a human approves. `approve` compares it field by field
/// with the stored one (decision 7).
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentPayload {
    /// Assigned by the guard.
    pub request_id: U64,
    pub token: Token,
    pub receiver_id: AccountId,
    /// The token's smallest unit; yoctoNEAR for native.
    pub amount: U128,
    /// `Some` means `ft_transfer_call` (FT only); `None` means `ft_transfer`
    /// or a native transfer.
    pub msg: Option<String>,
    /// Nanoseconds, in `block_timestamp` units.
    pub expires_at: U64,
}

#[near(serializers = [borsh])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub per_payment_max: u128,
    pub daily_max: u128,
    pub approval_above: u128,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiverRule {
    pub account_id: AccountId,
    /// Whether `ft_transfer_call` (a payment with `msg`) may target this
    /// receiver.
    pub ft_call: bool,
}

/// One hour of spending. `epoch = block_timestamp / slot_ns`.
#[near(serializers = [borsh])]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slot {
    pub epoch: u64,
    pub autonomous: u128,
    pub approved: u128,
}

/// Everything the guard keeps for one token. Receivers and slots are inline,
/// so no nested collection needs its own storage prefix.
#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenState {
    pub policy: Policy,
    pub receivers: Vec<ReceiverRule>,
    pub slots: [Slot; WINDOW_SLOTS],
    /// New each time the token is added. A callback releases budget only if
    /// the token still has the generation it was dispatched under, so a
    /// payment from before `remove_token` can't free budget in the window of
    /// a re-added token (decision 16).
    pub generation: u64,
}

impl TokenState {
    pub fn new(policy: Policy, generation: u64) -> Self {
        Self {
            policy,
            receivers: Vec::new(),
            slots: [Slot::default(); WINDOW_SLOTS],
            generation,
        }
    }

    pub fn receiver(&self, account_id: &AccountId) -> Option<&ReceiverRule> {
        self.receivers.iter().find(|r| &r.account_id == account_id)
    }
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEntry {
    pub payload: PaymentPayload,
    pub payload_hash: [u8; 32],
    pub reason: PendingReason,
    pub created_at: u64,
}

#[near(serializers = [borsh, json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Autonomous,
    Approved,
}

/// Derived from the token and `msg`.
#[near(serializers = [borsh, json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Native,
    FtTransfer,
    FtTransferCall,
}

#[near(serializers = [borsh, json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingReason {
    AboveThreshold,
    DailyCap,
}

#[near(serializers = [borsh, json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalReason {
    Paused,
    TokenNotConfigured,
    ReceiverNotAllowed,
    TransferCallNotAllowed,
    AbovePerPaymentMax,
    TooManyPending,
}

/// Returned by `request_payment` and `approve`.
#[near(serializers = [json])]
#[serde(tag = "status", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestOutcome {
    /// Policy said no. Nothing reserved or stored.
    Refused {
        request_id: U64,
        reason: RefusalReason,
    },
    /// Waiting for the approver.
    Pending {
        request_id: U64,
        reason: PendingReason,
        payload_hash: String,
    },
    /// Final, from the callback. `sent` is below the amount only after an
    /// `ft_transfer_call` refund.
    Paid { request_id: U64, sent: U128 },
    /// A single-receipt transfer failed: nothing moved, budget released.
    Failed { request_id: U64, released: U128 },
    /// `ft_transfer_call` reported failure; tokens may have moved, so the
    /// budget stays spent (decision 5).
    Uncertain { request_id: U64 },
    /// `approve` arrived after `expires_at`; the request is removed.
    Expired { request_id: U64 },
}

/// The `change` field of a `config_changed` event.
#[near(serializers = [json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigChange {
    Token,
    TokenRemoved,
    ReceiverAllowed,
    ReceiverRemoved,
    Agent,
    Approver,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyView {
    pub per_payment_max: U128,
    pub daily_max: U128,
    pub approval_above: U128,
}

impl From<Policy> for PolicyView {
    fn from(p: Policy) -> Self {
        Self {
            per_payment_max: U128(p.per_payment_max),
            daily_max: U128(p.daily_max),
            approval_above: U128(p.approval_above),
        }
    }
}

/// Policy and receivers; the window numbers arrive with the ledger.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenView {
    pub token: Token,
    pub policy: PolicyView,
    pub receivers: Vec<ReceiverRule>,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitsView {
    pub window_slots: u32,
    pub max_pending: u32,
    pub max_receivers: u32,
    pub max_tokens: u32,
    pub max_msg_bytes: u32,
    pub max_expiry_ns: U64,
    pub min_slot_ns: U64,
}

#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigView {
    pub owner_id: AccountId,
    pub agent_id: AccountId,
    pub approver_id: AccountId,
    pub paused: bool,
    pub slot_ns: U64,
    pub limits: LimitsView,
    pub version: String,
}
