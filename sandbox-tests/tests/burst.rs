//! A compromised agent fires a burst to beat the daily cap (demo scene 1,
//! against our guard). Ten requests of 25 against a cap of 100 are all sent
//! before any is final; exactly four leave and six wait for a human.

mod common;

use std::time::Duration;

use common::*;
use near_workspaces::types::{AccessKeyPermission, NearToken, PublicKey};
use serde_json::{Value, json};

/// The agent key's remaining allowance.
async fn allowance(env: &Env, key: &PublicKey) -> anyhow::Result<NearToken> {
    let key = env.agent.view_access_key(key).await?;
    match key.permission {
        AccessKeyPermission::FunctionCall(p) => Ok(p.allowance.expect("a limited allowance")),
        AccessKeyPermission::FullAccess => anyhow::bail!("expected a function-call key"),
    }
}

#[tokio::test]
async fn race_guard_exact() -> anyhow::Result<()> {
    let env = setup().await?;

    // Warm the key with one sequential call, so the burst's nonces come from
    // near-workspaces' cache (two first uses of a key can get the same
    // nonce). The shop isn't allowed for TST, so it's refused and the
    // budget stays whole.
    let warm = request(&env, env.shop.id(), 25 * TST, None).await?;
    assert_eq!(
        warm.json::<Value>()?,
        json!({ "status": "refused", "request_id": "1", "reason": "receiver_not_allowed" })
    );
    assert_eq!(remaining(&env).await?, DAILY_MAX);

    // Ten requests of 25, all broadcast before polling any.
    let expires_at = expiry(&env).await?;
    let mut sent = Vec::new();
    for _ in 0..10 {
        let tx = env
            .agent
            .call(env.guard.id(), "request_payment")
            .args_json(json!({
                "token": tst_token(&env),
                "receiver_id": env.alice.id(),
                "amount": (25 * TST).to_string(),
                "msg": null,
                "expires_at": expires_at,
            }))
            .gas(BURST_GAS)
            .transact_async()
            .await?;
        sent.push(tx);
    }
    let results = wait_all(&sent).await?;

    // It was a race: at least five requests read the window before the
    // first payment's callback ran, so a guard that counted only in its
    // callback would have let at least five leave.
    let (before, requested, settled) = requests_before_first_callback(&env, &results).await?;
    eprintln!("request heights {requested:?}, callback heights {settled:?}");
    assert!(
        before >= 5,
        "only {before} requests ran before the first callback; the burst wasn't parallel"
    );

    // Exactly the cap left without a human; the rest wait, reason daily_cap.
    let mut statuses: Vec<(String, String)> = results
        .into_iter()
        .map(|r| {
            let v = r.json::<Value>().unwrap();
            let detail = v.get("reason").or(v.get("sent")).cloned().unwrap();
            (
                v["status"].as_str().unwrap().to_string(),
                detail.to_string(),
            )
        })
        .collect();
    statuses.sort();
    let mut expected = vec![("paid".to_string(), format!("\"{}\"", 25 * TST)); 4];
    expected.extend(vec![
        ("pending".to_string(), "\"daily_cap\"".to_string());
        6
    ]);
    assert_eq!(statuses, expected);
    assert_eq!(balance(&env, env.alice.id()).await?, DAILY_MAX);
    assert_eq!(remaining(&env).await?, 0);
    let pending = view(&env, "get_pending", json!({})).await?;
    assert_eq!(pending.as_array().unwrap().len(), 6);

    Ok(())
}

/// What one request costs the agent key's allowance. nearcore takes the
/// prepaid gas from the allowance when the transaction is accepted and gives
/// the unused part back with the refund receipt a few blocks later, so the
/// allowance bounds how many requests can be in flight at once, not only how
/// many the key can ever make. Measured for the README.
#[tokio::test]
async fn agent_key_allowance_per_request() -> anyhow::Result<()> {
    let env = setup().await?;
    let key = env.agent.secret_key().public_key();
    let start = allowance(&env, &key).await?.as_yoctonear();
    let paid = request(&env, env.alice.id(), 5 * TST, None).await?;
    assert_eq!(paid.json::<Value>()?["status"], "paid");
    let at_final = allowance(&env, &key).await?.as_yoctonear();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let refunded = allowance(&env, &key).await?.as_yoctonear();
    eprintln!(
        "one paid ft_transfer request at {} TGas: {} yocto held when final, {} after refunds",
        AGENT_GAS.as_tgas(),
        start - at_final,
        start - refunded
    );
    assert!(refunded >= at_final && refunded < start);
    Ok(())
}
