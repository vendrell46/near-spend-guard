//! How a payment settles against a real NEP-141 token, including the ways a
//! token or a receiver can fail or answer oddly. The unit tests replay these
//! results by hand; here the test token produces them (decisions 5 and 13).

mod common;

use common::*;
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
