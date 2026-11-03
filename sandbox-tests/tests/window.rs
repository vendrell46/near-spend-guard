//! The rolling window on a real chain: spending counts for 24 to 25 slots,
//! then comes back. One-second slots (the shortest `new` accepts) and
//! near-workspaces' `fast_forward` stand in for hours.

mod common;

use common::*;
use serde_json::{Value, json};

const SLOT_NS: u64 = 1_000_000_000;

async fn now(env: &Env) -> anyhow::Result<u64> {
    Ok(env.worker.view_block().await?.timestamp())
}

#[tokio::test]
async fn window_rollover_with_fast_forward() -> anyhow::Result<()> {
    let env = setup_with_slot(SLOT_NS).await?;

    // Spend the whole cap, in four payments.
    for id in 1..=4 {
        let paid = request(&env, env.alice.id(), 25 * TST, None).await?;
        assert_eq!(paid.json::<Value>()?["request_id"], id.to_string());
    }
    assert_eq!(remaining(&env).await?, 0);
    let held = request(&env, env.alice.id(), 25 * TST, None).await?;
    assert_eq!(held.json::<Value>()?["reason"], "daily_cap");

    // The view says when budget next comes back: the end of the oldest slot
    // with spending, 25 slots on. The four payments may sit in different
    // one-second slots, so it comes back in steps. At each step, half a slot
    // before the promised time nothing has come back, and one block after
    // it something has, so a promise a slot early or late fails.
    let mut spent = DAILY_MAX;
    let mut checked_before = 0;
    let mut steps = 0;
    while spent > 0 {
        steps += 1;
        assert!(steps <= 4, "more release steps than payments");
        let token = view(&env, "get_token", json!({ "token": tst_token(&env) })).await?;
        assert_eq!(token["autonomous_spent"], spent.to_string());
        let release_at: u64 = token["next_release_at"].as_str().unwrap().parse()?;
        assert!(release_at - now(&env).await? <= 25 * SLOT_NS);

        // A fast-forwarded block moves the clock about 0.3 s, so jumping
        // gap / 0.35 s blocks never passes the target.
        let target = release_at - SLOT_NS / 2;
        loop {
            let gap = target.saturating_sub(now(&env).await?);
            if gap == 0 {
                break;
            }
            env.worker
                .fast_forward((gap / 350_000_000).clamp(1, 20))
                .await?;
        }
        // Counted only if the chain's clock was still before the promise on
        // both sides of the view: blocks keep coming in real time.
        let before = remaining(&env).await?;
        if now(&env).await? < release_at {
            assert_eq!(
                before,
                DAILY_MAX - spent,
                "budget came back before next_release_at"
            );
            checked_before += 1;
        }
        while now(&env).await? <= release_at {
            env.worker.fast_forward(1).await?;
        }
        let still = DAILY_MAX - remaining(&env).await?;
        assert!(still < spent, "nothing came back at next_release_at");
        spent = still;
    }
    assert!(
        checked_before >= 1,
        "never checked the window just before a release"
    );
    let token = view(&env, "get_token", json!({ "token": tst_token(&env) })).await?;
    assert!(token["next_release_at"].is_null());

    // The whole cap is back, and the agent pays again without a human.
    let again = request(&env, env.alice.id(), 25 * TST, None).await?;
    assert_eq!(
        again.json::<Value>()?,
        json!({ "status": "paid", "request_id": "6", "sent": (25 * TST).to_string() })
    );
    assert_eq!(balance(&env, env.alice.id()).await?, 125 * TST);
    Ok(())
}
