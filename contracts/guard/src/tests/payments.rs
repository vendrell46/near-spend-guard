//! `request_payment`, the rolling window and the settle callback.
//!
//! A dispatched payment's callback is replayed from the receipt the guard
//! actually created, with a mocked promise result, so each test runs the
//! callback with exactly the arguments the chain would pass.

use near_sdk::mock::{MockAction, Receipt};
use near_sdk::serde::Deserialize;
use near_sdk::test_utils::get_created_receipts;
use near_sdk::{
    Gas, PromiseError, PromiseOrValue, PromiseResult, RuntimeFeesConfig, test_vm_config,
};

use super::*;

mod approvals;

const TEN_MIN_NS: u64 = 600_000_000_000;
const E0: u64 = START_NS / HOUR_NS;

fn bob() -> AccountId {
    acc("bob.testnet")
}
fn tst_contract() -> AccountId {
    acc("tst.nsg-token.testnet")
}

/// `setup()` plus native NEAR (10 per payment, 30 a day, approval above 5)
/// with alice allowed, and bob allowed for `ft_transfer_call` on TST.
fn payments_setup() -> Guard {
    let mut g = setup();
    g.set_token(Token::Native, U128(10), U128(30), U128(5));
    g.allow_receiver(Token::Native, alice(), false);
    g.allow_receiver(tst(), bob(), true);
    g
}

/// TST with no approval threshold below the cap, so only the cap holds
/// payments back.
fn cap_only_setup(per_payment_max: u128, daily_max: u128) -> Guard {
    let mut g = setup();
    g.set_token(
        tst(),
        U128(per_payment_max),
        U128(daily_max),
        U128(per_payment_max),
    );
    g.allow_receiver(tst(), bob(), true);
    g
}

fn request_full(
    g: &mut Guard,
    token: Token,
    receiver: AccountId,
    amount: u128,
    msg: Option<String>,
    expires_at: u64,
) -> PromiseOrValue<RequestOutcome> {
    call_as(&agent(), 0);
    g.request_payment(token, receiver, U128(amount), msg, U64(expires_at))
}

/// `request_payment` from the agent now, expiring in ten minutes.
fn request(
    g: &mut Guard,
    token: Token,
    receiver: AccountId,
    amount: u128,
    msg: Option<&str>,
) -> PromiseOrValue<RequestOutcome> {
    let expires_at = now() + TEN_MIN_NS;
    request_full(
        g,
        token,
        receiver,
        amount,
        msg.map(str::to_string),
        expires_at,
    )
}

fn value(out: PromiseOrValue<RequestOutcome>) -> RequestOutcome {
    match out {
        PromiseOrValue::Value(v) => v,
        PromiseOrValue::Promise(_) => panic!("expected an outcome, got a dispatch"),
    }
}

/// A dispatched payment: its transfer receipt and the callback's arguments
/// exactly as the guard wrote them.
struct Sent {
    transfer: Receipt,
    callback: Value,
    callback_gas: Gas,
}

fn sent(out: PromiseOrValue<RequestOutcome>) -> Sent {
    match out {
        PromiseOrValue::Promise(p) => drop(p),
        PromiseOrValue::Value(v) => panic!("expected a dispatch, got {v:?}"),
    }
    let mut receipts = get_created_receipts();
    assert_eq!(receipts.len(), 2, "{receipts:?}");
    let callback = receipts.pop().unwrap();
    let transfer = receipts.pop().unwrap();
    assert_eq!(callback.receiver_id, guard_id());
    let [
        MockAction::FunctionCallWeight {
            method_name,
            args,
            attached_deposit,
            prepaid_gas,
            ..
        },
    ] = callback.actions.as_slice()
    else {
        panic!("callback receipt: {:?}", callback.actions)
    };
    assert_eq!(method_name, b"on_payment_settled");
    assert_eq!(*attached_deposit, NearToken::from_yoctonear(0));
    Sent {
        transfer,
        callback: serde_json::from_slice(args).unwrap(),
        callback_gas: *prepaid_gas,
    }
}

/// The single function call in a receipt: name, JSON args, deposit, gas.
fn function_call(receipt: &Receipt) -> (String, Value, NearToken, Gas) {
    let [
        MockAction::FunctionCallWeight {
            method_name,
            args,
            attached_deposit,
            prepaid_gas,
            ..
        },
    ] = receipt.actions.as_slice()
    else {
        panic!("not one function call: {:?}", receipt.actions)
    };
    (
        String::from_utf8(method_name.clone()).unwrap(),
        serde_json::from_slice(args).unwrap(),
        *attached_deposit,
        *prepaid_gas,
    )
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct CallbackArgs {
    request_id: U64,
    token: Token,
    amount: U128,
    method: Method,
    mode: Mode,
    epoch: U64,
    generation: U64,
}

/// Runs the callback the guard chained, as the guard, with `result` as the
/// transfer's outcome.
fn settle_with(g: &mut Guard, sent: &Sent, result: PromiseResult) -> RequestOutcome {
    testing_env!(
        context(&guard_id(), 0),
        vm_config(test_vm_config),
        RuntimeFeesConfig::test(),
        Default::default(),
        vec![result]
    );
    let a: CallbackArgs = serde_json::from_value(sent.callback.clone()).unwrap();
    g.on_payment_settled(
        a.request_id,
        a.token,
        a.amount,
        a.method,
        a.mode,
        a.epoch,
        a.generation,
    )
}

fn ok(bytes: &[u8]) -> PromiseResult {
    PromiseResult::Successful(bytes.to_vec())
}

fn remaining(g: &Guard, token: Token) -> u128 {
    call_as(&stranger(), 0);
    g.remaining_budget(token).0
}

fn tst_json() -> Value {
    json!({ "ft": "tst.nsg-token.testnet" })
}

// ---------- dispatch ----------

#[test]
fn ft_transfer_dispatch_reserves_and_chains_the_callback() {
    let mut g = payments_setup();
    let s = sent(request(&mut g, tst(), alice(), 25, None));
    let logs = events();
    let hash = hex(&hash_payload(&PaymentPayload {
        request_id: U64(1),
        token: tst(),
        receiver_id: alice(),
        amount: U128(25),
        msg: None,
        expires_at: U64(START_NS + TEN_MIN_NS),
    }));

    assert_eq!(s.transfer.receiver_id, tst_contract());
    assert_eq!(
        function_call(&s.transfer),
        (
            "ft_transfer".to_string(),
            json!({ "receiver_id": "alice.testnet", "amount": "25", "memo": "nsg:1" }),
            NearToken::from_yoctonear(1),
            GAS_FT_TRANSFER,
        )
    );
    assert_eq!(s.callback_gas, GAS_CALLBACK);
    assert_eq!(
        s.callback,
        json!({
            "request_id": "1", "token": tst_json(), "amount": "25", "method": "ft_transfer",
            "mode": "autonomous", "epoch": E0.to_string(), "generation": "1",
        })
    );
    assert_eq!(
        logs,
        vec![event(
            "payment_dispatched",
            json!({
                "request_id": "1", "mode": "autonomous", "token": tst_json(),
                "receiver_id": "alice.testnet", "amount": "25", "method": "ft_transfer",
                "payload_hash": hash, "window_spent": "25", "daily_max": "100",
            })
        )]
    );
    assert_eq!(remaining(&g, tst()), 75);
    assert_eq!(g.next_request_id, 2);
    assert!(g.pending.is_empty());
}

#[test]
fn native_dispatch_is_a_plain_transfer() {
    let mut g = payments_setup();
    let s = sent(request(&mut g, Token::Native, alice(), 5, None));
    assert_eq!(s.transfer.receiver_id, alice());
    assert!(matches!(
        s.transfer.actions.as_slice(),
        [MockAction::Transfer { deposit, .. }] if *deposit == NearToken::from_yoctonear(5)
    ));
    assert_eq!(s.callback["method"], "native");
    assert_eq!(s.callback["token"], "native");
    assert_eq!(s.callback["generation"], "2");
    assert_eq!(remaining(&g, Token::Native), 25);
    assert_eq!(remaining(&g, tst()), 100, "windows are per token");
}

#[test]
fn ft_transfer_call_dispatch_carries_msg() {
    let mut g = payments_setup();
    let s = sent(request(&mut g, tst(), bob(), 30, Some("order:9")));
    assert_eq!(
        function_call(&s.transfer),
        (
            "ft_transfer_call".to_string(),
            json!({ "receiver_id": "bob.testnet", "amount": "30", "memo": "nsg:1", "msg": "order:9" }),
            NearToken::from_yoctonear(1),
            GAS_FT_TRANSFER_CALL,
        )
    );
    assert_eq!(s.callback["method"], "ft_transfer_call");
    assert_eq!(remaining(&g, tst()), 70);
}

// ---------- step 1: panics that record nothing ----------

/// Request id, TST window and pending count: what a panicking request must
/// leave as it found.
fn ledger(g: &Guard) -> (u64, u128, u32) {
    (g.next_request_id, remaining(g, tst()), g.pending.len())
}

#[test]
fn request_payment_needs_the_agent() {
    for caller in [owner(), approver(), stranger(), guard_id()] {
        let mut g = payments_setup();
        let before = ledger(&g);
        call_as(&caller, 0);
        assert_panics(ERR_NOT_AGENT, || {
            let _ = g.request_payment(tst(), alice(), U128(1), None, U64(now() + TEN_MIN_NS));
        });
        assert!(get_logs().is_empty(), "{caller}");
        assert_eq!(ledger(&g), before, "{caller}");
    }
}

/// Expected panic, token, receiver, amount, msg, expiry ahead of now.
type BadRequest = (&'static str, Token, AccountId, u128, Option<String>, u64);

#[test]
fn malformed_requests_panic_and_record_nothing() {
    let long = "x".repeat(MAX_MSG_BYTES + 1);
    let cases: [BadRequest; 7] = [
        (ERR_ZERO_AMOUNT, tst(), alice(), 0, None, TEN_MIN_NS),
        (
            ERR_RECEIVER_IS_GUARD,
            tst(),
            guard_id(),
            1,
            None,
            TEN_MIN_NS,
        ),
        (ERR_EXPIRED, tst(), alice(), 1, None, 0),
        (
            ERR_EXPIRY_TOO_FAR,
            tst(),
            alice(),
            1,
            None,
            MAX_EXPIRY_NS + 1,
        ),
        (
            ERR_MSG_NATIVE,
            Token::Native,
            alice(),
            1,
            Some(String::new()),
            TEN_MIN_NS,
        ),
        (ERR_MSG_TOO_LONG, tst(), bob(), 1, Some(long), TEN_MIN_NS),
        (ERR_NOT_AGENT, tst(), alice(), 1, None, TEN_MIN_NS),
    ];
    for (i, (expected, token, receiver, amount, msg, ahead)) in cases.into_iter().enumerate() {
        let mut g = payments_setup();
        let before = ledger(&g);
        if expected == ERR_NOT_AGENT {
            // The caller check comes before the input checks: a stranger with
            // bad input learns nothing about the input.
            call_as(&stranger(), 0);
            assert_panics(expected, || {
                let _ = g.request_payment(token, receiver, U128(0), msg, U64(0));
            });
        } else {
            assert_panics(expected, || {
                let _ = request_full(&mut g, token, receiver, amount, msg, now() + ahead);
            });
        }
        assert!(get_logs().is_empty(), "case {i}");
        assert_eq!(ledger(&g), before, "case {i}");
    }
}

#[test]
fn expiry_bounds() {
    let mut g = payments_setup();
    // In the past.
    assert_panics(ERR_EXPIRED, || {
        let _ = request_full(&mut g, tst(), alice(), 1, None, now() - 1);
    });
    // Exactly seven days ahead is allowed; one nanosecond more is not.
    sent(request_full(
        &mut g,
        tst(),
        alice(),
        1,
        None,
        now() + MAX_EXPIRY_NS,
    ));
    // One nanosecond ahead is allowed.
    sent(request_full(&mut g, tst(), alice(), 1, None, now() + 1));
}

#[test]
fn msg_of_exactly_1024_bytes_is_allowed() {
    let mut g = payments_setup();
    let s = sent(request(
        &mut g,
        tst(),
        bob(),
        1,
        Some(&"x".repeat(MAX_MSG_BYTES)),
    ));
    assert_eq!(
        function_call(&s.transfer).1["msg"].as_str().unwrap().len(),
        MAX_MSG_BYTES
    );
}

#[test]
fn msg_length_is_counted_in_bytes() {
    let mut g = payments_setup();
    // 512 two-byte characters fit; 513 don't.
    sent(request(&mut g, tst(), bob(), 1, Some(&"é".repeat(512))));
    assert_panics(ERR_MSG_TOO_LONG, || {
        let _ = request(&mut g, tst(), bob(), 1, Some(&"é".repeat(513)));
    });
}

#[test]
fn request_payment_checks_gas_before_reserving() {
    let cases: [(Token, AccountId, Option<&str>, u64, u64); 3] = [
        // (token, receiver, msg, too little, enough) in TGas
        (Token::Native, alice(), None, 14, 30),
        (tst(), alice(), None, 24, 40),
        (tst(), bob(), Some("m"), 74, 90),
    ];
    for (token, receiver, msg, low, high) in cases {
        let mut g = payments_setup();
        let before = ledger(&g);
        let native_before = remaining(&g, Token::Native);
        for (tgas, enough) in [(low, false), (high, true)] {
            let mut c = context(&agent(), 0);
            c.prepaid_gas = Gas::from_tgas(tgas);
            testing_env!(c, vm_config(test_vm_config));
            let mut call = || {
                g.request_payment(
                    token.clone(),
                    receiver.clone(),
                    U128(1),
                    msg.map(str::to_string),
                    U64(now() + TEN_MIN_NS),
                )
            };
            if enough {
                sent(call());
            } else {
                assert_panics(ERR_NOT_ENOUGH_GAS, || {
                    let _ = call();
                });
                assert_eq!(ledger(&g), before, "{token:?} at {tgas} TGas");
                assert_eq!(remaining(&g, Token::Native), native_before);
            }
        }
    }
}

// ---------- step 3: refusals ----------

#[test]
fn policy_refusals_return_a_reason() {
    let unknown = Token::Ft(acc("other.testnet"));
    let cases: [(Token, AccountId, u128, Option<&str>, RefusalReason); 5] = [
        (
            unknown.clone(),
            alice(),
            1,
            None,
            RefusalReason::TokenNotConfigured,
        ),
        (
            Token::Native,
            bob(),
            1,
            None,
            RefusalReason::ReceiverNotAllowed,
        ),
        (
            tst(),
            alice(),
            1,
            Some("m"),
            RefusalReason::TransferCallNotAllowed,
        ),
        (tst(), alice(), 101, None, RefusalReason::AbovePerPaymentMax),
        (
            tst(),
            bob(),
            101,
            Some("m"),
            RefusalReason::AbovePerPaymentMax,
        ),
    ];
    let mut g = payments_setup();
    for (i, (token, receiver, amount, msg, reason)) in cases.into_iter().enumerate() {
        let id = i as u64 + 1;
        let out = request(&mut g, token.clone(), receiver.clone(), amount, msg);
        assert!(get_created_receipts().is_empty(), "case {i}");
        assert_eq!(
            value(out),
            RequestOutcome::Refused {
                request_id: U64(id),
                reason
            }
        );
        assert_eq!(
            events(),
            vec![event(
                "payment_refused",
                json!({
                    "request_id": id.to_string(),
                    "reason": serde_json::to_value(reason).unwrap(),
                    "token": serde_json::to_value(&token).unwrap(),
                    "receiver_id": receiver,
                    "amount": amount.to_string(),
                })
            )],
            "case {i}"
        );
    }
    // Refusals use up ids and nothing else.
    assert_eq!(g.next_request_id, 6);
    assert_eq!(remaining(&g, tst()), 100);
    assert_eq!(remaining(&g, Token::Native), 30);
    assert!(g.pending.is_empty());
}

#[test]
fn paused_refuses_before_anything_else() {
    let mut g = payments_setup();
    call_as(&approver(), 0);
    g.pause();
    for (token, receiver, amount) in [
        (tst(), alice(), 1),
        (tst(), alice(), 60),
        (Token::Ft(acc("other.testnet")), alice(), 1),
    ] {
        assert!(matches!(
            value(request(&mut g, token, receiver, amount, None)),
            RequestOutcome::Refused {
                reason: RefusalReason::Paused,
                ..
            }
        ));
    }
    assert!(g.pending.is_empty());
    call_as(&owner(), 1);
    g.unpause();
    sent(request(&mut g, tst(), alice(), 1, None));
}

// ---------- step 4: held for a human ----------

#[test]
fn above_threshold_waits_for_a_human() {
    let mut g = payments_setup();
    let out = value(request(&mut g, tst(), alice(), 60, None));
    assert!(get_created_receipts().is_empty());
    let logs = events();
    let payload = PaymentPayload {
        request_id: U64(1),
        token: tst(),
        receiver_id: alice(),
        amount: U128(60),
        msg: None,
        expires_at: U64(START_NS + TEN_MIN_NS),
    };
    let hash = hash_payload(&payload);
    assert_eq!(
        out,
        RequestOutcome::Pending {
            request_id: U64(1),
            reason: PendingReason::AboveThreshold,
            payload_hash: hex(&hash),
        }
    );
    assert_eq!(
        g.pending.get(&1),
        Some(&PendingEntry {
            payload: payload.clone(),
            payload_hash: hash,
            reason: PendingReason::AboveThreshold,
            created_at: START_NS,
        })
    );
    assert_eq!(
        logs,
        vec![event(
            "approval_requested",
            json!({
                "request_id": "1", "reason": "above_threshold",
                "payload": {
                    "request_id": "1", "token": tst_json(), "receiver_id": "alice.testnet",
                    "amount": "60", "msg": null, "expires_at": (START_NS + TEN_MIN_NS).to_string(),
                },
                "payload_hash": hex(&hash), "window_spent": "0", "daily_max": "100",
            })
        )]
    );
    assert_eq!(remaining(&g, tst()), 100, "nothing reserved");
}

#[test]
fn threshold_is_inclusive() {
    let mut g = payments_setup();
    // At the threshold the agent pays alone; one above, it waits.
    sent(request(&mut g, tst(), alice(), 50, None));
    assert!(matches!(
        value(request(&mut g, tst(), alice(), 51, None)),
        RequestOutcome::Pending {
            reason: PendingReason::AboveThreshold,
            ..
        }
    ));
}

#[test]
fn over_budget_becomes_pending() {
    let mut g = payments_setup();
    for _ in 0..4 {
        let s = sent(request(&mut g, tst(), alice(), 25, None));
        settle_with(&mut g, &s, ok(b""));
    }
    assert_eq!(remaining(&g, tst()), 0);
    let out = value(request(&mut g, tst(), alice(), 1, None));
    assert!(matches!(
        out,
        RequestOutcome::Pending {
            reason: PendingReason::DailyCap,
            ..
        }
    ));
    assert_eq!(events()[0]["data"]["window_spent"], "100");
    assert_eq!(g.pending.len(), 1);
}

#[test]
fn above_threshold_wins_over_daily_cap() {
    let mut g = payments_setup();
    for _ in 0..2 {
        sent(request(&mut g, tst(), alice(), 50, None));
    }
    assert!(matches!(
        value(request(&mut g, tst(), alice(), 60, None)),
        RequestOutcome::Pending {
            reason: PendingReason::AboveThreshold,
            ..
        }
    ));
}

/// The race demo in unit form: ten requests of 25 against a cap of 100, all
/// before any callback. Exactly four leave; six wait for a human.
#[test]
fn burst_before_callbacks_stops_at_the_cap() {
    let mut g = payments_setup();
    let mut dispatched = Vec::new();
    let mut held = 0;
    for _ in 0..10 {
        match request(&mut g, tst(), alice(), 25, None) {
            PromiseOrValue::Promise(p) => {
                dispatched.push(sent(PromiseOrValue::Promise(p)));
            }
            PromiseOrValue::Value(RequestOutcome::Pending {
                reason: PendingReason::DailyCap,
                ..
            }) => held += 1,
            PromiseOrValue::Value(other) => panic!("{other:?}"),
        }
    }
    assert_eq!((dispatched.len(), held), (4, 6));
    for s in &dispatched {
        settle_with(&mut g, s, ok(b""));
    }
    assert_eq!(remaining(&g, tst()), 0);
}

#[test]
fn seventeenth_pending_is_refused() {
    let mut g = payments_setup();
    for i in 1..=MAX_PENDING as u64 {
        assert!(matches!(
            value(request(&mut g, tst(), alice(), 60, None)),
            RequestOutcome::Pending { request_id, .. } if request_id == U64(i)
        ));
    }
    assert_eq!(
        value(request(&mut g, tst(), alice(), 60, None)),
        RequestOutcome::Refused {
            request_id: U64(17),
            reason: RefusalReason::TooManyPending
        }
    );
    assert_eq!(events()[0]["event"], "payment_refused");
    assert_eq!(g.pending.len(), MAX_PENDING);
    assert!(g.pending.get(&17).is_none());
    // A payment the agent may make alone still goes through.
    sent(request(&mut g, tst(), alice(), 10, None));
}

// ---------- the rolling window ----------

#[test]
fn bucket_boundary() {
    // A payment in the last nanosecond of a slot counts for exactly 24 hours
    // and one nanosecond.
    let mut g = cap_only_setup(100, 100);
    set_now((E0 + 1) * HOUR_NS - 1);
    let s = sent(request(&mut g, tst(), alice(), 100, None));
    settle_with(&mut g, &s, ok(b""));
    let back_at = (E0 + 25) * HOUR_NS;
    assert_eq!(back_at - now(), 24 * HOUR_NS + 1);

    set_now(back_at - 1);
    assert_eq!(remaining(&g, tst()), 0);
    assert!(matches!(
        value(request(&mut g, tst(), alice(), 1, None)),
        RequestOutcome::Pending {
            reason: PendingReason::DailyCap,
            ..
        }
    ));
    set_now(back_at);
    assert_eq!(remaining(&g, tst()), 100);
    sent(request(&mut g, tst(), alice(), 100, None));
}

#[test]
fn payment_at_slot_start_counts_for_25_hours() {
    let mut g = cap_only_setup(100, 100);
    set_now(E0 * HOUR_NS);
    let s = sent(request(&mut g, tst(), alice(), 100, None));
    settle_with(&mut g, &s, ok(b""));
    set_now((E0 + 25) * HOUR_NS - 1);
    assert_eq!(remaining(&g, tst()), 0);
    set_now((E0 + 25) * HOUR_NS);
    assert_eq!(remaining(&g, tst()), 100);
}

#[test]
fn get_token_reports_the_window() {
    let mut g = payments_setup();
    let view = |g: &Guard| {
        call_as(&stranger(), 0);
        let v = g.get_token(tst()).unwrap();
        (
            v.autonomous_spent.0,
            v.approved_spent.0,
            v.remaining.0,
            v.next_release_at.map(|t| t.0),
        )
    };
    sent(request(&mut g, tst(), alice(), 25, None));
    set_now((E0 + 3) * HOUR_NS + 5);
    sent(request(&mut g, tst(), alice(), 10, None));
    assert_eq!(view(&g), (35, 0, 65, Some((E0 + 25) * HOUR_NS)));
    set_now((E0 + 25) * HOUR_NS);
    assert_eq!(view(&g), (10, 0, 90, Some((E0 + 28) * HOUR_NS)));
    set_now((E0 + 28) * HOUR_NS);
    assert_eq!(view(&g), (0, 0, 100, None));
}

#[test]
fn caps_near_u128_max() {
    let mut g = cap_only_setup(u128::MAX, u128::MAX);
    sent(request(&mut g, tst(), alice(), u128::MAX - 1, None));
    // 2 more would overflow the window sum: held, not a panic.
    assert!(matches!(
        value(request(&mut g, tst(), alice(), 2, None)),
        RequestOutcome::Pending {
            reason: PendingReason::DailyCap,
            ..
        }
    ));
    sent(request(&mut g, tst(), alice(), 1, None));
    assert_eq!(remaining(&g, tst()), 0);
    call_as(&stranger(), 0);
    assert_eq!(
        g.get_token(tst()).unwrap().autonomous_spent,
        U128(u128::MAX)
    );
}

#[test]
fn lowering_the_cap_holds_the_next_payment() {
    let mut g = payments_setup();
    sent(request(&mut g, tst(), alice(), 40, None));
    call_as(&owner(), 1);
    g.set_token(tst(), U128(100), U128(30), U128(50));
    assert_eq!(remaining(&g, tst()), 0);
    assert!(matches!(
        value(request(&mut g, tst(), alice(), 1, None)),
        RequestOutcome::Pending {
            reason: PendingReason::DailyCap,
            ..
        }
    ));
}

// ---------- the callback (decision 13 table) ----------

#[test]
fn settle_table() {
    use Method::*;
    let long = || Err(PromiseError::TooLong(65));
    let failed = || Err(PromiseError::Failed);
    let paid = |sent, release| Settled {
        outcome: Settlement::Paid,
        sent: Some(sent),
        release,
    };
    let cases: Vec<(Method, Result<Vec<u8>, PromiseError>, Settled)> = vec![
        (Native, Ok(vec![]), paid(25, 0)),
        (FtTransfer, Ok(vec![]), paid(25, 0)),
        (FtTransfer, Ok(b"\"3\"".to_vec()), paid(25, 0)),
        (
            Native,
            failed(),
            Settled {
                outcome: Settlement::Failed,
                sent: Some(0),
                release: 25,
            },
        ),
        (
            FtTransfer,
            failed(),
            Settled {
                outcome: Settlement::Failed,
                sent: Some(0),
                release: 25,
            },
        ),
        (Native, long(), paid(25, 0)),
        (FtTransfer, long(), paid(25, 0)),
        (FtTransferCall, Ok(b"\"25\"".to_vec()), paid(25, 0)),
        (FtTransferCall, Ok(b"\"10\"".to_vec()), paid(10, 15)),
        (FtTransferCall, Ok(b"\"0\"".to_vec()), paid(0, 25)),
        // More than the amount is capped at the amount.
        (FtTransferCall, Ok(b"\"30\"".to_vec()), paid(25, 0)),
        // Anything but a JSON string of a u128 counts as fully used.
        (FtTransferCall, Ok(vec![]), paid(25, 0)),
        (FtTransferCall, Ok(b"10".to_vec()), paid(25, 0)),
        (FtTransferCall, Ok(b"null".to_vec()), paid(25, 0)),
        (FtTransferCall, Ok(b"\"-1\"".to_vec()), paid(25, 0)),
        (FtTransferCall, Ok(b"\"abc\"".to_vec()), paid(25, 0)),
        (
            FtTransferCall,
            Ok(b"\"340282366920938463463374607431768211456\"".to_vec()),
            paid(25, 0),
        ),
        (
            FtTransferCall,
            failed(),
            Settled {
                outcome: Settlement::Uncertain,
                sent: None,
                release: 0,
            },
        ),
        (FtTransferCall, long(), paid(25, 0)),
    ];
    for (method, result, expected) in cases {
        let label = format!("{method:?} {result:?}");
        assert_eq!(settle(method, 25, result), expected, "{label}");
    }
}

#[test]
fn ft_transfer_success_keeps_the_reservation() {
    let mut g = payments_setup();
    let s = sent(request(&mut g, tst(), alice(), 25, None));
    assert_eq!(
        settle_with(&mut g, &s, ok(b"")),
        RequestOutcome::Paid {
            request_id: U64(1),
            sent: U128(25)
        }
    );
    assert_eq!(
        events(),
        vec![event(
            "payment_settled",
            json!({ "request_id": "1", "outcome": "paid", "sent": "25", "released": "0" })
        )]
    );
    assert_eq!(remaining(&g, tst()), 75);
}

#[test]
fn unregistered_receiver_releases_budget() {
    // An unregistered receiver makes ft_transfer panic: one receipt, nothing
    // moved, so the budget comes back.
    let mut g = payments_setup();
    let s = sent(request(&mut g, tst(), alice(), 25, None));
    assert_eq!(
        settle_with(&mut g, &s, PromiseResult::Failed),
        RequestOutcome::Failed {
            request_id: U64(1),
            released: U128(25)
        }
    );
    assert_eq!(
        events(),
        vec![event(
            "payment_settled",
            json!({ "request_id": "1", "outcome": "failed", "sent": "0", "released": "25" })
        )]
    );
    assert_eq!(remaining(&g, tst()), 100);
}

#[test]
fn native_to_missing_account_releases() {
    let mut g = payments_setup();
    let s = sent(request(&mut g, Token::Native, alice(), 5, None));
    assert_eq!(
        settle_with(&mut g, &s, PromiseResult::Failed),
        RequestOutcome::Failed {
            request_id: U64(1),
            released: U128(5)
        }
    );
    assert_eq!(remaining(&g, Token::Native), 30);
}

#[test]
fn callback_too_long_result_keeps_reservation() {
    // A valid used amount of 5, zero-padded to `len` bytes.
    let used_5 = |len: usize| {
        let mut bytes = b"\"".to_vec();
        bytes.extend(std::iter::repeat_n(b'0', len - 3));
        bytes.extend(b"5\"");
        assert_eq!(bytes.len(), len);
        bytes
    };
    // The spec's bound is 64 bytes (decision 13), written out rather than
    // read from the constant so a changed constant fails here.
    // One byte too long: not read at all, so not parsed as 5 either.
    for (receiver, msg) in [(alice(), None), (bob(), Some("m"))] {
        let mut g = payments_setup();
        let s = sent(request(&mut g, tst(), receiver, 25, msg));
        assert_eq!(
            settle_with(&mut g, &s, ok(&used_5(65))),
            RequestOutcome::Paid {
                request_id: U64(1),
                sent: U128(25)
            }
        );
        assert_eq!(remaining(&g, tst()), 75, "{msg:?}");
    }
    // Exactly 64 bytes is read and parsed.
    let mut g = payments_setup();
    let s = sent(request(&mut g, tst(), bob(), 25, Some("m")));
    assert_eq!(
        settle_with(&mut g, &s, ok(&used_5(64))),
        RequestOutcome::Paid {
            request_id: U64(1),
            sent: U128(5)
        }
    );
    assert_eq!(remaining(&g, tst()), 95);
}

/// The release never takes a slot below zero, even if the slot holds less
/// than the payment (no public path leads there; the guard is defensive).
#[test]
fn release_saturates() {
    let mut g = payments_setup();
    let s = sent(request(&mut g, tst(), alice(), 25, None));
    let index = slot_index(E0);
    g.tokens.get_mut(&tst()).unwrap().slots[index].autonomous = 10;
    assert_eq!(
        settle_with(&mut g, &s, PromiseResult::Failed),
        RequestOutcome::Failed {
            request_id: U64(1),
            released: U128(10)
        }
    );
    assert_eq!(g.tokens.get(&tst()).unwrap().slots[index].autonomous, 0);
}

#[test]
fn ft_transfer_call_partial_refund() {
    let mut g = payments_setup();
    let s = sent(request(&mut g, tst(), bob(), 25, Some("m")));
    assert_eq!(
        settle_with(&mut g, &s, ok(b"\"10\"")),
        RequestOutcome::Paid {
            request_id: U64(1),
            sent: U128(10)
        }
    );
    assert_eq!(
        events(),
        vec![event(
            "payment_settled",
            json!({ "request_id": "1", "outcome": "paid", "sent": "10", "released": "15" })
        )]
    );
    assert_eq!(remaining(&g, tst()), 90);
}

#[test]
fn ft_transfer_call_failure_keeps_reservation() {
    let mut g = payments_setup();
    let s = sent(request(&mut g, tst(), bob(), 25, Some("m")));
    assert_eq!(
        settle_with(&mut g, &s, PromiseResult::Failed),
        RequestOutcome::Uncertain { request_id: U64(1) }
    );
    assert_eq!(
        events(),
        vec![event(
            "payment_settled",
            json!({ "request_id": "1", "outcome": "uncertain", "sent": null, "released": "0" })
        )]
    );
    assert_eq!(remaining(&g, tst()), 75);
}

#[test]
fn callback_unparseable_transfer_call_counts_as_used() {
    for result in [&b""[..], b"10", b"\"abc\"", b"{\"used\":\"1\"}"] {
        let mut g = payments_setup();
        let s = sent(request(&mut g, tst(), bob(), 25, Some("m")));
        assert_eq!(
            settle_with(&mut g, &s, ok(result)),
            RequestOutcome::Paid {
                request_id: U64(1),
                sent: U128(25)
            }
        );
        assert_eq!(remaining(&g, tst()), 75);
    }
}

#[test]
fn release_only_frees_its_own_payment() {
    let mut g = payments_setup();
    let first = sent(request(&mut g, tst(), alice(), 25, None));
    let second = sent(request(&mut g, tst(), alice(), 30, None));
    settle_with(&mut g, &first, PromiseResult::Failed);
    assert_eq!(remaining(&g, tst()), 70);
    settle_with(&mut g, &second, ok(b""));
    assert_eq!(remaining(&g, tst()), 70);
}

#[test]
fn release_after_slot_rotated_is_noop() {
    let mut g = payments_setup();
    let old = sent(request(&mut g, tst(), alice(), 25, None));
    // 25 slots later the same slot holds a new hour.
    set_now(START_NS + 25 * HOUR_NS);
    sent(request(&mut g, tst(), alice(), 30, None));
    assert_eq!(
        settle_with(&mut g, &old, PromiseResult::Failed),
        RequestOutcome::Failed {
            request_id: U64(1),
            released: U128(0)
        }
    );
    assert_eq!(remaining(&g, tst()), 70);
}

#[test]
fn release_after_token_removed_is_noop() {
    let mut g = payments_setup();
    let s = sent(request(&mut g, tst(), alice(), 25, None));
    call_as(&owner(), 1);
    g.remove_token(tst());
    assert_eq!(
        settle_with(&mut g, &s, PromiseResult::Failed),
        RequestOutcome::Failed {
            request_id: U64(1),
            released: U128(0)
        }
    );
    assert!(g.tokens.get(&tst()).is_none(), "the callback recreated it");
}

#[test]
fn release_after_token_readded_is_noop() {
    // Decision 16: a payment from before remove-and-re-add can't free budget
    // in the new window, even in the same hour.
    let mut g = payments_setup();
    let old = sent(request(&mut g, tst(), alice(), 25, None));
    call_as(&owner(), 1);
    g.remove_token(tst());
    g.set_token(tst(), U128(100), U128(100), U128(50));
    g.allow_receiver(tst(), alice(), false);
    let new = sent(request(&mut g, tst(), alice(), 40, None));
    assert_eq!(new.callback["generation"], "3");
    assert_eq!(old.callback["epoch"], new.callback["epoch"]);
    assert_eq!(
        settle_with(&mut g, &old, PromiseResult::Failed),
        RequestOutcome::Failed {
            request_id: U64(1),
            released: U128(0)
        }
    );
    assert_eq!(remaining(&g, tst()), 60);
}

// ---------- withdraw ----------

#[test]
fn withdraw_is_the_owners_exit() {
    let mut g = payments_setup();
    // Allowed while paused, for any receiver, and not counted in any cap.
    call_as(&approver(), 0);
    g.pause();
    call_as(&owner(), 1);
    drop(g.withdraw(tst(), U128(500), bob()));
    let receipts = get_created_receipts();
    assert_eq!(receipts.len(), 1, "no callback");
    assert_eq!(receipts[0].receiver_id, tst_contract());
    assert_eq!(
        function_call(&receipts[0]),
        (
            "ft_transfer".to_string(),
            json!({ "receiver_id": "bob.testnet", "amount": "500", "memo": "nsg:withdraw" }),
            NearToken::from_yoctonear(1),
            GAS_FT_TRANSFER,
        )
    );
    assert_eq!(
        events(),
        vec![event(
            "withdrawal",
            json!({ "token": tst_json(), "amount": "500", "receiver_id": "bob.testnet" })
        )]
    );
    assert_eq!(remaining(&g, tst()), 100);

    // A token that isn't configured (an airdrop, or one removed) can leave
    // too, and native NEAR is a plain transfer.
    call_as(&owner(), 1);
    drop(g.withdraw(Token::Ft(acc("other.testnet")), U128(1), owner()));
    assert_eq!(get_created_receipts()[0].receiver_id, acc("other.testnet"));
    call_as(&owner(), 1);
    drop(g.withdraw(Token::Native, U128(7), owner()));
    let receipts = get_created_receipts();
    assert_eq!(receipts[0].receiver_id, owner());
    assert!(matches!(
        receipts[0].actions.as_slice(),
        [MockAction::Transfer { deposit, .. }] if *deposit == NearToken::from_yoctonear(7)
    ));
}

#[test]
fn withdraw_refuses_zero_and_the_guard() {
    let mut g = payments_setup();
    call_as(&owner(), 1);
    assert_panics(ERR_ZERO_AMOUNT, || {
        drop(g.withdraw(tst(), U128(0), owner()));
    });
    assert_panics(ERR_RECEIVER_IS_GUARD, || {
        drop(g.withdraw(Token::Native, U128(1), guard_id()));
    });
    assert!(get_logs().is_empty());
}

// ---------- payload hash ----------

/// Vectors computed outside Rust (Python's hashlib over the byte layout in
/// INTERFACE.md), so the Rust encoding is checked against the spec rather
/// than against itself. The TypeScript client checks the same three.
#[test]
fn payload_hash_matches_vectors() {
    let expires_at = U64(1_762_300_800_000_000_000);
    let vectors = [
        (
            PaymentPayload {
                request_id: U64(7),
                token: tst(),
                receiver_id: alice(),
                amount: U128(50_000_000),
                msg: None,
                expires_at,
            },
            "1ec1dd43aca335126c75f72732ed3f5a0c89cf2ee36c192ec3b94d2ecd7b068b",
        ),
        (
            PaymentPayload {
                request_id: U64(8),
                token: tst(),
                receiver_id: bob(),
                amount: U128(1),
                msg: Some("order:9".into()),
                expires_at,
            },
            "6e8fde8b3cd756c635d55e593c5861734811e189802f97c294936546f047694a",
        ),
        (
            PaymentPayload {
                request_id: U64(9),
                token: Token::Native,
                receiver_id: alice(),
                amount: U128(10u128.pow(24)),
                msg: None,
                expires_at,
            },
            "51de72ceed9873fb42f66539cef6901ab38d12f5b74b4424d2340c3615404527",
        ),
    ];
    let g = init();
    for (payload, expected) in vectors {
        assert_eq!(g.payload_hash(payload.clone()), expected);
        assert_eq!(hex(&hash_payload(&payload)), expected);
    }
}

#[test]
fn payload_hash_depends_on_the_guard() {
    let payload = PaymentPayload {
        request_id: U64(7),
        token: tst(),
        receiver_id: alice(),
        amount: U128(50_000_000),
        msg: None,
        expires_at: U64(1),
    };
    call_as(&stranger(), 0);
    let here = hash_payload(&payload);
    let mut c = context(&stranger(), 0);
    c.current_account_id = acc("other-guard.testnet");
    testing_env!(c, vm_config(test_vm_config));
    assert_ne!(hash_payload(&payload), here);
    // `null` and `""` are different payloads.
    let empty = PaymentPayload {
        msg: Some(String::new()),
        ..payload.clone()
    };
    assert_ne!(hash_payload(&empty), hash_payload(&payload));
}

// ---------- property tests (seeded, no extra dependency) ----------

/// xorshift64: deterministic, so a failing seed reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random time steps: often none, often minutes, sometimes hours.
fn advance(rng: &mut Rng) {
    let step = match rng.below(10) {
        0..=3 => 0,
        4..=7 => rng.below(20 * 60 * 1_000_000_000),
        _ => rng.below(4 * HOUR_NS),
    };
    set_now(now() + step);
}

/// The model's window: the sum of counted amounts dispatched in the current
/// slot or the 24 before it.
fn model_spent(live: &[(u64, u128)], epoch: u64) -> u128 {
    live.iter()
        .filter(|(e, _)| *e <= epoch && epoch - e < WINDOW_SLOTS as u64)
        .map(|(_, a)| a)
        .sum()
}

/// Every request is compared with a brute-force model of the window, and at
/// the end no 24-hour span holds more than the cap.
#[test]
fn window_never_exceeds_cap() {
    const CAP: u128 = 100;
    for seed in 1..=64u64 {
        set_now(START_NS);
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut g = cap_only_setup(40, CAP);
        let mut live: Vec<(u64, u128)> = Vec::new();
        let mut paid: Vec<(u64, u128)> = Vec::new();
        for step in 0..150 {
            advance(&mut rng);
            let amount = 1 + rng.below(40) as u128;
            let epoch = now() / HOUR_NS;
            let fits = model_spent(&live, epoch) + amount <= CAP;
            match request(&mut g, tst(), alice(), amount, None) {
                PromiseOrValue::Promise(p) => {
                    assert!(fits, "seed {seed} step {step}: dispatched over the cap");
                    let s = sent(PromiseOrValue::Promise(p));
                    if rng.below(4) == 0 {
                        settle_with(&mut g, &s, PromiseResult::Failed);
                    } else {
                        settle_with(&mut g, &s, ok(b""));
                        live.push((epoch, amount));
                        paid.push((now(), amount));
                    }
                }
                PromiseOrValue::Value(RequestOutcome::Pending {
                    reason: PendingReason::DailyCap,
                    ..
                }) => assert!(!fits, "seed {seed} step {step}: held below the cap"),
                PromiseOrValue::Value(other) => panic!("seed {seed} step {step}: {other:?}"),
            }
            g.pending.clear();
            assert_eq!(
                remaining(&g, tst()),
                CAP - model_spent(&live, now() / HOUR_NS),
                "seed {seed} step {step}"
            );
        }
        for (i, (t, _)) in paid.iter().enumerate() {
            let span: u128 = paid[..=i]
                .iter()
                .filter(|(tj, _)| t - tj <= 24 * HOUR_NS)
                .map(|(_, a)| a)
                .sum();
            assert!(span <= CAP, "seed {seed}: {span} within 24 hours");
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Outcome {
    Empty,
    Failed,
    Used(u128),
    Long,
    Garbage,
}

impl Outcome {
    fn result(self) -> PromiseResult {
        match self {
            Outcome::Empty => ok(b""),
            Outcome::Failed => PromiseResult::Failed,
            Outcome::Used(u) => ok(format!("\"{u}\"").as_bytes()),
            Outcome::Long => ok(&[b'1'; MAX_RESULT_BYTES + 1]),
            Outcome::Garbage => ok(b"{]"),
        }
    }

    /// The table in INTERFACE.md, written out again independently of
    /// `settle`.
    fn model_release(self, method: Method, amount: u128) -> u128 {
        match (method, self) {
            (Method::FtTransferCall, Outcome::Used(u)) => amount - u.min(amount),
            (Method::FtTransferCall, _) => 0,
            (_, Outcome::Failed) => amount,
            _ => 0,
        }
    }
}

/// Requests and callbacks interleave in random order, with every kind of
/// transfer result. After every step the guard's window equals the model's,
/// and the model never holds more than the cap.
#[test]
fn reservation_interleavings() {
    const CAP: u128 = 100;
    for seed in 1..=64u64 {
        set_now(START_NS);
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
        let mut g = cap_only_setup(30, CAP);
        // (sent, method, amount, index into live)
        let mut in_flight: Vec<(Sent, Method, u128, usize)> = Vec::new();
        let mut live: Vec<(u64, u128)> = Vec::new();
        for step in 0..200 {
            advance(&mut rng);
            let epoch = now() / HOUR_NS;
            if in_flight.is_empty() || rng.below(2) == 0 {
                let amount = 1 + rng.below(30) as u128;
                let (receiver, msg, method) = if rng.below(2) == 0 {
                    (alice(), None, Method::FtTransfer)
                } else {
                    (bob(), Some("m"), Method::FtTransferCall)
                };
                let fits = model_spent(&live, epoch) + amount <= CAP;
                match request(&mut g, tst(), receiver, amount, msg) {
                    PromiseOrValue::Promise(p) => {
                        assert!(fits, "seed {seed} step {step}: dispatched over the cap");
                        live.push((epoch, amount));
                        in_flight.push((
                            sent(PromiseOrValue::Promise(p)),
                            method,
                            amount,
                            live.len() - 1,
                        ));
                    }
                    PromiseOrValue::Value(RequestOutcome::Pending { .. }) => {
                        assert!(!fits, "seed {seed} step {step}: held below the cap");
                    }
                    PromiseOrValue::Value(other) => panic!("seed {seed} step {step}: {other:?}"),
                }
                g.pending.clear();
            } else {
                let (s, method, amount, idx) =
                    in_flight.swap_remove(rng.below(in_flight.len() as u64) as usize);
                let outcome = match rng.below(5) {
                    0 => Outcome::Empty,
                    1 => Outcome::Failed,
                    2 => Outcome::Used(rng.below(amount as u64 + 5) as u128),
                    3 => Outcome::Long,
                    _ => Outcome::Garbage,
                };
                settle_with(&mut g, &s, outcome.result());
                live[idx].1 -= outcome.model_release(method, amount);
            }
            let spent = model_spent(&live, now() / HOUR_NS);
            assert!(spent <= CAP, "seed {seed} step {step}: model over the cap");
            assert_eq!(remaining(&g, tst()), CAP - spent, "seed {seed} step {step}");
        }
    }
}
