//! How a payment settles against a real NEP-141 token, including the ways a
//! token or a receiver can fail or answer oddly. The unit tests replay these
//! results by hand; here the test token produces them (decisions 5 and 13).

mod common;

use common::*;
use near_workspaces::AccountId;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use serde_json::{Value, json};

async fn set_switch(env: &Env, switch: &str) -> anyhow::Result<()> {
    env.owner
        .call(env.tst.id(), "set_switch")
        .args_json(json!({ "switch": switch }))
        .transact()
        .await?
        .into_result()?;
    Ok(())
}

fn settled(logs: Vec<&str>) -> Value {
    guard_events(logs)
        .into_iter()
        .find(|e| e["event"] == "payment_settled")
        .expect("a payment_settled event")["data"]
        .clone()
}

const GUARD_HOLDS: u128 = 1_000 * TST;

#[tokio::test]
async fn unregistered_receiver_releases_budget() -> anyhow::Result<()> {
    let env = setup().await?;
    let root = env.worker.root_account()?;
    // Allowed by the guard, but never registered with the token, so
    // `ft_transfer` panics and nothing moves. The guard doesn't pay for the
    // receiver's storage.
    let bob = subaccount(&root, "bob", 5).await?;
    allow(&env, bob.id(), false).await?;

    let result = request(&env, bob.id(), 25 * TST, None).await?;
    assert_eq!(
        result.json::<Value>()?,
        json!({ "status": "failed", "request_id": "1", "released": (25 * TST).to_string() })
    );
    assert_eq!(remaining(&env).await?, DAILY_MAX);
    assert_eq!(balance(&env, env.guard.id()).await?, GUARD_HOLDS);
    Ok(())
}

#[tokio::test]
async fn failed_ft_transfer_releases_budget() -> anyhow::Result<()> {
    let env = setup().await?;
    set_switch(&env, "fail_transfer").await?;
    let result = request(&env, env.alice.id(), 25 * TST, None).await?;
    let logs = result.logs();
    assert_eq!(
        settled(logs),
        json!({ "request_id": "1", "outcome": "failed", "sent": "0", "released": (25 * TST).to_string() })
    );
    assert_eq!(remaining(&env).await?, DAILY_MAX);
    assert_eq!(balance(&env, env.alice.id()).await?, 0);
    Ok(())
}

#[tokio::test]
async fn ft_transfer_call_partial_refund() -> anyhow::Result<()> {
    let env = setup().await?;
    allow(&env, env.shop.id(), true).await?;
    // The shop uses 15 of 25 and hands 10 back: 15 counts.
    let result = request(&env, env.shop.id(), 25 * TST, Some("refund:10000000")).await?;
    assert_eq!(
        result.json::<Value>()?,
        json!({ "status": "paid", "request_id": "1", "sent": (15 * TST).to_string() })
    );
    assert_eq!(remaining(&env).await?, DAILY_MAX - 15 * TST);
    assert_eq!(balance(&env, env.shop.id()).await?, 15 * TST);
    assert_eq!(balance(&env, env.guard.id()).await?, GUARD_HOLDS - 15 * TST);
    Ok(())
}

#[tokio::test]
async fn ft_transfer_call_receiver_failure_refunds_everything() -> anyhow::Result<()> {
    let env = setup().await?;
    allow(&env, env.shop.id(), true).await?;
    // The shop's `ft_on_transfer` panics, so the token refunds all of it and
    // reports 0 used: nothing counts.
    let result = request(&env, env.shop.id(), 25 * TST, Some("fail")).await?;
    assert_eq!(
        result.json::<Value>()?,
        json!({ "status": "paid", "request_id": "1", "sent": "0" })
    );
    assert_eq!(remaining(&env).await?, DAILY_MAX);
    assert_eq!(balance(&env, env.guard.id()).await?, GUARD_HOLDS);
    Ok(())
}

#[tokio::test]
async fn ft_transfer_call_failure_keeps_reservation() -> anyhow::Result<()> {
    let env = setup().await?;
    allow(&env, env.shop.id(), true).await?;
    // The token's resolve step fails after the 25 already moved. The guard
    // sees a failure, but releasing would let the cap leak by 25
    // (decision 5).
    set_switch(&env, "fail_resolve").await?;
    let result = request(&env, env.shop.id(), 25 * TST, Some("keep")).await?;
    let logs = result.logs();
    assert_eq!(
        settled(logs),
        json!({ "request_id": "1", "outcome": "uncertain", "sent": null, "released": "0" })
    );
    assert_eq!(
        result.json::<Value>()?,
        json!({ "status": "uncertain", "request_id": "1" })
    );
    assert_eq!(balance(&env, env.shop.id()).await?, 25 * TST);
    assert_eq!(remaining(&env).await?, DAILY_MAX - 25 * TST);
    Ok(())
}

#[tokio::test]
async fn callback_too_long_result_keeps_reservation() -> anyhow::Result<()> {
    let env = setup().await?;
    allow(&env, env.shop.id(), true).await?;
    set_switch(&env, "long_result").await?;

    // `ft_transfer` moved 25 and then answered with 70 bytes. Over the
    // 64-byte bound: counted as paid, never as failed.
    let plain = request(&env, env.alice.id(), 25 * TST, None).await?;
    assert_eq!(
        plain.json::<Value>()?,
        json!({ "status": "paid", "request_id": "1", "sent": (25 * TST).to_string() })
    );
    assert_eq!(balance(&env, env.alice.id()).await?, 25 * TST);

    // `ft_transfer_call`: the shop handed 10 back, but the resolve step's
    // answer is too long to read, so all 25 stay counted. Fails closed:
    // budget lost, never funds.
    let call = request(&env, env.shop.id(), 25 * TST, Some("refund:10000000")).await?;
    assert_eq!(
        call.json::<Value>()?,
        json!({ "status": "paid", "request_id": "2", "sent": (25 * TST).to_string() })
    );
    assert_eq!(balance(&env, env.shop.id()).await?, 15 * TST);
    assert_eq!(remaining(&env).await?, DAILY_MAX - 50 * TST);
    Ok(())
}

#[tokio::test]
async fn callback_unparseable_transfer_call_counts_as_used() -> anyhow::Result<()> {
    let env = setup().await?;
    allow(&env, env.shop.id(), true).await?;
    set_switch(&env, "garbage_result").await?;
    let result = request(&env, env.shop.id(), 25 * TST, Some("refund:10000000")).await?;
    assert_eq!(
        result.json::<Value>()?,
        json!({ "status": "paid", "request_id": "1", "sent": (25 * TST).to_string() })
    );
    assert_eq!(balance(&env, env.shop.id()).await?, 15 * TST);
    assert_eq!(remaining(&env).await?, DAILY_MAX - 25 * TST);
    Ok(())
}

fn refused_for_balance(result: &ExecutionFinalResult, request_id: &str) {
    assert_eq!(
        result.clone().json::<Value>().unwrap(),
        json!({ "status": "refused", "request_id": request_id, "reason": "insufficient_balance" })
    );
    let events = guard_events(result.logs());
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["event"], "payment_refused");
}

/// Decision 26. The guard's sum and nearcore's agree to the yocto: one more
/// than the guard can afford is refused, and exactly what it can afford is
/// paid and leaves it at its storage cost.
#[tokio::test]
async fn guard_refuses_what_it_cannot_afford() -> anyhow::Result<()> {
    let env = setup().await?;
    native_policy(&env, 100 * NEAR).await?;
    allow_native(&env, env.alice.id()).await?;

    let free = free_balance(&env).await?;
    let alice_before = env.alice.view_account().await?.balance;
    let over = request_token(
        &env,
        &json!("native"),
        env.alice.id(),
        free + 1,
        None,
        NATIVE_GAS,
    )
    .await?;
    refused_for_balance(&over, "1");
    let native = view(&env, "get_token", json!({ "token": "native" })).await?;
    assert_eq!(native["autonomous_spent"], "0");
    assert_eq!(env.alice.view_account().await?.balance, alice_before);

    let exact = request_token(
        &env,
        &json!("native"),
        env.alice.id(),
        free,
        None,
        NATIVE_GAS,
    )
    .await?;
    assert_eq!(
        exact.json::<Value>()?,
        json!({ "status": "paid", "request_id": "2", "sent": free.to_string() })
    );
    assert_eq!(free_balance(&env).await?, 0);

    // With nothing free, the 1 yocto of an FT payment and a held request's
    // storage are out of reach too, and neither fails the receipt.
    let ft = request(&env, env.alice.id(), 5 * TST, None).await?;
    refused_for_balance(&ft, "3");
    let held = request(&env, env.alice.id(), 60 * TST, None).await?;
    refused_for_balance(&held, "4");
    assert_eq!(view(&env, "get_pending", json!({})).await?, json!([]));
    assert_eq!(remaining(&env).await?, DAILY_MAX);

    // The owner tops the guard up and both work again.
    env.owner
        .transfer_near(env.guard.id(), NearToken::from_near(1))
        .await?
        .into_result()?;
    let ft = request(&env, env.alice.id(), 5 * TST, None).await?;
    assert_eq!(ft.json::<Value>()?["status"], "paid");
    let held = request(&env, env.alice.id(), 60 * TST, None).await?;
    assert_eq!(held.json::<Value>()?["status"], "pending");
    Ok(())
}

#[tokio::test]
async fn approve_waits_until_the_guard_can_pay() -> anyhow::Result<()> {
    let env = setup().await?;
    native_policy(&env, NEAR).await?;
    allow_native(&env, env.alice.id()).await?;

    let filed = request_token(
        &env,
        &json!("native"),
        env.alice.id(),
        5 * NEAR,
        None,
        NATIVE_GAS,
    )
    .await?;
    assert_eq!(filed.json::<Value>()?["reason"], "above_threshold");

    // The owner leaves 2 yocto less than the payment free. The owner's own
    // 1 yocto stays with the guard, and the approver's will count towards the
    // payment, so that's still 1 short.
    let free = free_balance(&env).await?;
    owner_call(
        &env,
        "withdraw",
        json!({
            "token": "native",
            "amount": (free + 1 - (5 * NEAR - 2)).to_string(),
            "receiver_id": env.owner.id(),
        }),
    )
    .await?
    .into_result()?;

    let approve = async || {
        let entry = view(&env, "get_request", json!({ "request_id": "1" })).await?;
        anyhow::Ok(
            env.approver
                .call(env.guard.id(), "approve")
                .args_json(json!({ "payload": entry["payload"] }))
                .deposit(ONE_YOCTO)
                .gas(FT_TRANSFER_GAS)
                .transact()
                .await?,
        )
    };
    let short = approve().await?;
    assert!(
        failure_text(&short).contains(near_spend_guard::ERR_CANNOT_AFFORD),
        "{short:?}"
    );
    assert!(guard_events(short.logs()).is_empty());

    env.owner
        .transfer_near(env.guard.id(), ONE_YOCTO)
        .await?
        .into_result()?;
    let paid = approve().await?;
    assert_eq!(
        paid.json::<Value>()?,
        json!({ "status": "paid", "request_id": "1", "sent": (5 * NEAR).to_string() })
    );
    Ok(())
}

#[tokio::test]
async fn withdraw_stops_at_the_storage_cost() -> anyhow::Result<()> {
    let env = setup().await?;
    let withdraw = async |amount: u128| {
        owner_call(
            &env,
            "withdraw",
            json!({
                "token": "native",
                "amount": amount.to_string(),
                "receiver_id": env.owner.id(),
            }),
        )
        .await
    };

    // The owner's 1 yocto is the guard's until the call ends, so free + 1
    // leaves it at its storage cost and one more can't go.
    let free = free_balance(&env).await?;
    let over = withdraw(free + 2).await?;
    assert!(
        failure_text(&over).contains(near_spend_guard::ERR_CANNOT_AFFORD),
        "{over:?}"
    );
    assert!(guard_events(over.logs()).is_empty());
    withdraw(free + 1).await?.into_result()?;
    assert_eq!(free_balance(&env).await?, 0);
    Ok(())
}

/// Decision 27. A named account has to exist, so the transfer fails, NEAR
/// sends the money back and the budget comes back. Implicit accounts come into
/// being with their first transfer, `0u` ones included (NEP-655, protocol 88):
/// `paid` is true, and the money is theirs from then on.
#[tokio::test]
async fn native_payment_to_an_account_that_does_not_exist() -> anyhow::Result<()> {
    let env = setup().await?;
    native_policy(&env, 100 * NEAR).await?;
    let amount = NEAR / 10;
    let native = json!("native");

    let missing: AccountId = "missing.test.near".parse()?;
    allow_native(&env, &missing).await?;
    let before = env.guard.view_account().await?.balance;
    let failed = request_token(&env, &native, &missing, amount, None, NATIVE_GAS).await?;
    assert_eq!(
        failed.json::<Value>()?,
        json!({ "status": "failed", "request_id": "1", "released": amount.to_string() })
    );
    assert_eq!(env.guard.view_account().await?.balance, before);
    let token = view(&env, "get_token", json!({ "token": "native" })).await?;
    assert_eq!(token["autonomous_spent"], "0");

    let implicit = [
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0x0123456789abcdef0123456789abcdef01234567",
        "0s0123456789abcdef0123456789abcdef01234567",
        "0u0000000000000000000000000000000000000000000000000000",
    ];
    for (i, id) in implicit.into_iter().enumerate() {
        let id: AccountId = id.parse()?;
        allow_native(&env, &id).await?;
        let before = env.guard.view_account().await?.balance.as_yoctonear();
        let paid = request_token(&env, &native, &id, amount, None, NATIVE_GAS).await?;
        assert_eq!(
            paid.json::<Value>()?,
            json!({
                "status": "paid",
                "request_id": (i + 2).to_string(),
                "sent": amount.to_string(),
            }),
            "{id}"
        );
        let after = env.guard.view_account().await?.balance.as_yoctonear();
        assert_eq!(before - after, amount, "{id}");
        let created = env.worker.view_account(&id).await?;
        assert_eq!(created.balance.as_yoctonear(), amount, "{id}");
    }
    Ok(())
}
