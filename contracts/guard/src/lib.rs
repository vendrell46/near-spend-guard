//! near-spend-guard: an on-chain spending guard for AI agents on NEAR.
//!
//! The guard holds the funds. The agent calls it through a function-call key
//! and the guard enforces per-payment caps, a rolling daily cap, receiver
//! allowlists, payload-bound human approvals and a kill switch. The interface
//! is specified in INTERFACE.md in the plan; this file follows it.

use near_sdk::json_types::{U64, U128};
use near_sdk::serde_json::{self, json};
use near_sdk::store::IterableMap;
use near_sdk::{
    AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseError, PromiseOrValue,
    assert_one_yocto, env, near, require,
};

pub mod events;
pub mod types;

#[cfg(test)]
mod tests;

use events::GuardEvent;
use types::{
    ConfigChange, ConfigView, LimitsView, Method, Mode, PaymentPayload, PendingEntry,
    PendingReason, PendingView, Policy, ReceiverRule, RefusalReason, RejectionReason,
    RequestOutcome, Settlement, Token, TokenState, TokenView,
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
/// Domain string of the payload hash, so a hash can't be confused with one
/// from another format or contract.
pub const PAYLOAD_DOMAIN: &str = "near-spend-guard/payload/v1";
/// A transfer's result is read up to this many bytes (decision 13).
pub const MAX_RESULT_BYTES: usize = 64;

pub const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
/// The reference token forwards what is left above its 5 TGas resolve step
/// to the receiver's `ft_on_transfer`.
pub const GAS_FT_TRANSFER_CALL: Gas = Gas::from_tgas(60);
pub const GAS_CALLBACK: Gas = Gas::from_tgas(10);
/// For the guard's own work after the check: the reservation, the receipts
/// and the event.
pub const GAS_MARGIN: Gas = Gas::from_tgas(5);

pub const ERR_NOT_OWNER: &str = "only the owner can call this";
pub const ERR_NOT_OWNER_OR_APPROVER: &str = "only the owner or the approver can call this";
pub const ERR_NOT_APPROVER: &str = "only the approver can call this";
pub const ERR_PAUSED: &str = "the guard is paused";
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
pub const ERR_NOT_AGENT: &str = "only the agent can call this";
pub const ERR_ZERO_AMOUNT: &str = "amount must be above 0";
pub const ERR_EXPIRED: &str = "expires_at must be in the future";
pub const ERR_EXPIRY_TOO_FAR: &str = "expires_at can be at most 7 days ahead";
pub const ERR_MSG_NATIVE: &str = "msg needs an FT token";
pub const ERR_MSG_TOO_LONG: &str = "msg can be at most 1024 bytes";
pub const ERR_NOT_ENOUGH_GAS: &str = "not enough gas for the transfer and its callback";
pub const ERR_OVERFLOW: &str = "arithmetic overflow";

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
        self.agent_id = agent_id.clone();
        // An old, possibly compromised agent's requests don't outlive it
        // (decision 12).
        let mut cancelled: Vec<u64> = self.pending.keys().copied().collect();
        cancelled.sort_unstable();
        self.pending.clear();
        config_changed(ConfigChange::Agent, None, Some(agent_id));
        let owner = env::predecessor_account_id();
        for id in cancelled {
            GuardEvent::RequestRejected {
                request_id: U64(id),
                by: owner.clone(),
                reason: RejectionReason::AgentChanged,
            }
            .emit();
        }
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

    /// The owner's exit: not counted in any cap, allowed while paused, and
    /// for any token, configured or not, so nothing can get stuck in the
    /// guard. Returns the transfer.
    #[payable]
    pub fn withdraw(&mut self, token: Token, amount: U128, receiver_id: AccountId) -> Promise {
        self.require_owner();
        require!(amount.0 > 0, ERR_ZERO_AMOUNT);
        require!(
            receiver_id != env::current_account_id(),
            ERR_RECEIVER_IS_GUARD
        );
        let transfer = match &token {
            Token::Native => {
                Promise::new(receiver_id.clone()).transfer(NearToken::from_yoctonear(amount.0))
            }
            Token::Ft(contract_id) => Promise::new(contract_id.clone()).function_call(
                "ft_transfer",
                serde_json::to_vec(&json!({
                    "receiver_id": receiver_id,
                    "amount": amount,
                    "memo": "nsg:withdraw",
                }))
                .unwrap_or_else(|_| env::abort()),
                NearToken::from_yoctonear(1),
                GAS_FT_TRANSFER,
            ),
        };
        GuardEvent::Withdrawal {
            token,
            amount,
            receiver_id,
        }
        .emit();
        transfer
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

    // ---------- agent ----------

    /// The agent's only method. Policy refusals and payments held for a human
    /// return with a reason and an event (decision 9). A dispatched payment
    /// returns the transfer's promise, so the agent's transaction ends with
    /// the callback's outcome (decision 14).
    pub fn request_payment(
        &mut self,
        token: Token,
        receiver_id: AccountId,
        amount: U128,
        msg: Option<String>,
        expires_at: U64,
    ) -> PromiseOrValue<RequestOutcome> {
        // 1. Wrong caller or malformed input: panic, nothing recorded.
        require!(
            env::predecessor_account_id() == self.agent_id,
            ERR_NOT_AGENT
        );
        require!(amount.0 > 0, ERR_ZERO_AMOUNT);
        require!(
            receiver_id != env::current_account_id(),
            ERR_RECEIVER_IS_GUARD
        );
        let now = env::block_timestamp();
        require!(expires_at.0 > now, ERR_EXPIRED);
        require!(expires_at.0 - now <= MAX_EXPIRY_NS, ERR_EXPIRY_TOO_FAR);
        let method = payment_method(&token, &msg);
        require_dispatch_gas(method);

        // 2. Every request that passes the checks above gets an id.
        let request_id = self.next_request_id;
        self.next_request_id = checked(request_id.checked_add(1));
        let payload = PaymentPayload {
            request_id: U64(request_id),
            token,
            receiver_id,
            amount,
            msg,
            expires_at,
        };

        // 3. Policy refusals.
        if self.paused {
            return refuse(payload, RefusalReason::Paused);
        }
        if let Some(reason) = self.policy_refusal(&payload) {
            return refuse(payload, reason);
        }

        // 4. Above the threshold or over the rolling cap: wait for a human
        //    (decision 8).
        let epoch = now / self.slot_ns;
        let state = self
            .tokens
            .get(&payload.token)
            .unwrap_or_else(|| env::panic_str(ERR_TOKEN_NOT_CONFIGURED));
        let policy = state.policy;
        let spent = state.window_spent(epoch);
        let after = spent.checked_add(amount.0);
        let hold = if amount.0 > policy.approval_above {
            Some(PendingReason::AboveThreshold)
        } else if after.is_none_or(|total| total > policy.daily_max) {
            Some(PendingReason::DailyCap)
        } else {
            None
        };
        if let Some(reason) = hold {
            return self.hold(payload, reason, spent, policy.daily_max, now);
        }

        // 5. Reserve in the receipt that dispatches; only the callback can
        //    give it back.
        let state = self.token_state_mut(&payload.token);
        let slot = state.slot_mut(epoch);
        slot.autonomous = checked(slot.autonomous.checked_add(amount.0));
        let generation = state.generation;
        let window_spent = checked(after);
        PromiseOrValue::Promise(dispatch(
            payload,
            method,
            Mode::Autonomous,
            epoch,
            generation,
            window_spent,
            policy.daily_max,
        ))
    }

    // ---------- approver ----------

    /// Executes exactly the payload the approver's wallet shows. The full
    /// payload is the argument (decision 1) and is compared field by field
    /// with the stored one (decision 7); exactly 1 yocto forces a full-access
    /// key and a wallet confirmation.
    #[payable]
    pub fn approve(&mut self, payload: PaymentPayload) -> PromiseOrValue<RequestOutcome> {
        // 1. Deposit, then caller, as the owner methods do.
        assert_one_yocto();
        let approver = env::predecessor_account_id();
        require!(approver == self.approver_id, ERR_NOT_APPROVER);

        // 2. Only the stored payload executes. A mismatch keeps the entry, so
        //    the approver can still reject it.
        let request_id = payload.request_id;
        let Some(entry) = self.pending.get(&request_id.0) else {
            env::panic_str(&format!("no pending request {}", request_id.0));
        };
        if entry.payload != payload {
            env::panic_str(&format!("payload does not match request {}", request_id.0));
        }

        // 3. Too late: return instead of panicking, so the removal sticks.
        if env::block_timestamp() >= payload.expires_at.0 {
            self.pending.remove(&request_id.0);
            GuardEvent::RequestExpired { request_id }.emit();
            return PromiseOrValue::Value(RequestOutcome::Expired { request_id });
        }

        // 4. Paused: nothing leaves, and the entry waits.
        require!(!self.paused, ERR_PAUSED);
        let method = payment_method(&payload.token, &payload.msg);
        require_dispatch_gas(method);

        // 5. The policy may have changed since the request.
        if let Some(reason) = self.policy_refusal(&payload) {
            self.pending.remove(&request_id.0);
            GuardEvent::RequestRejected {
                request_id,
                by: approver,
                reason: RejectionReason::Policy(reason),
            }
            .emit();
            return PromiseOrValue::Value(RequestOutcome::Refused { request_id, reason });
        }

        // 6. Single use; counted as approved, not against `daily_max`
        //    (decision 2).
        self.pending.remove(&request_id.0);
        let epoch = self.epoch();
        let state = self.token_state_mut(&payload.token);
        let slot = state.slot_mut(epoch);
        slot.approved = checked(slot.approved.checked_add(payload.amount.0));
        let generation = state.generation;
        let window_spent = state.window_spent(epoch);
        let daily_max = state.policy.daily_max;
        PromiseOrValue::Promise(dispatch(
            payload,
            method,
            Mode::Approved,
            epoch,
            generation,
            window_spent,
            daily_max,
        ))
    }

    /// Removes a pending request. No deposit, like `pause`.
    pub fn reject(&mut self, request_id: U64) {
        let caller = env::predecessor_account_id();
        require!(
            caller == self.owner_id || caller == self.approver_id,
            ERR_NOT_OWNER_OR_APPROVER
        );
        if self.pending.remove(&request_id.0).is_none() {
            env::panic_str(&format!("no pending request {}", request_id.0));
        }
        GuardEvent::RequestRejected {
            request_id,
            by: caller,
            reason: RejectionReason::Rejected,
        }
        .emit();
    }

    /// Anyone may remove expired requests, oldest id first, up to `limit`
    /// (default 16). Returns how many were removed.
    pub fn prune_expired(&mut self, limit: Option<u32>) -> u32 {
        let now = env::block_timestamp();
        let mut expired: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, entry)| now >= entry.payload.expires_at.0)
            .map(|(id, _)| *id)
            .collect();
        expired.sort_unstable();
        expired.truncate(limit.unwrap_or(MAX_PENDING) as usize);
        for &id in &expired {
            self.pending.remove(&id);
            GuardEvent::RequestExpired {
                request_id: U64(id),
            }
            .emit();
        }
        expired.len() as u32
    }

    // ---------- callback ----------

    /// Settles one payment. Reads the transfer's result itself, bounded,
    /// instead of `#[callback_result]` (decision 13): only a plain failure of
    /// a single-receipt transfer gives budget back.
    #[private]
    #[allow(clippy::too_many_arguments)]
    pub fn on_payment_settled(
        &mut self,
        request_id: U64,
        token: Token,
        amount: U128,
        method: Method,
        mode: Mode,
        epoch: U64,
        generation: U64,
    ) -> RequestOutcome {
        let result = env::promise_result_checked(0, MAX_RESULT_BYTES);
        let settled = settle(method, amount.0, result);
        let released = self.release(&token, generation.0, epoch.0, mode, settled.release);
        GuardEvent::PaymentSettled {
            request_id,
            outcome: settled.outcome,
            sent: settled.sent.map(U128),
            released: U128(released),
        }
        .emit();
        match settled.outcome {
            Settlement::Paid => RequestOutcome::Paid {
                request_id,
                sent: U128(settled.sent.unwrap_or(amount.0)),
            },
            Settlement::Failed => RequestOutcome::Failed {
                request_id,
                released: U128(released),
            },
            Settlement::Uncertain => RequestOutcome::Uncertain { request_id },
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
            .map(|(token, state)| self.token_view(token, state))
            .collect()
    }

    pub fn get_token(&self, token: Token) -> Option<TokenView> {
        self.tokens
            .get(&token)
            .map(|state| self.token_view(&token, state))
    }

    /// What the agent can still move without a human; 0 if the token isn't
    /// configured.
    pub fn remaining_budget(&self, token: Token) -> U128 {
        U128(self.tokens.get(&token).map_or(0, |state| {
            state
                .policy
                .daily_max
                .saturating_sub(state.window_spent(self.epoch()))
        }))
    }

    /// Pending requests in id order.
    pub fn get_pending(&self, from_index: Option<u32>, limit: Option<u32>) -> Vec<PendingView> {
        let mut entries: Vec<&PendingEntry> = self.pending.values().collect();
        entries.sort_unstable_by_key(|entry| entry.payload.request_id.0);
        entries
            .into_iter()
            .skip(from_index.unwrap_or(0) as usize)
            .take(limit.unwrap_or(MAX_PENDING) as usize)
            .map(pending_view)
            .collect()
    }

    pub fn get_request(&self, request_id: U64) -> Option<PendingView> {
        self.pending.get(&request_id.0).map(pending_view)
    }

    /// The hash of a payload as this guard computes it, so a client can check
    /// its own implementation.
    pub fn payload_hash(&self, payload: PaymentPayload) -> String {
        hex(&hash_payload(&payload))
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

    fn epoch(&self) -> u64 {
        env::block_timestamp() / self.slot_ns
    }

    /// The refusals `approve` repeats against the current policy (day 5).
    /// Pause is checked by the caller: it refuses a request but makes
    /// `approve` panic.
    fn policy_refusal(&self, payload: &PaymentPayload) -> Option<RefusalReason> {
        let Some(state) = self.tokens.get(&payload.token) else {
            return Some(RefusalReason::TokenNotConfigured);
        };
        let Some(rule) = state.receiver(&payload.receiver_id) else {
            return Some(RefusalReason::ReceiverNotAllowed);
        };
        if payload.msg.is_some() && !rule.ft_call {
            return Some(RefusalReason::TransferCallNotAllowed);
        }
        if payload.amount.0 > state.policy.per_payment_max {
            return Some(RefusalReason::AbovePerPaymentMax);
        }
        None
    }

    /// Stores the full payload for the approver (decision 7). Nothing is
    /// reserved until a human approves.
    fn hold(
        &mut self,
        payload: PaymentPayload,
        reason: PendingReason,
        window_spent: u128,
        daily_max: u128,
        now: u64,
    ) -> PromiseOrValue<RequestOutcome> {
        if self.pending.len() >= MAX_PENDING {
            return refuse(payload, RefusalReason::TooManyPending);
        }
        let request_id = payload.request_id;
        let payload_hash = hash_payload(&payload);
        self.pending.insert(
            request_id.0,
            PendingEntry {
                payload: payload.clone(),
                payload_hash,
                reason,
                created_at: now,
            },
        );
        let payload_hash = hex(&payload_hash);
        GuardEvent::ApprovalRequested {
            request_id,
            reason,
            payload,
            payload_hash: payload_hash.clone(),
            window_spent: U128(window_spent),
            daily_max: U128(daily_max),
        }
        .emit();
        PromiseOrValue::Value(RequestOutcome::Pending {
            request_id,
            reason,
            payload_hash,
        })
    }

    /// Gives back up to `amount` of one payment's reservation and returns how
    /// much it gave back. A no-op unless the token still has the generation
    /// the payment was dispatched under (decision 16) and the payment's slot
    /// still holds that hour; a slot reused for a later hour already dropped
    /// the payment from the window.
    fn release(
        &mut self,
        token: &Token,
        generation: u64,
        epoch: u64,
        mode: Mode,
        amount: u128,
    ) -> u128 {
        // Checked before `get_mut`, which would write the state back even
        // when nothing changes.
        if amount == 0
            || self
                .tokens
                .get(token)
                .is_none_or(|state| state.generation != generation)
        {
            return 0;
        }
        let state = self.token_state_mut(token);
        let Some(slot) = state.slot_at(epoch) else {
            return 0;
        };
        let spent = match mode {
            Mode::Autonomous => &mut slot.autonomous,
            Mode::Approved => &mut slot.approved,
        };
        let released = amount.min(*spent);
        *spent -= released;
        released
    }

    fn token_view(&self, token: &Token, state: &TokenState) -> TokenView {
        let epoch = self.epoch();
        let autonomous_spent = state.window_spent(epoch);
        TokenView {
            token: token.clone(),
            policy: state.policy.into(),
            receivers: state.receivers.clone(),
            autonomous_spent: U128(autonomous_spent),
            approved_spent: U128(state.window_approved(epoch)),
            remaining: U128(state.policy.daily_max.saturating_sub(autonomous_spent)),
            next_release_at: state
                .oldest_spent_epoch(epoch)
                .map(|oldest| U64((oldest + WINDOW_SLOTS as u64).saturating_mul(self.slot_ns))),
        }
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

fn checked<T>(value: Option<T>) -> T {
    value.unwrap_or_else(|| env::panic_str(ERR_OVERFLOW))
}

/// Derived from the token and `msg`. Panics on what `request_payment` must
/// not accept; a stored payload passed these checks already.
fn payment_method(token: &Token, msg: &Option<String>) -> Method {
    match (token, msg) {
        (Token::Native, None) => Method::Native,
        (Token::Native, Some(_)) => env::panic_str(ERR_MSG_NATIVE),
        (Token::Ft(_), None) => Method::FtTransfer,
        (Token::Ft(_), Some(m)) => {
            require!(m.len() <= MAX_MSG_BYTES, ERR_MSG_TOO_LONG);
            Method::FtTransferCall
        }
    }
}

/// Checked before anything is reserved, so a payment can't be counted and
/// then lose its callback for lack of gas.
fn require_dispatch_gas(method: Method) {
    let needed = dispatch_gas(method)
        .saturating_add(GAS_CALLBACK)
        .saturating_add(GAS_MARGIN);
    require!(
        env::prepaid_gas().saturating_sub(env::used_gas()) >= needed,
        ERR_NOT_ENOUGH_GAS
    );
}

fn dispatch_gas(method: Method) -> Gas {
    match method {
        Method::Native => Gas::from_gas(0),
        Method::FtTransfer => GAS_FT_TRANSFER,
        Method::FtTransferCall => GAS_FT_TRANSFER_CALL,
    }
}

fn refuse(payload: PaymentPayload, reason: RefusalReason) -> PromiseOrValue<RequestOutcome> {
    GuardEvent::PaymentRefused {
        request_id: payload.request_id,
        reason,
        token: payload.token,
        receiver_id: payload.receiver_id,
        amount: payload.amount,
    }
    .emit();
    PromiseOrValue::Value(RequestOutcome::Refused {
        request_id: payload.request_id,
        reason,
    })
}

/// Sends the transfer (the guard attaches the 1 yocto NEP-141 asks for) and
/// chains `on_payment_settled` with everything the release needs.
fn dispatch(
    payload: PaymentPayload,
    method: Method,
    mode: Mode,
    epoch: u64,
    generation: u64,
    window_spent: u128,
    daily_max: u128,
) -> Promise {
    let request_id = payload.request_id;
    let transfer = match &payload.token {
        Token::Native => Promise::new(payload.receiver_id.clone())
            .transfer(NearToken::from_yoctonear(payload.amount.0)),
        Token::Ft(contract_id) => {
            let mut args = json!({
                "receiver_id": payload.receiver_id,
                "amount": payload.amount,
                "memo": format!("nsg:{}", request_id.0),
            });
            let function = match &payload.msg {
                Some(msg) => {
                    args["msg"] = json!(msg);
                    "ft_transfer_call"
                }
                None => "ft_transfer",
            };
            Promise::new(contract_id.clone()).function_call(
                function,
                serde_json::to_vec(&args).unwrap_or_else(|_| env::abort()),
                NearToken::from_yoctonear(1),
                dispatch_gas(method),
            )
        }
    };
    let promise = transfer.then(
        Guard::ext(env::current_account_id())
            .with_static_gas(GAS_CALLBACK)
            .on_payment_settled(
                request_id,
                payload.token.clone(),
                payload.amount,
                method,
                mode,
                U64(epoch),
                U64(generation),
            ),
    );
    GuardEvent::PaymentDispatched {
        request_id,
        mode,
        token: payload.token.clone(),
        receiver_id: payload.receiver_id.clone(),
        amount: payload.amount,
        method,
        payload_hash: hex(&hash_payload(&payload)),
        window_spent: U128(window_spent),
        daily_max: U128(daily_max),
    }
    .emit();
    promise
}

/// What a transfer's result means for the ledger (the table in INTERFACE.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settled {
    pub outcome: Settlement,
    /// `None` when it can't be known: `ft_transfer_call` failed after the
    /// tokens may have moved.
    pub sent: Option<u128>,
    pub release: u128,
}

pub(crate) fn settle(
    method: Method,
    amount: u128,
    result: Result<Vec<u8>, PromiseError>,
) -> Settled {
    let paid = |sent: u128| Settled {
        outcome: Settlement::Paid,
        sent: Some(sent),
        release: amount - sent,
    };
    match (method, result) {
        // The resolver returns the used amount as a JSON string. Anything
        // else counts as fully used.
        (Method::FtTransferCall, Ok(bytes)) => {
            paid(serde_json::from_slice::<U128>(&bytes).map_or(amount, |used| used.0.min(amount)))
        }
        (_, Ok(_)) => paid(amount),
        // The tokens moved before the step that failed, or may have
        // (decision 5).
        (Method::FtTransferCall, Err(PromiseError::Failed)) => Settled {
            outcome: Settlement::Uncertain,
            sent: None,
            release: 0,
        },
        // One receipt: its failure means nothing moved.
        (_, Err(PromiseError::Failed)) => Settled {
            outcome: Settlement::Failed,
            sent: Some(0),
            release: amount,
        },
        // `TooLong` is a success with a long result. `PromiseError` is
        // non-exhaustive, so anything new also keeps the reservation.
        (_, Err(_)) => paid(amount),
    }
}

/// `sha256(borsh((PAYLOAD_DOMAIN, guard account id, payload)))`.
pub fn hash_payload(payload: &PaymentPayload) -> [u8; 32] {
    let bytes = near_sdk::borsh::to_vec(&(PAYLOAD_DOMAIN, env::current_account_id(), payload))
        .unwrap_or_else(|_| env::abort());
    env::sha256_array(bytes)
}

fn pending_view(entry: &PendingEntry) -> PendingView {
    PendingView {
        payload: entry.payload.clone(),
        payload_hash: hex(&entry.payload_hash),
        reason: entry.reason,
        created_at: U64(entry.created_at),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
