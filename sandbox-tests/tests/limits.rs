//! The guard at its limits: 8 tokens, 32 receivers on each, 16 pending
//! requests, with the longest account ids and messages NEAR allows. What it
//! stores per token and per request is read whole, so the gas and storage it
//! needs grow with these limits; day 4 measured them with one or two
//! receivers.

mod common;

use common::*;
use near_spend_guard::{MAX_MSG_BYTES, MAX_PENDING, MAX_PENDING_ENTRY_BYTES, MAX_RECEIVERS};
use near_workspaces::AccountId;
use near_workspaces::types::Gas;
use serde_json::{Value, json};

const GAS_CALLBACK: Gas = Gas::from_gas(near_spend_guard::GAS_CALLBACK.as_gas());
/// What the owner's wallet or near CLI attaches by default.
const WALLET_GAS: Gas = Gas::from_tgas(100);

/// A 64-character id, the longest NEAR allows. It has an implicit account's
/// shape, so nothing has to exist at it.
fn long_id(n: usize) -> AccountId {
    format!("{n:064x}").parse().unwrap()
}

fn long_msg() -> String {
    "m".repeat(MAX_MSG_BYTES)
}

/// Allowlists `ids` for `token`, all sent before any is polled: a full policy
/// is 32 transactions.
async fn allow_all(
    env: &Env,
    token: &Value,
    ids: impl Iterator<Item = AccountId>,
    ft_call: bool,
) -> anyhow::Result<()> {
    let mut sent = Vec::new();
    for id in ids {
        let tx = env
            .owner
            .call(env.guard.id(), "allow_receiver")
            .args_json(json!({ "token": token, "receiver_id": id, "ft_call": ft_call }))
            .deposit(ONE_YOCTO)
            .gas(Gas::from_tgas(10))
            .transact_async()
            .await?;
        sent.push(tx);
    }
    for result in wait_all(&sent).await? {
        result.into_result()?;
    }
    Ok(())
}

async fn add_token(env: &Env, token: &Value) -> anyhow::Result<()> {
    owner_call(
        env,
        "set_token",
        json!({
            "token": token,
            "per_payment_max": (1_000 * TST).to_string(),
            "daily_max": (1_000 * TST).to_string(),
            "approval_above": "1",
        }),
    )
    .await?
    .into_result()?;
    Ok(())
}

/// Files `MAX_PENDING` requests of the largest size, expiring at `expires_at`.
async fn fill_pending(
    env: &Env,
    token: &Value,
    receiver: &AccountId,
    expires_at: &str,
) -> anyhow::Result<()> {
    for _ in 0..MAX_PENDING {
        let filed: Value = env
            .agent
            .call(env.guard.id(), "request_payment")
            .args_json(json!({
                "token": token,
                "receiver_id": receiver,
                "amount": (2 * TST).to_string(),
                "msg": long_msg(),
                "expires_at": expires_at,
            }))
            .gas(AGENT_GAS)
            .transact()
            .await?
            .json()?;
        assert_eq!(filed["status"], "pending", "{filed}");
    }
    Ok(())
}

#[tokio::test]
async fn pending_entry_storage() -> anyhow::Result<()> {
    let env = setup().await?;
    let token = json!({ "ft": long_id(1) });
    let receiver = long_id(2);
    add_token(&env, &token).await?;
    allow_all(&env, &token, std::iter::once(receiver.clone()), true).await?;

    let before = env.guard.view_account().await?.storage_usage;
    fill_pending(&env, &token, &receiver, &expiry(&env).await?).await?;
    let after = env.guard.view_account().await?.storage_usage;
    let per_entry = (after - before).div_ceil(u64::from(MAX_PENDING));
    eprintln!("a full pending entry: {per_entry} bytes of storage");
    assert!(
        per_entry <= MAX_PENDING_ENTRY_BYTES,
        "{per_entry} bytes, over MAX_PENDING_ENTRY_BYTES"
    );
    Ok(())
}

/// The agent's gas (decision 24) and the callback's headroom (decision 23)
/// hold with a token at 32 receivers of the longest ids, paying the last.
#[tokio::test]
async fn gas_with_a_full_policy() -> anyhow::Result<()> {
    let env = setup().await?;
    let tst = tst_token(&env);
    let guard = env.guard.id().as_str();

    // alice is the first, then 29 more, one unregistered and one registered.
    allow_all(&env, &tst, (0..MAX_RECEIVERS - 3).map(long_id), false).await?;
    let unregistered = long_id(1_000);
    let registered = long_id(1_001);
    env.owner
        .call(env.tst.id(), "storage_deposit")
        .args_json(json!({ "account_id": registered }))
        .deposit(near_workspaces::types::NearToken::from_millinear(10))
        .transact()
        .await?
        .into_result()?;
    allow_all(&env, &tst, std::iter::once(unregistered.clone()), false).await?;
    allow_all(&env, &tst, std::iter::once(registered.clone()), true).await?;
    let token = view(&env, "get_token", json!({ "token": tst })).await?;
    assert_eq!(token["receivers"].as_array().unwrap().len(), MAX_RECEIVERS);

    let paid = request_token(&env, &tst, &registered, 5 * TST, None, FT_TRANSFER_GAS).await?;
    eprintln!(
        "request_payment, 32 receivers: {:.2} TGas burnt",
        burnt(&paid, guard, Some("payment_dispatched")).as_gas() as f64 / 1e12
    );
    headroom(
        "callback after ft_transfer, 32 receivers",
        burnt(&paid, guard, Some("payment_settled")),
        GAS_CALLBACK,
    );
    assert_eq!(paid.json::<Value>()?["status"], "paid");

    // The callback's heaviest path: a release rewrites everything the guard
    // keeps for the token.
    let failed = request_token(&env, &tst, &unregistered, 5 * TST, None, FT_TRANSFER_GAS).await?;
    headroom(
        "callback releasing, 32 receivers",
        burnt(&failed, guard, Some("payment_settled")),
        GAS_CALLBACK,
    );
    assert_eq!(failed.json::<Value>()?["status"], "failed");

    // The longest `ft_transfer_call`: nobody is deployed at the receiver, so
    // its `ft_on_transfer` fails and the tokens come back.
    let call = request_token(
        &env,
        &tst,
        &registered,
        5 * TST,
        Some(&long_msg()),
        FT_TRANSFER_CALL_GAS,
    )
    .await?;
    headroom(
        "callback after the longest ft_transfer_call, 32 receivers",
        burnt(&call, guard, Some("payment_settled")),
        GAS_CALLBACK,
    );
    assert_eq!(call.json::<Value>()?["sent"], "0");

    // Native, creating the account it pays: the dearest transfer there is.
    native_policy(&env, 100 * NEAR).await?;
    allow_all(
        &env,
        &json!("native"),
        (0..MAX_RECEIVERS - 1).map(long_id),
        false,
    )
    .await?;
    let universal: AccountId = "0u0000000000000000000000000000000000000000000000000000".parse()?;
    allow_native(&env, &universal).await?;
    let native = request_token(
        &env,
        &json!("native"),
        &universal,
        NEAR / 10,
        None,
        NATIVE_GAS,
    )
    .await?;
    eprintln!(
        "native payment creating an account, 32 receivers: {:.2} TGas burnt in all",
        native.total_gas_burnt.as_gas() as f64 / 1e12
    );
    headroom(
        "callback after a native payment, 32 receivers",
        burnt(&native, guard, Some("payment_settled")),
        GAS_CALLBACK,
    );
    assert_eq!(native.json::<Value>()?["status"], "paid");
    Ok(())
}

/// Replacing a compromised agent has to work however full the guard is, and
/// so do the views the approver reads and `prune_expired`.
#[tokio::test]
async fn full_guard_stays_manageable() -> anyhow::Result<()> {
    let env = setup().await?;
    let root = env.worker.root_account()?;
    let new_agent = subaccount(&root, "agent2", 1).await?;

    // TST, native and six more, with the longest ids.
    let mut tokens = vec![tst_token(&env), json!("native")];
    tokens.extend((0..6).map(|i| json!({ "ft": long_id(100 + i) })));
    native_policy(&env, NEAR).await?;
    for token in &tokens[2..] {
        add_token(&env, token).await?;
    }
    allow_all(&env, &tokens[0], (0..MAX_RECEIVERS - 1).map(long_id), false).await?;
    for token in &tokens[1..tokens.len() - 1] {
        allow_all(&env, token, (0..MAX_RECEIVERS).map(long_id), false).await?;
    }
    let last = &tokens[tokens.len() - 1];
    allow_all(&env, last, (0..MAX_RECEIVERS).map(long_id), true).await?;
    let guard = env.guard.view_account().await?;
    eprintln!(
        "a full policy: {} bytes of storage, code included ({:.3} NEAR)",
        guard.storage_usage,
        guard.storage_usage as f64 / 1e5
    );

    // A full pending list that expires soon, read and then pruned by anyone.
    let now = env.worker.view_block().await?.timestamp();
    let soon = (now + 120_000_000_000).to_string();
    fill_pending(&env, last, &long_id(0), &soon).await?;
    let all = view(&env, "get_tokens", json!({})).await?;
    assert_eq!(all.as_array().unwrap().len(), tokens.len());
    let pending = view(&env, "get_pending", json!({})).await?;
    assert_eq!(pending.as_array().unwrap().len(), MAX_PENDING as usize);
    while env.worker.view_block().await?.timestamp() <= now + 120_000_000_000 {
        env.worker.fast_forward(100).await?;
    }
    let pruned = env
        .alice
        .call(env.guard.id(), "prune_expired")
        .args_json(json!({}))
        .gas(WALLET_GAS)
        .transact()
        .await?;
    eprintln!(
        "prune_expired of 16 full entries: {:.2} TGas burnt",
        burnt(&pruned, env.guard.id().as_str(), None).as_gas() as f64 / 1e12
    );
    assert_eq!(pruned.json::<u32>()?, MAX_PENDING);

    // The same list again, and the owner replaces the agent.
    fill_pending(&env, last, &long_id(0), &expiry(&env).await?).await?;
    let replaced = env
        .owner
        .call(env.guard.id(), "set_agent")
        .args_json(json!({ "agent_id": new_agent.id() }))
        .deposit(ONE_YOCTO)
        .gas(WALLET_GAS)
        .transact()
        .await?;
    eprintln!(
        "set_agent with 8 full tokens and 16 full entries: {:.2} TGas burnt",
        burnt(&replaced, env.guard.id().as_str(), None).as_gas() as f64 / 1e12
    );
    replaced.into_result()?;
    assert_eq!(view(&env, "get_pending", json!({})).await?, json!([]));
    Ok(())
}
