//! naive-guard: DO NOT USE. The demo's naive guard, written to lose.
//!
//! It is `near-spend-guard` cut down to what the demo runs: the same policy
//! per token (a per-payment ceiling, an approval threshold and a daily cap
//! over the same 25-slot window), the same receiver allowlist, the same
//! refusals, the same limit of 16 pending requests and the same 1 yocto
//! approval from the approver. It differs in exactly two places, each marked
//! `DIFFERENCE` below:
//!
//! 1. A payment is counted only in the transfer's callback, so every request
//!    that lands before the first callback reads the same total (scene 1).
//! 2. `approve` takes a request number, so the approver signs a number and
//!    the guard pays whatever that number points to (scene 2).
//!
//! Left out because the demo doesn't use them: native NEAR,
//! `ft_transfer_call`, pause, reject, withdraw, events, and changing the
//! agent, approver or allowlist after set-up.

use near_sdk::json_types::{U64, U128};
use near_sdk::serde_json::{self, json};
use near_sdk::store::{IterableMap, LookupMap};
use near_sdk::{
    AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseError, PromiseOrValue,
    assert_one_yocto, env, near, require,
};

#[cfg(test)]
mod tests;

pub const WINDOW_SLOTS: u64 = 25;
pub const MAX_PENDING: u32 = 16;
pub const MAX_RECEIVERS: usize = 32;
pub const MAX_EXPIRY_NS: u64 = 7 * 24 * 60 * 60 * 1_000_000_000;
pub const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
pub const GAS_CALLBACK: Gas = Gas::from_tgas(10);
/// The transfer, its callback and a margin, as near-spend-guard checks.
const GAS_NEEDED: Gas = Gas::from_tgas(25);
const ERR_FT_ONLY: &str = "the naive guard pays FT tokens with ft_transfer only";

#[near(serializers = [borsh, json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Token {
    Native,
    Ft(AccountId),
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentPayload {
    pub request_id: U64,
    pub token: Token,
    pub receiver_id: AccountId,
    pub amount: U128,
    pub msg: Option<String>,
    pub expires_at: U64,
}

/// The same JSON as near-spend-guard's outcomes, minus the payload hash.
#[near(serializers = [json])]
#[serde(tag = "status", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestOutcome {
    Refused { request_id: U64, reason: String },
    Pending { request_id: U64, reason: String },
    Paid { request_id: U64, sent: U128 },
    Failed { request_id: U64 },
    Expired { request_id: U64 },
}

#[near(serializers = [borsh])]
pub struct TokenState {
    per_payment_max: u128,
    daily_max: u128,
    approval_above: u128,
    receivers: Vec<AccountId>,
    /// `(epoch, spent)` per slot; `epoch = block_timestamp / slot_ns`.
    slots: [(u64, u128); WINDOW_SLOTS as usize],
}

impl TokenState {
    fn window_spent(&self, epoch: u64) -> u128 {
        self.slots
            .iter()
            .filter(|(e, _)| *e <= epoch && epoch - e < WINDOW_SLOTS)
            .fold(0, |sum, (_, spent)| sum.saturating_add(*spent))
    }
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct NaiveGuard {
    owner_id: AccountId,
    agent_id: AccountId,
    approver_id: AccountId,
    slot_ns: u64,
    next_request_id: u64,
    tokens: LookupMap<Token, TokenState>,
    pending: IterableMap<u64, PaymentPayload>,
}

#[near]
impl NaiveGuard {
    #[init]
    #[private]
    pub fn new(
        owner_id: AccountId,
        agent_id: AccountId,
        approver_id: AccountId,
        slot_ns: U64,
    ) -> Self {
        require!(
            slot_ns.0 >= 1_000_000_000,
            "slot_ns must be at least 1 second"
        );
        require!(
            agent_id != owner_id && agent_id != approver_id,
            "the agent cannot be the owner or the approver"
        );
        Self {
            owner_id,
            agent_id,
            approver_id,
            slot_ns: slot_ns.0,
            next_request_id: 1,
            tokens: LookupMap::new(b"t"),
            pending: IterableMap::new(b"p"),
        }
    }

    #[payable]
    pub fn set_token(
        &mut self,
        token: Token,
        per_payment_max: U128,
        daily_max: U128,
        approval_above: U128,
    ) {
        self.require_owner();
        require!(matches!(token, Token::Ft(_)), ERR_FT_ONLY);
        require!(
            approval_above.0 <= per_payment_max.0,
            "approval_above cannot exceed per_payment_max"
        );
        let state = self.tokens.entry(token).or_insert_with(|| TokenState {
            per_payment_max: 0,
            daily_max: 0,
            approval_above: 0,
            receivers: Vec::new(),
            slots: [(0, 0); WINDOW_SLOTS as usize],
        });
        (state.per_payment_max, state.daily_max, state.approval_above) =
            (per_payment_max.0, daily_max.0, approval_above.0);
    }

    #[payable]
    pub fn allow_receiver(&mut self, token: Token, receiver_id: AccountId, ft_call: bool) {
        self.require_owner();
        require!(!ft_call, ERR_FT_ONLY);
        require!(
            receiver_id != env::current_account_id(),
            "the receiver cannot be the guard"
        );
        require!(
            receiver_id != self.agent_id,
            "the agent cannot be a receiver"
        );
        let state = self
            .tokens
            .get_mut(&token)
            .unwrap_or_else(|| env::panic_str("token not configured"));
        if !state.receivers.contains(&receiver_id) {
            require!(
                state.receivers.len() < MAX_RECEIVERS,
                "at most 32 receivers per token"
            );
            state.receivers.push(receiver_id);
        }
    }

    pub fn request_payment(
        &mut self,
        token: Token,
        receiver_id: AccountId,
        amount: U128,
        msg: Option<String>,
        expires_at: U64,
    ) -> PromiseOrValue<RequestOutcome> {
        require!(
            env::predecessor_account_id() == self.agent_id,
            "only the agent can call this"
        );
        require!(amount.0 > 0, "amount must be above 0");
        require!(
            receiver_id != env::current_account_id(),
            "the receiver cannot be the guard"
        );
        let now = env::block_timestamp();
        require!(expires_at.0 > now, "expires_at must be in the future");
        require!(
            expires_at.0 - now <= MAX_EXPIRY_NS,
            "expires_at can be at most 7 days ahead"
        );
        require!(msg.is_none() && matches!(token, Token::Ft(_)), ERR_FT_ONLY);
        require_gas();
        let request_id = U64(self.next_request_id);
        self.next_request_id += 1;
        let payload = PaymentPayload {
            request_id,
            token,
            receiver_id,
            amount,
            msg,
            expires_at,
        };
        if let Some(reason) = self.policy_refusal(&payload) {
            return refused(request_id, reason);
        }
        let state = &self.tokens[&payload.token];
        let after = state.window_spent(now / self.slot_ns).checked_add(amount.0);
        let hold = if amount.0 > state.approval_above {
            Some("above_threshold")
        } else if after.is_none_or(|total| total > state.daily_max) {
            Some("daily_cap")
        } else {
            None
        };
        if let Some(reason) = hold {
            if self.pending.len() >= MAX_PENDING {
                return refused(request_id, "too_many_pending");
            }
            self.pending.insert(request_id.0, payload);
            return PromiseOrValue::Value(RequestOutcome::Pending {
                request_id,
                reason: reason.into(),
            });
        }
        // DIFFERENCE 1: nothing is counted here. near-spend-guard adds the
        // amount to the window in this receipt, before the transfer leaves.
        PromiseOrValue::Promise(dispatch(payload, true))
    }

    /// DIFFERENCE 2: the approver signs a request number, not the payment.
    /// near-spend-guard takes the full payload and refuses any field that
    /// differs from the stored one.
    #[payable]
    pub fn approve(&mut self, request_id: U64) -> PromiseOrValue<RequestOutcome> {
        assert_one_yocto();
        require!(
            env::predecessor_account_id() == self.approver_id,
            "only the approver can call this"
        );
        let Some(payload) = self.pending.remove(&request_id.0) else {
            env::panic_str(&format!("no pending request {}", request_id.0));
        };
        if env::block_timestamp() >= payload.expires_at.0 {
            return PromiseOrValue::Value(RequestOutcome::Expired { request_id });
        }
        require_gas();
        if let Some(reason) = self.policy_refusal(&payload) {
            return refused(request_id, reason);
        }
        PromiseOrValue::Promise(dispatch(payload, false))
    }

    #[private]
    pub fn on_payment_settled(
        &mut self,
        request_id: U64,
        token: Token,
        amount: U128,
        autonomous: bool,
    ) -> RequestOutcome {
        if let Err(PromiseError::Failed) = env::promise_result_checked(0, 64) {
            return RequestOutcome::Failed { request_id };
        }
        // DIFFERENCE 1, continued: the payment is counted only now, at least
        // two blocks after the request that sent it.
        if autonomous {
            let epoch = env::block_timestamp() / self.slot_ns;
            if let Some(state) = self.tokens.get_mut(&token) {
                let slot = &mut state.slots[(epoch % WINDOW_SLOTS) as usize];
                if slot.0 != epoch {
                    *slot = (epoch, 0);
                }
                slot.1 = slot.1.saturating_add(amount.0);
            }
        }
        RequestOutcome::Paid {
            request_id,
            sent: amount,
        }
    }

    pub fn remaining_budget(&self, token: Token) -> U128 {
        let epoch = env::block_timestamp() / self.slot_ns;
        U128(
            self.tokens
                .get(&token)
                .map_or(0, |s| s.daily_max.saturating_sub(s.window_spent(epoch))),
        )
    }

    pub fn get_request(&self, request_id: U64) -> Option<PaymentPayload> {
        self.pending.get(&request_id.0).cloned()
    }
}

impl NaiveGuard {
    fn require_owner(&self) {
        assert_one_yocto();
        require!(
            env::predecessor_account_id() == self.owner_id,
            "only the owner can call this"
        );
    }

    fn policy_refusal(&self, payload: &PaymentPayload) -> Option<&'static str> {
        let Some(state) = self.tokens.get(&payload.token) else {
            return Some("token_not_configured");
        };
        if !state.receivers.contains(&payload.receiver_id) {
            Some("receiver_not_allowed")
        } else if payload.amount.0 > state.per_payment_max {
            Some("above_per_payment_max")
        } else {
            None
        }
    }
}

fn require_gas() {
    require!(
        env::prepaid_gas().saturating_sub(env::used_gas()) >= GAS_NEEDED,
        "not enough gas for the transfer and its callback"
    );
}

fn refused(request_id: U64, reason: &str) -> PromiseOrValue<RequestOutcome> {
    PromiseOrValue::Value(RequestOutcome::Refused {
        request_id,
        reason: reason.into(),
    })
}

fn dispatch(payload: PaymentPayload, autonomous: bool) -> Promise {
    let Token::Ft(contract_id) = &payload.token else {
        env::panic_str(ERR_FT_ONLY)
    };
    let args = json!({
        "receiver_id": payload.receiver_id,
        "amount": payload.amount,
        "memo": format!("naive:{}", payload.request_id.0),
    });
    Promise::new(contract_id.clone())
        .function_call(
            "ft_transfer",
            serde_json::to_vec(&args).unwrap_or_else(|_| env::abort()),
            NearToken::from_yoctonear(1),
            GAS_FT_TRANSFER,
        )
        .then(
            NaiveGuard::ext(env::current_account_id())
                .with_static_gas(GAS_CALLBACK)
                .on_payment_settled(
                    payload.request_id,
                    payload.token,
                    payload.amount,
                    autonomous,
                ),
        )
}
