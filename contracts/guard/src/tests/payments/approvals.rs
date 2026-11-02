//! `approve`, `reject`, `prune_expired`, the pending views, and decision 12
//! (changing the agent cancels its pending requests).

use super::*;

/// Files a request for `amount` TST to alice that waits for a human, and
/// returns the payload the guard stored.
fn pending(g: &mut Guard, amount: u128) -> PaymentPayload {
    let out = value(request(g, tst(), alice(), amount, None));
    let RequestOutcome::Pending { request_id, .. } = out else {
        panic!("expected pending, got {out:?}")
    };
    g.pending.get(&request_id.0).unwrap().payload.clone()
}

fn approve_as(
    g: &mut Guard,
    caller: &AccountId,
    yocto: u128,
    payload: PaymentPayload,
) -> PromiseOrValue<RequestOutcome> {
    call_as(caller, yocto);
    g.approve(payload)
}

fn approve(g: &mut Guard, payload: PaymentPayload) -> PromiseOrValue<RequestOutcome> {
    approve_as(g, &approver(), 1, payload)
}

/// (autonomous spent, approved spent, remaining) for TST.
fn window(g: &Guard) -> (u128, u128, u128) {
    call_as(&stranger(), 0);
    let v = g.get_token(tst()).unwrap();
    (v.autonomous_spent.0, v.approved_spent.0, v.remaining.0)
}

fn pending_ids(g: &Guard) -> Vec<u64> {
    call_as(&stranger(), 0);
    g.get_pending(None, None)
        .iter()
        .map(|v| v.payload.request_id.0)
        .collect()
}

// ---------- approve ----------

#[test]
fn approve_dispatches_exactly_the_stored_payload() {
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    let s = sent(approve(&mut g, p.clone()));
    let logs = events();
    let hash = hex(&hash_payload(&p));
    assert_eq!(
        function_call(&s.transfer),
        (
            "ft_transfer".to_string(),
            json!({ "receiver_id": "alice.testnet", "amount": "60", "memo": "nsg:1" }),
            NearToken::from_yoctonear(1),
            GAS_FT_TRANSFER,
        )
    );
    assert_eq!(s.callback["mode"], "approved");
    assert_eq!(
        logs,
        vec![event(
            "payment_dispatched",
            json!({
                "request_id": "1", "mode": "approved", "token": tst_json(),
                "receiver_id": "alice.testnet", "amount": "60", "method": "ft_transfer",
                "payload_hash": hash, "window_spent": "0", "daily_max": "100",
            })
        )]
    );
    assert!(g.pending.is_empty());
    assert_eq!(window(&g), (0, 60, 100));
    assert_eq!(
        settle_with(&mut g, &s, ok(b"")),
        RequestOutcome::Paid {
            request_id: U64(1),
            sent: U128(60)
        }
    );
    assert_eq!(window(&g), (0, 60, 100));
}

#[test]
fn approve_needs_one_yocto_then_the_approver() {
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    for yocto in [0, 2] {
        assert_panics(ERR_ONE_YOCTO, || {
            let _ = approve_as(&mut g, &approver(), yocto, p.clone());
        });
    }
    for caller in [owner(), agent(), stranger(), guard_id()] {
        assert_panics(ERR_NOT_APPROVER, || {
            let _ = approve_as(&mut g, &caller, 1, p.clone());
        });
    }
    assert_eq!(pending_ids(&g), vec![1]);
    assert_eq!(window(&g), (0, 0, 100));
}

#[test]
fn approval_requires_exact_payload() {
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    let changed = [
        PaymentPayload {
            token: Token::Native,
            ..p.clone()
        },
        PaymentPayload {
            receiver_id: bob(),
            ..p.clone()
        },
        PaymentPayload {
            amount: U128(59),
            ..p.clone()
        },
        PaymentPayload {
            amount: U128(61),
            ..p.clone()
        },
        PaymentPayload {
            msg: Some(String::new()),
            ..p.clone()
        },
        PaymentPayload {
            expires_at: U64(p.expires_at.0 + 1),
            ..p.clone()
        },
    ];
    for (i, payload) in changed.into_iter().enumerate() {
        assert_panics("payload does not match request 1", || {
            let _ = approve(&mut g, payload);
        });
        assert!(get_created_receipts().is_empty(), "field {i}");
        assert_eq!(pending_ids(&g), vec![1], "field {i}");
    }
    assert_panics("no pending request 2", || {
        let _ = approve(
            &mut g,
            PaymentPayload {
                request_id: U64(2),
                ..p.clone()
            },
        );
    });
    sent(approve(&mut g, p));
}

/// Demo scene 2: the provider quoted 50, the filed request says 190. An
/// approval of what the human agreed to is refused; what the guard stores,
/// and would execute, is the 190.
#[test]
fn payload_swap_guard_refuses() {
    let mut g = payments_setup();
    call_as(&owner(), 1);
    g.set_token(tst(), U128(200), U128(100), U128(50));
    let filed = pending(&mut g, 190);
    let quoted = PaymentPayload {
        amount: U128(50),
        ..filed.clone()
    };
    assert_panics("payload does not match request 1", || {
        let _ = approve(&mut g, quoted);
    });
    call_as(&stranger(), 0);
    assert_eq!(g.get_request(U64(1)).unwrap().payload.amount, U128(190));
    assert_eq!(window(&g), (0, 0, 100));
}

#[test]
fn approval_single_use() {
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    sent(approve(&mut g, p.clone()));
    assert_panics("no pending request 1", || {
        let _ = approve(&mut g, p);
    });
    assert_eq!(window(&g), (0, 60, 100));
}

#[test]
fn approval_at_exact_expiry() {
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    set_now(p.expires_at.0);
    assert_eq!(
        value(approve(&mut g, p.clone())),
        RequestOutcome::Expired { request_id: U64(1) }
    );
    assert!(get_created_receipts().is_empty());
    assert_eq!(
        events(),
        vec![event("request_expired", json!({ "request_id": "1" }))]
    );
    assert!(g.pending.is_empty());

    // One nanosecond earlier it is still valid.
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    set_now(p.expires_at.0 - 1);
    sent(approve(&mut g, p));
}

#[test]
fn approve_when_paused_keeps_the_entry() {
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    call_as(&approver(), 0);
    g.pause();
    assert_panics(ERR_PAUSED, || {
        let _ = approve(&mut g, p.clone());
    });
    assert_eq!(pending_ids(&g), vec![1]);
    call_as(&owner(), 1);
    g.unpause();
    sent(approve(&mut g, p));
}

#[test]
fn expiry_wins_over_pause() {
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    call_as(&approver(), 0);
    g.pause();
    set_now(p.expires_at.0);
    assert_eq!(
        value(approve(&mut g, p)),
        RequestOutcome::Expired { request_id: U64(1) }
    );
    assert!(g.pending.is_empty());
}

#[test]
fn approve_after_disallow_refuses_and_removes() {
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    call_as(&owner(), 1);
    g.disallow_receiver(tst(), alice());
    assert_eq!(
        value(approve(&mut g, p)),
        RequestOutcome::Refused {
            request_id: U64(1),
            reason: RefusalReason::ReceiverNotAllowed
        }
    );
    assert!(get_created_receipts().is_empty());
    assert_eq!(
        events(),
        vec![event(
            "request_rejected",
            json!({ "request_id": "1", "by": "approver.testnet", "reason": "receiver_not_allowed" })
        )]
    );
    assert!(g.pending.is_empty());
    assert_eq!(window(&g), (0, 0, 100));
}

/// An owner call made after the request, and the refusal it causes at
/// `approve`.
type PolicyChange = (fn(&mut Guard), RefusalReason);

#[test]
fn approval_rechecks_the_current_policy() {
    let cases: [PolicyChange; 3] = [
        (
            |g| g.set_token(tst(), U128(59), U128(100), U128(50)),
            RefusalReason::AbovePerPaymentMax,
        ),
        (|g| g.remove_token(tst()), RefusalReason::TokenNotConfigured),
        (
            |g| g.allow_receiver(tst(), bob(), false),
            RefusalReason::TransferCallNotAllowed,
        ),
    ];
    for (change, reason) in cases {
        let mut g = payments_setup();
        let p = if reason == RefusalReason::TransferCallNotAllowed {
            let out = value(request(&mut g, tst(), bob(), 60, Some("m")));
            assert!(matches!(out, RequestOutcome::Pending { .. }), "{out:?}");
            g.pending.get(&1).unwrap().payload.clone()
        } else {
            pending(&mut g, 60)
        };
        call_as(&owner(), 1);
        change(&mut g);
        assert_eq!(
            value(approve(&mut g, p)),
            RequestOutcome::Refused {
                request_id: U64(1),
                reason
            }
        );
        assert_eq!(events()[0]["data"]["reason"], json!(reason));
        assert!(g.pending.is_empty(), "{reason:?}");
    }
}

#[test]
fn approved_payment_is_not_counted_against_daily_max() {
    let mut g = payments_setup();
    for _ in 0..4 {
        sent(request(&mut g, tst(), alice(), 25, None));
    }
    let p = pending(&mut g, 25);
    assert_eq!(g.pending.get(&5).unwrap().reason, PendingReason::DailyCap);
    let s = sent(approve(&mut g, p));
    assert_eq!(s.callback["mode"], "approved");
    assert_eq!(window(&g), (100, 25, 0));
    // Above the threshold too: approvals never touch the autonomous window.
    let p = pending(&mut g, 60);
    sent(approve(&mut g, p));
    assert_eq!(window(&g), (100, 85, 0));
}

#[test]
fn approved_failure_releases_from_approved() {
    let mut g = payments_setup();
    sent(request(&mut g, tst(), alice(), 30, None));
    let p = pending(&mut g, 60);
    let s = sent(approve(&mut g, p));
    assert_eq!(window(&g), (30, 60, 70));
    assert_eq!(
        settle_with(&mut g, &s, PromiseResult::Failed),
        RequestOutcome::Failed {
            request_id: U64(2),
            released: U128(60)
        }
    );
    assert_eq!(window(&g), (30, 0, 70));
}

#[test]
fn approve_checks_gas() {
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    let mut c = context(&approver(), 1);
    c.prepaid_gas = Gas::from_tgas(20);
    testing_env!(c, vm_config(test_vm_config));
    assert_panics(ERR_NOT_ENOUGH_GAS, || {
        let _ = g.approve(p.clone());
    });
    assert_eq!(pending_ids(&g), vec![1]);
    sent(approve(&mut g, p));
}

// ---------- reject ----------

#[test]
fn reject_by_approver_or_owner() {
    let mut g = payments_setup();
    pending(&mut g, 60);
    pending(&mut g, 70);
    for caller in [agent(), stranger(), guard_id()] {
        call_as(&caller, 0);
        assert_panics(ERR_NOT_OWNER_OR_APPROVER, || g.reject(U64(1)));
    }
    assert_eq!(pending_ids(&g), vec![1, 2]);

    call_as(&approver(), 0);
    g.reject(U64(1));
    assert_eq!(
        events(),
        vec![event(
            "request_rejected",
            json!({ "request_id": "1", "by": "approver.testnet", "reason": "rejected" })
        )]
    );
    call_as(&owner(), 0);
    g.reject(U64(2));
    assert_eq!(events()[0]["data"]["by"], "owner.testnet");
    assert!(g.pending.is_empty());

    call_as(&owner(), 0);
    assert_panics("no pending request 2", || g.reject(U64(2)));
    assert_panics("no pending request 9", || g.reject(U64(9)));
}

// ---------- prune_expired ----------

#[test]
fn prune_expired_removes_only_expired() {
    let mut g = payments_setup();
    let minute = 60_000_000_000;
    for ahead in [minute, 5 * minute, 10 * minute] {
        let expires_at = now() + ahead;
        value(request_full(&mut g, tst(), alice(), 60, None, expires_at));
    }
    // At exactly the second expiry, the first two have expired.
    set_now(START_NS + 5 * minute);
    call_as(&stranger(), 0);
    assert_eq!(g.prune_expired(None), 2);
    assert_eq!(
        events(),
        vec![
            event("request_expired", json!({ "request_id": "1" })),
            event("request_expired", json!({ "request_id": "2" })),
        ]
    );
    assert_eq!(pending_ids(&g), vec![3]);
    call_as(&stranger(), 0);
    assert_eq!(g.prune_expired(None), 0);
    assert!(get_logs().is_empty());
}

#[test]
fn prune_expired_takes_the_oldest_ids_first() {
    let mut g = payments_setup();
    for _ in 0..4 {
        pending(&mut g, 60);
    }
    // Removing the first entry moves the last one into its place, so the
    // map no longer iterates in id order.
    call_as(&approver(), 0);
    g.reject(U64(1));
    set_now(START_NS + TEN_MIN_NS);
    call_as(&stranger(), 0);
    assert_eq!(g.prune_expired(Some(0)), 0);
    assert_eq!(g.prune_expired(Some(1)), 1);
    assert_eq!(
        events(),
        vec![event("request_expired", json!({ "request_id": "2" }))]
    );
    assert_eq!(pending_ids(&g), vec![3, 4]);
}

// ---------- decision 12 and set_approver ----------

#[test]
fn set_agent_cancels_pending() {
    let mut g = payments_setup();
    for amount in [60, 70, 80] {
        pending(&mut g, amount);
    }
    call_as(&owner(), 1);
    g.set_agent(acc("agent2.testnet"));
    let rejected = |id: &str| {
        event(
            "request_rejected",
            json!({ "request_id": id, "by": "owner.testnet", "reason": "agent_changed" }),
        )
    };
    assert_eq!(
        events(),
        vec![
            event(
                "config_changed",
                json!({ "change": "agent", "token": null, "account_id": "agent2.testnet" })
            ),
            rejected("1"),
            rejected("2"),
            rejected("3"),
        ]
    );
    assert!(g.pending.is_empty());
    // The old agent is locked out; the new one can pay.
    assert_panics(ERR_NOT_AGENT, || {
        let _ = request(&mut g, tst(), alice(), 1, None);
    });
    call_as(&acc("agent2.testnet"), 0);
    sent(g.request_payment(tst(), alice(), U128(1), None, U64(now() + TEN_MIN_NS)));
}

#[test]
fn set_approver_keeps_pending() {
    let mut g = payments_setup();
    let p = pending(&mut g, 60);
    call_as(&owner(), 1);
    g.set_approver(acc("approver2.testnet"));
    assert_eq!(pending_ids(&g), vec![1]);
    assert_panics(ERR_NOT_APPROVER, || {
        let _ = approve(&mut g, p.clone());
    });
    sent(approve_as(&mut g, &acc("approver2.testnet"), 1, p));
}

// ---------- views ----------

#[test]
fn get_pending_and_get_request() {
    let mut g = payments_setup();
    let first = pending(&mut g, 60);
    pending(&mut g, 70);
    sent(request(&mut g, tst(), alice(), 10, None));
    pending(&mut g, 80);
    pending(&mut g, 90);
    call_as(&approver(), 0);
    g.reject(U64(1));

    // Id order, whatever order the map keeps.
    assert_eq!(pending_ids(&g), vec![2, 4, 5]);
    call_as(&stranger(), 0);
    let page: Vec<u64> = g
        .get_pending(Some(1), Some(1))
        .iter()
        .map(|v| v.payload.request_id.0)
        .collect();
    assert_eq!(page, vec![4]);
    assert!(g.get_pending(Some(3), None).is_empty());
    assert_eq!(g.get_request(U64(1)), None);
    assert_eq!(g.get_request(U64(3)), None, "dispatched, never pending");

    let second = PaymentPayload {
        request_id: U64(2),
        amount: U128(70),
        ..first
    };
    assert_eq!(
        serde_json::to_value(g.get_request(U64(2))).unwrap(),
        json!({
            "payload": {
                "request_id": "2", "token": tst_json(), "receiver_id": "alice.testnet",
                "amount": "70", "msg": null, "expires_at": (START_NS + TEN_MIN_NS).to_string(),
            },
            "payload_hash": hex(&hash_payload(&second)),
            "reason": "above_threshold",
            "created_at": START_NS.to_string(),
        })
    );
}
