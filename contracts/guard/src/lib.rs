//! near-spend-guard: an on-chain spending guard for AI agents on NEAR.
//!
//! The guard holds the funds. The agent calls it through a function-call key
//! and the guard enforces per-payment caps, a rolling daily cap, receiver
//! allowlists, payload-bound human approvals and a kill switch. The interface
//! is specified in INTERFACE.md in the plan; this file follows it.

use near_sdk::json_types::{U64, U128};
use near_sdk::store::IterableMap;
use near_sdk::{AccountId, PanicOnDefault, assert_one_yocto, env, near, require};

pub mod events;
pub mod types;

#[cfg(test)]
mod tests;

use events::GuardEvent;
use types::{
    ConfigChange, ConfigView, LimitsView, PendingEntry, Policy, ReceiverRule, Token, TokenState,
    TokenView,
};

pub const VERSION: &str = "1.0.0";

/// Slots in the rolling window: the current one and the 24 before it.
pub const WINDOW_SLOTS: usize = 25;
pub const MAX_PENDING: u32 = 16;
pub const MAX_RECEIVERS: usize = 32;
pub const MAX_TOKENS: u32 = 8;
pub const MAX_MSG_BYTES: usize = 1024;
pub const MAX_EXPIRY_NS: u64 = 7 * 24 * 60 * 60 * 1_000_000_000;
/// One second. A slot given in seconds or milliseconds by mistake would make
/// every block a new window, so the rolling cap would never bind
/// (decision 17).
pub const MIN_SLOT_NS: u64 = 1_000_000_000;

pub const ERR_NOT_OWNER: &str = "only the owner can call this";
pub const ERR_NOT_OWNER_OR_APPROVER: &str = "only the owner or the approver can call this";
pub const ERR_SLOT_TOO_SHORT: &str = "slot_ns must be at least 1 second (1000000000)";
pub const ERR_AGENT_IS_OWNER: &str = "the agent cannot be the owner";
pub const ERR_AGENT_IS_APPROVER: &str = "the agent cannot be the approver";
pub const ERR_AGENT_IS_GUARD: &str = "the agent cannot be the guard";
pub const ERR_AGENT_IS_RECEIVER: &str = "the agent cannot be a receiver";
pub const ERR_OWNER_IS_GUARD: &str = "the owner cannot be the guard";
pub const ERR_APPROVER_IS_GUARD: &str = "the approver cannot be the guard";
pub const ERR_RECEIVER_IS_GUARD: &str = "the receiver cannot be the guard";
pub const ERR_TOKEN_IS_GUARD: &str = "the token contract cannot be the guard";
pub const ERR_THRESHOLD_ABOVE_MAX: &str = "approval_above cannot exceed per_payment_max";
pub const ERR_TOO_MANY_TOKENS: &str = "at most 8 tokens";
pub const ERR_TOO_MANY_RECEIVERS: &str = "at most 32 receivers per token";
pub const ERR_TOKEN_NOT_CONFIGURED: &str = "token not configured";
pub const ERR_RECEIVER_NOT_ALLOWED: &str = "receiver not allowed";
pub const ERR_FT_CALL_NATIVE: &str = "ft_call needs an FT token";

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Guard {
    owner_id: AccountId,
    agent_id: AccountId,
    approver_id: AccountId,
    paused: bool,
    /// One hour in production; seconds in sandbox tests.
    slot_ns: u64,
    next_request_id: u64,
    /// Given to the next token added.
    next_generation: u64,
    /// At most `MAX_TOKENS`.
    tokens: IterableMap<Token, TokenState>,
    /// At most `MAX_PENDING`.
    pending: IterableMap<u64, PendingEntry>,
}

#[near]
impl Guard {
    /// Deploy and initialise in one transaction.
    #[init]
    #[private]
    pub fn new(
        owner_id: AccountId,
        agent_id: AccountId,
        approver_id: AccountId,
        slot_ns: U64,
    ) -> Self {
        require!(slot_ns.0 >= MIN_SLOT_NS, ERR_SLOT_TOO_SHORT);
        let guard_id = env::current_account_id();
        // Nobody can sign as the guard once its own key is deleted, so an
        // owner or approver equal to the guard could never act (decision 15).
        require!(owner_id != guard_id, ERR_OWNER_IS_GUARD);
        require!(approver_id != guard_id, ERR_APPROVER_IS_GUARD);
        check_agent(&agent_id, &owner_id, &approver_id);
        Self {
            owner_id,
            agent_id,
            approver_id,
            paused: false,
            slot_ns: slot_ns.0,
            next_request_id: 1,
            next_generation: 1,
            tokens: IterableMap::new(b"t"),
            pending: IterableMap::new(b"p"),
        }
    }

    // ---------- owner configuration (exactly 1 yocto) ----------

    /// Creates or updates a token's policy. An update keeps the receivers and
    /// the spending window.
    #[payable]
    pub fn set_token(
        &mut self,
        token: Token,
        per_payment_max: U128,
        daily_max: U128,
        approval_above: U128,
    ) {
        self.require_owner();
        require!(
            approval_above.0 <= per_payment_max.0,
            ERR_THRESHOLD_ABOVE_MAX
        );
        if let Token::Ft(contract_id) = &token {
            require!(
                contract_id != &env::current_account_id(),
                ERR_TOKEN_IS_GUARD
            );
        }
        let policy = Policy {
            per_payment_max: per_payment_max.0,
            daily_max: daily_max.0,
            approval_above: approval_above.0,
        };
        match self.tokens.get_mut(&token) {
            Some(state) => state.policy = policy,
            None => {
                require!(self.tokens.len() < MAX_TOKENS, ERR_TOO_MANY_TOKENS);
                let generation = self.next_generation;
                self.next_generation += 1;
                self.tokens
                    .insert(token.clone(), TokenState::new(policy, generation));
            }
        }
        config_changed(ConfigChange::Token, Some(token), None);
    }

    /// Deletes the policy, receivers and window. Re-adding the token starts a
    /// fresh window: an owner power, listed in the README.
    #[payable]
    pub fn remove_token(&mut self, token: Token) {
        self.require_owner();
        require!(
            self.tokens.remove(&token).is_some(),
            ERR_TOKEN_NOT_CONFIGURED
        );
        config_changed(ConfigChange::TokenRemoved, Some(token), None);
    }

    /// Adds a receiver, or updates its `ft_call` flag if it is already there.
    #[payable]
    pub fn allow_receiver(&mut self, token: Token, receiver_id: AccountId, ft_call: bool) {
        self.require_owner();
        require!(
            receiver_id != env::current_account_id(),
            ERR_RECEIVER_IS_GUARD
        );
        require!(receiver_id != self.agent_id, ERR_AGENT_IS_RECEIVER);
        require!(!ft_call || token != Token::Native, ERR_FT_CALL_NATIVE);
        let state = self.token_state_mut(&token);
        match state
            .receivers
            .iter_mut()
            .find(|r| r.account_id == receiver_id)
        {
            Some(rule) => rule.ft_call = ft_call,
            None => {
                require!(
                    state.receivers.len() < MAX_RECEIVERS,
                    ERR_TOO_MANY_RECEIVERS
                );
                state.receivers.push(ReceiverRule {
                    account_id: receiver_id.clone(),
                    ft_call,
                });
            }
        }
        config_changed(
            ConfigChange::ReceiverAllowed,
            Some(token),
            Some(receiver_id),
        );
    }

    /// Pending requests to this receiver are refused at `approve`.
    #[payable]
    pub fn disallow_receiver(&mut self, token: Token, receiver_id: AccountId) {
        self.require_owner();
        let state = self.token_state_mut(&token);
        let before = state.receivers.len();
        state.receivers.retain(|r| r.account_id != receiver_id);
        require!(state.receivers.len() < before, ERR_RECEIVER_NOT_ALLOWED);
        config_changed(
            ConfigChange::ReceiverRemoved,
            Some(token),
            Some(receiver_id),
        );
    }

    /// Same checks as `new`, and the new agent must not be a receiver of any
    /// token (decision 10).
    #[payable]
    pub fn set_agent(&mut self, agent_id: AccountId) {
        self.require_owner();
        check_agent(&agent_id, &self.owner_id, &self.approver_id);
        require!(
            self.tokens
                .values()
                .all(|state| state.receiver(&agent_id).is_none()),
            ERR_AGENT_IS_RECEIVER
        );
        // Decision 12 lands with the pending requests themselves (day 5):
        // every pending request is cancelled here.
        self.agent_id = agent_id.clone();
        config_changed(ConfigChange::Agent, None, Some(agent_id));
    }

    /// Pending requests stay.
    #[payable]
    pub fn set_approver(&mut self, approver_id: AccountId) {
        self.require_owner();
        require!(approver_id != self.agent_id, ERR_AGENT_IS_APPROVER);
        require!(
            approver_id != env::current_account_id(),
            ERR_APPROVER_IS_GUARD
        );
        self.approver_id = approver_id.clone();
        config_changed(ConfigChange::Approver, None, Some(approver_id));
    }

    // ---------- kill switch ----------

    /// No deposit, so it works from a function-call key limited to `pause`
    /// (decision 11). Pausing a paused guard succeeds and emits nothing.
    pub fn pause(&mut self) {
        let caller = env::predecessor_account_id();
        require!(
            caller == self.owner_id || caller == self.approver_id,
            ERR_NOT_OWNER_OR_APPROVER
        );
        if !self.paused {
            self.paused = true;
            GuardEvent::Paused { by: caller }.emit();
        }
    }

    #[payable]
    pub fn unpause(&mut self) {
        self.require_owner();
        if self.paused {
            self.paused = false;
            GuardEvent::Unpaused {
                by: env::predecessor_account_id(),
            }
            .emit();
        }
    }

    // ---------- views ----------

    pub fn get_config(&self) -> ConfigView {
        ConfigView {
            owner_id: self.owner_id.clone(),
            agent_id: self.agent_id.clone(),
            approver_id: self.approver_id.clone(),
            paused: self.paused,
            slot_ns: U64(self.slot_ns),
            limits: LimitsView {
                window_slots: WINDOW_SLOTS as u32,
                max_pending: MAX_PENDING,
                max_receivers: MAX_RECEIVERS as u32,
                max_tokens: MAX_TOKENS,
                max_msg_bytes: MAX_MSG_BYTES as u32,
                max_expiry_ns: U64(MAX_EXPIRY_NS),
                min_slot_ns: U64(MIN_SLOT_NS),
            },
            version: VERSION.to_string(),
        }
    }

    pub fn get_tokens(&self) -> Vec<TokenView> {
        self.tokens
            .iter()
            .map(|(token, state)| token_view(token, state))
            .collect()
    }

    pub fn get_token(&self, token: Token) -> Option<TokenView> {
        self.tokens
            .get(&token)
            .map(|state| token_view(&token, state))
    }
}

impl Guard {
    /// Exactly 1 yocto first (it forces a full-access key and a wallet
    /// confirmation), then the caller.
    fn require_owner(&self) {
        assert_one_yocto();
        require!(
            env::predecessor_account_id() == self.owner_id,
            ERR_NOT_OWNER
        );
    }

    fn token_state_mut(&mut self, token: &Token) -> &mut TokenState {
        self.tokens
            .get_mut(token)
            .unwrap_or_else(|| env::panic_str(ERR_TOKEN_NOT_CONFIGURED))
    }
}

/// The agent can never be the owner, the approver or the guard
/// (decision 10).
fn check_agent(agent_id: &AccountId, owner_id: &AccountId, approver_id: &AccountId) {
    require!(agent_id != owner_id, ERR_AGENT_IS_OWNER);
    require!(agent_id != approver_id, ERR_AGENT_IS_APPROVER);
    require!(agent_id != &env::current_account_id(), ERR_AGENT_IS_GUARD);
}

fn config_changed(change: ConfigChange, token: Option<Token>, account_id: Option<AccountId>) {
    GuardEvent::ConfigChanged {
        change,
        token,
        account_id,
    }
    .emit();
}

fn token_view(token: &Token, state: &TokenState) -> TokenView {
    TokenView {
        token: token.clone(),
        policy: state.policy.into(),
        receivers: state.receivers.clone(),
    }
}
