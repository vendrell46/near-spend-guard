//! Checks that exist only in the compiled contract or in nearcore, so unit
//! tests can't see them: near-sdk generates the `#[init]` re-init check,
//! `#[private]` and the "doesn't accept deposit" check into the wasm export,
//! and access-key permissions are enforced by nearcore before the guard runs.

mod common;

use common::*;
use near_workspaces::types::NearToken;
use serde_json::json;

fn new_args(env: &Env) -> serde_json::Value {
    json!({
        "owner_id": env.owner.id(),
        "agent_id": env.agent_full.id(),
        "approver_id": env.approver.id(),
        "slot_ns": HOUR_NS.to_string(),
    })
}

#[tokio::test]
async fn new_twice_panics() -> anyhow::Result<()> {
    let env = setup().await?;
    let again = env
        .guard
        .call("new")
        .args_json(new_args(&env))
        .transact()
        .await?;
    assert!(
        failure_text(&again).contains("The contract has already been initialized"),
        "{again:?}"
    );
    // The configured token survived.
    assert_eq!(remaining(&env).await?, DAILY_MAX);
    Ok(())
}

#[tokio::test]
async fn new_is_private() -> anyhow::Result<()> {
    let env = setup().await?;
    let root = env.worker.root_account()?;
    let fresh = subaccount(&root, "fresh", 20)
        .await?
        .deploy(&guard_wasm())
        .await?
        .into_result()?;

    // Someone else initialising a freshly deployed guard would choose its
    // owner. Refused by `#[private]`.
    let by_stranger = env
        .alice
        .call(fresh.id(), "new")
        .args_json(new_args(&env))
        .transact()
        .await?;
    assert!(
        failure_text(&by_stranger).contains("Method new is private"),
        "{by_stranger:?}"
    );

    // The same call from the guard's own account works, so the refusal was
    // the private check and not the arguments.
    fresh
        .call("new")
        .args_json(new_args(&env))
        .transact()
        .await?
        .into_result()?;
    Ok(())
}

#[tokio::test]
async fn callback_is_private() -> anyhow::Result<()> {
    let env = setup().await?;
    // Spend 25 so a forged "failed" callback would have something to give
    // back.
    let paid = request(&env, env.alice.id(), 25 * TST, None).await?;
    assert_eq!(
        paid.json::<serde_json::Value>()?,
        json!({ "status": "paid", "request_id": "1", "sent": (25 * TST).to_string() })
    );
    assert_eq!(remaining(&env).await?, DAILY_MAX - 25 * TST);

    let epoch = env.worker.view_block().await?.timestamp() / HOUR_NS;
    let forged = env
        .alice
        .call(env.guard.id(), "on_payment_settled")
        .args_json(json!({
            "request_id": "1",
            "token": tst_token(&env),
            "amount": (25 * TST).to_string(),
            "method": "ft_transfer",
            "mode": "autonomous",
            "epoch": epoch.to_string(),
            "generation": "1",
        }))
        .gas(AGENT_GAS)
        .transact()
        .await?;
    assert!(
        failure_text(&forged).contains("Method on_payment_settled is private"),
        "{forged:?}"
    );
    assert_eq!(remaining(&env).await?, DAILY_MAX - 25 * TST);
    Ok(())
}

#[tokio::test]
async fn request_payment_refuses_deposit() -> anyhow::Result<()> {
    let env = setup().await?;
    // From the agent's full-access key: a function-call key couldn't attach
    // a deposit at all.
    let with_deposit = env
        .agent_full
        .call(env.guard.id(), "request_payment")
        .args_json(json!({
            "token": tst_token(&env),
            "receiver_id": env.alice.id(),
            "amount": (25 * TST).to_string(),
            "msg": null,
            "expires_at": expiry(&env).await?,
        }))
        .deposit(ONE_YOCTO)
        .gas(AGENT_GAS)
        .transact()
        .await?;
    assert!(
        failure_text(&with_deposit).contains("Method request_payment doesn't accept deposit"),
        "{with_deposit:?}"
    );

    // Nothing was recorded: the budget is whole, nothing moved, and the
    // next request still gets id 1.
    assert_eq!(remaining(&env).await?, DAILY_MAX);
    assert_eq!(balance(&env, env.alice.id()).await?, 0);
    let next = request(&env, env.alice.id(), 25 * TST, None).await?;
    assert_eq!(next.json::<serde_json::Value>()?["request_id"], "1");
    Ok(())
}

#[tokio::test]
async fn pause_from_limited_key() -> anyhow::Result<()> {
    let env = setup().await?;
    // A kill switch: a function-call key on the approver's account limited
    // to `pause` (decision 11). It can't attach a deposit, and `pause`
    // needs none.
    let kill = limited_key(&env.worker, &env.approver, env.guard.id(), &["pause"]).await?;
    let paused = kill
        .call(env.guard.id(), "pause")
        .transact()
        .await?
        .into_result()?;
    assert_eq!(guard_events(paused.logs())[0]["event"], "paused");
    assert_eq!(view(&env, "get_config", json!({})).await?["paused"], true);

    // Paused: the agent's request is refused and nothing moves.
    let refused = request(&env, env.alice.id(), 25 * TST, None).await?;
    assert_eq!(
        refused.json::<serde_json::Value>()?,
        json!({ "status": "refused", "request_id": "1", "reason": "paused" })
    );

    // The same key can't unpause: nearcore refuses the method before the
    // guard runs.
    let unpause = kill.call(env.guard.id(), "unpause").transact().await;
    let err = format!("{unpause:?}");
    assert!(err.contains("MethodNameMismatch"), "{err}");

    // The owner unpauses with a full-access key and 1 yocto.
    owner_call(&env, "unpause", json!({}))
        .await?
        .into_result()?;
    assert_eq!(view(&env, "get_config", json!({})).await?["paused"], false);
    Ok(())
}

#[tokio::test]
async fn function_call_key_cannot_approve() -> anyhow::Result<()> {
    let env = setup().await?;
    // 60 TST is above `approval_above`, so it waits for a human.
    let pending = request(&env, env.alice.id(), 60 * TST, None).await?;
    let pending = pending.json::<serde_json::Value>()?;
    assert_eq!(pending["status"], "pending");
    let payload = view(&env, "get_request", json!({ "request_id": "1" })).await?["payload"].clone();

    // `approve` needs exactly 1 yocto, and nearcore refuses any deposit
    // from a function-call key, so a compromised agent holding such a key
    // to the approver's account still can't approve.
    let fc = limited_key(&env.worker, &env.approver, env.guard.id(), &["approve"]).await?;
    let by_fc = fc
        .call(env.guard.id(), "approve")
        .args_json(json!({ "payload": payload }))
        .deposit(ONE_YOCTO)
        .gas(AGENT_GAS)
        .transact()
        .await;
    let err = format!("{by_fc:?}");
    assert!(err.contains("DepositWithFunctionCall"), "{err}");
    assert!(
        !view(&env, "get_request", json!({ "request_id": "1" }))
            .await?
            .is_null()
    );

    // The approver's full-access key approves the same payload.
    let approved = env
        .approver
        .call(env.guard.id(), "approve")
        .args_json(json!({ "payload": payload }))
        .deposit(ONE_YOCTO)
        .gas(AGENT_GAS)
        .transact()
        .await?;
    assert_eq!(
        approved.json::<serde_json::Value>()?,
        json!({ "status": "paid", "request_id": "1", "sent": (60 * TST).to_string() })
    );
    assert_eq!(balance(&env, env.alice.id()).await?, 60 * TST);
    Ok(())
}

#[tokio::test]
async fn agent_key_reaches_only_request_payment() -> anyhow::Result<()> {
    let env = setup().await?;
    for (method, args) in [
        ("pause", json!({})),
        ("reject", json!({ "request_id": "1" })),
        ("prune_expired", json!({})),
    ] {
        let call = env
            .agent
            .call(env.guard.id(), method)
            .args_json(args)
            .transact()
            .await;
        let err = format!("{call:?}");
        assert!(err.contains("MethodNameMismatch"), "{method}: {err}");
    }
    // Nor any other contract, such as the token itself.
    let to_token = env
        .agent
        .call(env.tst.id(), "ft_transfer")
        .args_json(json!({ "receiver_id": env.alice.id(), "amount": "1" }))
        .transact()
        .await;
    let err = format!("{to_token:?}");
    assert!(err.contains("ReceiverMismatch"), "{err}");

    // And no plain transfer of NEAR. 1 yocto, so the key's allowance can't
    // be the reason: nearcore checks the allowance first.
    let send = env
        .agent
        .transfer_near(env.alice.id(), NearToken::from_yoctonear(1))
        .await;
    let err = format!("{send:?}");
    assert!(err.contains("RequiresFullAccess"), "{err}");
    Ok(())
}
