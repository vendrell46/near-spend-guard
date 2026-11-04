//! Both demo attacks as deterministic models, and the checks the naive guard
//! shares with near-spend-guard. A dispatched payment's callback is replayed
//! from the receipt the guard created, with a mocked transfer result.

use std::panic::{AssertUnwindSafe, catch_unwind};

use near_sdk::mock::MockAction;
use near_sdk::serde_json::{Value, json};
use near_sdk::test_utils::{VMContextBuilder, get_created_receipts};
use near_sdk::{
    AccountId, NearToken, PromiseResult, RuntimeFeesConfig, VMContext, test_vm_config, testing_env,
};

use super::*;

const HOUR_NS: u64 = 3_600_000_000_000;
const NOW_NS: u64 = 1_762_002_000 * 1_000_000_000;
const TEN_MIN_NS: u64 = 600_000_000_000;

fn acc(id: &str) -> AccountId {
    id.parse().unwrap()
}
fn tst() -> Token {
    Token::Ft(acc("tst.testnet"))
}

fn context(predecessor: &str, yocto: u128) -> VMContext {
    VMContextBuilder::new()
        .current_account_id(acc("naive.testnet"))
        .predecessor_account_id(acc(predecessor))
        .attached_deposit(NearToken::from_yoctonear(yocto))
        .block_timestamp(NOW_NS)
        .build()
}

fn call_as(predecessor: &str, yocto: u128) {
    testing_env!(context(predecessor, yocto));
}

/// The demo's policy (DEMO.md): 200 per payment, a human above 40, 100 a day
/// without one; api allowed.
fn setup() -> NaiveGuard {
    near_sdk::mock::with_mocked_blockchain(|b| b.take_storage());
    call_as("naive.testnet", 0);
    let mut g = NaiveGuard::new(
        acc("owner.testnet"),
        acc("agent.testnet"),
        acc("approver.testnet"),
        U64(HOUR_NS),
    );
    call_as("owner.testnet", 1);
    g.set_token(tst(), U128(200), U128(100), U128(40));
    g.allow_receiver(tst(), acc("api.testnet"), false);
    g
}

fn request(g: &mut NaiveGuard, receiver: &str, amount: u128) -> PromiseOrValue<RequestOutcome> {
    call_as("agent.testnet", 0);
    g.request_payment(
        tst(),
        acc(receiver),
        U128(amount),
        None,
        U64(NOW_NS + TEN_MIN_NS),
    )
}

fn value(out: PromiseOrValue<RequestOutcome>) -> Value {
    match out {
        PromiseOrValue::Value(v) => serde_json::to_value(v).unwrap(),
        PromiseOrValue::Promise(_) => panic!("expected an outcome, got a dispatch"),
    }
}

/// The `ft_transfer` arguments and the callback arguments of a dispatch.
fn sent(out: PromiseOrValue<RequestOutcome>) -> (Value, Value) {
    // The promise becomes receipts when it's dropped.
    match out {
        PromiseOrValue::Promise(p) => drop(p),
        PromiseOrValue::Value(v) => panic!("expected a dispatch, got {v:?}"),
    }
    let receipts = get_created_receipts();
    let args: Vec<(Vec<u8>, Value)> = receipts
        .iter()
        .map(|r| match r.actions.as_slice() {
            [
                MockAction::FunctionCallWeight {
                    method_name, args, ..
                },
            ] => (method_name.clone(), serde_json::from_slice(args).unwrap()),
            other => panic!("not one function call: {other:?}"),
        })
        .collect();
    let [(transfer, transfer_args), (callback, callback_args)] = args.as_slice() else {
        panic!("expected two receipts: {receipts:?}")
    };
    assert_eq!(
        (transfer.as_slice(), callback.as_slice()),
        (&b"ft_transfer"[..], &b"on_payment_settled"[..])
    );
    (transfer_args.clone(), callback_args.clone())
}

fn settle(g: &mut NaiveGuard, callback: &Value, result: PromiseResult) -> RequestOutcome {
    testing_env!(
        context("naive.testnet", 0),
        test_vm_config(),
        RuntimeFeesConfig::test(),
        Default::default(),
        vec![result]
    );
    let a = callback;
    g.on_payment_settled(
        serde_json::from_value(a["request_id"].clone()).unwrap(),
        serde_json::from_value(a["token"].clone()).unwrap(),
        serde_json::from_value(a["amount"].clone()).unwrap(),
        a["autonomous"].as_bool().unwrap(),
    )
}

fn remaining(g: &NaiveGuard) -> u128 {
    call_as("stranger.testnet", 0);
    g.remaining_budget(tst()).0
}

fn approve(g: &mut NaiveGuard, by: &str, yocto: u128, id: u64) -> PromiseOrValue<RequestOutcome> {
    call_as(by, yocto);
    g.approve(U64(id))
}

fn assert_panics(expected: &str, f: impl FnOnce()) {
    let err = catch_unwind(AssertUnwindSafe(f)).expect_err("did not panic");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains(expected),
        "panicked with {msg:?}, expected {expected:?}"
    );
}

/// Scene 1 as a model: ten requests of 25 against a cap of 100, all before
/// any callback, as in one block. Every one reads a window of 0, so all ten
/// leave: 250 against a cap of 100. near-spend-guard's
/// `burst_before_callbacks_stops_at_the_cap` sends four and holds six.
#[test]
fn burst_before_callbacks_overspends() {
    let mut g = setup();
    let callbacks: Vec<Value> = (0..10)
        .map(|_| sent(request(&mut g, "api.testnet", 25)).1)
        .collect();
    assert_eq!(remaining(&g), 100, "nothing counted before the callbacks");
    let sent_total: u128 = callbacks
        .iter()
        .map(
            |c| match settle(&mut g, c, PromiseResult::Successful(vec![])) {
                RequestOutcome::Paid { sent, .. } => sent.0,
                other => panic!("{other:?}"),
            },
        )
        .sum();
    assert_eq!(sent_total, 250);
    assert_eq!(remaining(&g), 0);
}

/// One at a time, the naive guard holds the cap: its only fault in scene 1
/// is the gap between a request and its callback.
#[test]
fn sequential_payments_are_counted() {
    let mut g = setup();
    for _ in 0..4 {
        let (_, callback) = sent(request(&mut g, "api.testnet", 25));
        settle(&mut g, &callback, PromiseResult::Successful(vec![]));
    }
    assert_eq!(remaining(&g), 0);
    assert_eq!(
        value(request(&mut g, "api.testnet", 25)),
        json!({ "status": "pending", "request_id": "5", "reason": "daily_cap" })
    );
}

#[test]
fn failed_transfer_is_not_counted() {
    let mut g = setup();
    let (_, callback) = sent(request(&mut g, "api.testnet", 25));
    assert_eq!(
        settle(&mut g, &callback, PromiseResult::Failed),
        RequestOutcome::Failed { request_id: U64(1) }
    );
    assert_eq!(remaining(&g), 100);
}

/// Scene 2 as a model: the human agreed to 50, the filed request says 190.
/// The approver signs only the number, so the guard pays 190.
#[test]
fn approve_by_number_pays_what_is_stored() {
    let mut g = setup();
    assert_eq!(
        value(request(&mut g, "api.testnet", 190)),
        json!({ "status": "pending", "request_id": "1", "reason": "above_threshold" })
    );
    let (transfer, callback) = sent(approve(&mut g, "approver.testnet", 1, 1));
    assert_eq!(
        transfer,
        json!({ "receiver_id": "api.testnet", "amount": "190", "memo": "naive:1" })
    );
    assert_eq!(callback["autonomous"], false);
    assert_panics("no pending request 1", || {
        let _ = approve(&mut g, "approver.testnet", 1, 1);
    });
    settle(&mut g, &callback, PromiseResult::Successful(vec![]));
    assert_eq!(
        remaining(&g),
        100,
        "approved payments don't count against daily_max"
    );
}

#[test]
fn same_refusals_as_near_spend_guard() {
    let mut g = setup();
    let refused = |g: &mut NaiveGuard, receiver: &str, amount: u128| {
        value(request(g, receiver, amount))["reason"].clone()
    };
    assert_eq!(
        refused(&mut g, "mallory.testnet", 5),
        "receiver_not_allowed"
    );
    assert_eq!(refused(&mut g, "api.testnet", 201), "above_per_payment_max");
    assert_eq!(refused(&mut g, "api.testnet", 41), "above_threshold");
    for _ in 1..MAX_PENDING {
        let _ = request(&mut g, "api.testnet", 41);
    }
    assert_eq!(refused(&mut g, "api.testnet", 41), "too_many_pending");
    call_as("agent.testnet", 0);
    let other = g.request_payment(
        Token::Ft(acc("other.testnet")),
        acc("api.testnet"),
        U128(5),
        None,
        U64(NOW_NS + 1),
    );
    assert_eq!(value(other)["reason"], "token_not_configured");
}

#[test]
fn malformed_requests_panic() {
    let mut g = setup();
    assert_panics("only the agent", || {
        call_as("stranger.testnet", 0);
        let _ = g.request_payment(tst(), acc("api.testnet"), U128(5), None, U64(NOW_NS + 1));
    });
    let mut bad = |amount: u128, expires_at: u64, msg: Option<String>| {
        call_as("agent.testnet", 0);
        let _ = g.request_payment(
            tst(),
            acc("api.testnet"),
            U128(amount),
            msg,
            U64(expires_at),
        );
    };
    assert_panics("amount must be above 0", || bad(0, NOW_NS + 1, None));
    assert_panics("in the future", || bad(5, NOW_NS, None));
    assert_panics("at most 7 days", || {
        bad(5, NOW_NS + MAX_EXPIRY_NS + 1, None)
    });
    assert_panics(ERR_FT_ONLY, || bad(5, NOW_NS + 1, Some("x".into())));
}

#[test]
fn approve_needs_the_approver_and_one_yocto() {
    let mut g = setup();
    let _ = request(&mut g, "api.testnet", 190);
    assert_panics("exactly 1 yoctoNEAR", || {
        let _ = approve(&mut g, "approver.testnet", 0, 1);
    });
    assert_panics("only the approver", || {
        let _ = approve(&mut g, "agent.testnet", 1, 1);
    });
    assert!(g.get_request(U64(1)).is_some());
}

#[test]
fn approve_rechecks_the_policy() {
    let mut g = setup();
    let _ = request(&mut g, "api.testnet", 190);
    call_as("owner.testnet", 1);
    g.set_token(tst(), U128(100), U128(100), U128(40));
    assert_eq!(
        value(approve(&mut g, "approver.testnet", 1, 1)),
        json!({ "status": "refused", "request_id": "1", "reason": "above_per_payment_max" })
    );
    assert!(g.get_request(U64(1)).is_none());
}

#[test]
fn owner_calls_need_the_owner_and_one_yocto() {
    let mut g = setup();
    assert_panics("exactly 1 yoctoNEAR", || {
        call_as("owner.testnet", 0);
        g.allow_receiver(tst(), acc("bob.testnet"), false);
    });
    assert_panics("only the owner", || {
        call_as("agent.testnet", 1);
        g.set_token(tst(), U128(1000), U128(1000), U128(1000));
    });
    assert_panics("the agent cannot be a receiver", || {
        call_as("owner.testnet", 1);
        g.allow_receiver(tst(), acc("agent.testnet"), false);
    });
}
