//! A compromised agent fires a burst to beat the daily cap (demo scene 1,
//! against our guard). Ten requests of 25 against a cap of 100 are all sent
//! before any is final; exactly four leave and six wait for a human.

mod common;

use std::task::Poll;
use std::time::{Duration, Instant};

use common::*;
use near_workspaces::operations::TransactionStatus;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{AccessKeyPermission, Gas, NearToken, PublicKey};
use serde_json::{Value, json};

/// The agent key's remaining allowance.
async fn allowance(env: &Env, key: &PublicKey) -> anyhow::Result<NearToken> {
    let key = env.agent.view_access_key(key).await?;
    match key.permission {
        AccessKeyPermission::FunctionCall(p) => Ok(p.allowance.expect("a limited allowance")),
        AccessKeyPermission::FullAccess => anyhow::bail!("expected a function-call key"),
    }
}

/// The height of the block in which the guard logged `event` for this
/// transaction.
async fn height_of(env: &Env, result: &ExecutionFinalResult, event: &str) -> anyhow::Result<u64> {
    let needle = format!("\"event\":\"{event}\"");
    let outcome = result
        .outcomes()
        .into_iter()
        .find(|o| &o.executor_id == env.guard.id() && o.logs.iter().any(|l| l.contains(&needle)))
        .unwrap_or_else(|| panic!("no {event} in {result:?}"));
    Ok(env
        .worker
        .view_block()
        .block_hash(outcome.block_hash)
        .await?
        .height())
}

/// Gas for each request in the burst. nearcore fills a chunk until the
/// transactions' prepaid gas would pass its 1,000 TGas limit; at the agent's
/// usual 100 TGas only nine of ten fitted, and the tenth was never included
/// (3 Nov: its status never resolved, and the key's next transaction skipped
/// its nonce). 40 TGas covers what `request_payment` checks for an
/// `ft_transfer` (25) plus its own work, and ten fit in one chunk.
const BURST_GAS: Gas = Gas::from_tgas(40);

/// Polls every transaction until it's final, and fails with the hash of one
/// that isn't within 30 seconds rather than waiting on it forever. A status
/// query for a transaction the node never includes blocks until the RPC
/// client times out, about 50 seconds, so each query gets 3.
async fn wait_all(sent: &[TransactionStatus]) -> anyhow::Result<Vec<ExecutionFinalResult>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut results = Vec::new();
    for tx in sent {
        let result = loop {
            anyhow::ensure!(
                Instant::now() < deadline,
                "{} was never included",
                tx.hash()
            );
            match tokio::time::timeout(Duration::from_secs(3), tx.status()).await {
                Ok(Ok(Poll::Ready(result))) => break result,
                Ok(Ok(Poll::Pending)) | Err(_) => {
                    tokio::time::sleep(Duration::from_millis(250)).await
                }
                Ok(Err(e)) => return Err(e.into()),
            }
        };
        results.push(result);
    }
    Ok(results)
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
    let mut requested = Vec::new();
    let mut settled = Vec::new();
    for result in &results {
        let paid = result.clone().json::<Value>()?["status"] == "paid";
        let event = if paid {
            "payment_dispatched"
        } else {
            "approval_requested"
        };
        requested.push(height_of(&env, result, event).await?);
        if paid {
            settled.push(height_of(&env, result, "payment_settled").await?);
        }
    }
    let first_callback = *settled.iter().min().expect("some payments");
    let before = requested.iter().filter(|&&h| h < first_callback).count();
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
