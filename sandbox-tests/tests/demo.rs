//! The demo's scenes as tests (DEMO.md), each printing its verdict lines and
//! asserting them:
//!
//! - `race`: ten payments of 25 TST at once against a daily cap of 100.
//! - `price_swap`: the human agreed to 50 TST, the filed request says 190.
//! - `approval_flow`: scene 3 on a local node, the fallback if testnet is
//!   down on the day.
//!
//! Run one with `scripts/test-sandbox.sh --test demo race -- --nocapture`.
//! The naive guard (`contracts/naive-guard`) has our policy, allowlist and
//! approval deposit, and differs in the two places its header names.

mod common;

use common::*;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId, Contract};
use serde_json::{Value, json};

/// What the approver attaches to `approve`: the transfer, its callback and
/// the guard's own work fit with room to spare.
const APPROVER_GAS: Gas = Gas::from_tgas(100);

struct Demo {
    env: Env,
    /// The API provider, allowed on both guards.
    api: Account,
    /// Registered with the token, allowed on neither guard.
    mallory: Account,
    naive: Contract,
    /// The agent's account signing with a function-call key limited to
    /// `request_payment` on the naive guard.
    naive_agent: Account,
}

/// `setup()` plus the API provider and mallory, and the naive guard deployed
/// next to ours with the same roles, slot, policy, receivers and 1,000 TST.
async fn demo_setup() -> anyhow::Result<Demo> {
    let env = setup().await?;
    let root = env.worker.root_account()?;
    let api = subaccount(&root, "api", 10).await?;
    let mallory = subaccount(&root, "mallory", 10).await?;
    let naive = subaccount(&root, "naive", 20)
        .await?
        .deploy(&naive_wasm())
        .await?
        .into_result()?;
    naive
        .call("new")
        .args_json(json!({
            "owner_id": env.owner.id(),
            "agent_id": env.agent_full.id(),
            "approver_id": env.approver.id(),
            "slot_ns": HOUR_NS.to_string(),
        }))
        .transact()
        .await?
        .into_result()?;
    for account in [api.id(), mallory.id(), naive.id()] {
        env.owner
            .call(env.tst.id(), "storage_deposit")
            .args_json(json!({ "account_id": account }))
            .deposit(NearToken::from_millinear(10))
            .transact()
            .await?
            .into_result()?;
    }
    env.owner
        .call(env.tst.id(), "ft_transfer")
        .args_json(json!({ "receiver_id": naive.id(), "amount": (1_000 * TST).to_string() }))
        .deposit(ONE_YOCTO)
        .transact()
        .await?
        .into_result()?;
    let token = tst_token(&env);
    let policy = json!({
        "token": token,
        "per_payment_max": PER_PAYMENT_MAX.to_string(),
        "daily_max": DAILY_MAX.to_string(),
        "approval_above": APPROVAL_ABOVE.to_string(),
    });
    let allow_api = json!({ "token": token, "receiver_id": api.id(), "ft_call": false });
    for (method, args) in [("set_token", policy), ("allow_receiver", allow_api.clone())] {
        env.owner
            .call(naive.id(), method)
            .args_json(args)
            .deposit(ONE_YOCTO)
            .transact()
            .await?
            .into_result()?;
    }
    owner_call(&env, "allow_receiver", allow_api)
        .await?
        .into_result()?;
    let naive_agent = limited_key(
        &env.worker,
        &env.agent_full,
        naive.id(),
        &["request_payment"],
    )
    .await?;
    Ok(Demo {
        env,
        api,
        mallory,
        naive,
        naive_agent,
    })
}

/// TST with its decimals, trailing zeros dropped: 250, 12.5.
fn tst(amount: u128) -> String {
    let fraction = format!("{:06}", amount % TST);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        (amount / TST).to_string()
    } else {
        format!("{}.{fraction}", amount / TST)
    }
}

/// Prints a verdict line held to the README's conventions: plain ASCII and
/// under 100 columns, so a recording at 120 columns never wraps.
fn say(line: &str) {
    assert!(
        line.is_ascii() && line.len() < 100,
        "verdict line breaks the conventions: {line:?}"
    );
    println!("{line}");
}

/// `request_payment` from `agent` to `guard`.
async fn request_on(
    demo: &Demo,
    guard: &AccountId,
    agent: &Account,
    receiver: &AccountId,
    amount: u128,
) -> anyhow::Result<Value> {
    Ok(agent
        .call(guard, "request_payment")
        .args_json(json!({
            "token": tst_token(&demo.env),
            "receiver_id": receiver,
            "amount": amount.to_string(),
            "msg": null,
            "expires_at": expiry(&demo.env).await?,
        }))
        .gas(AGENT_GAS)
        .transact()
        .await?
        .json::<Value>()?)
}

/// Ten requests of 25 TST to the API provider, all broadcast before any is
/// polled, after one refused request that warms the key's nonce cache (see
/// `race_guard_exact`). Returns each request's final result.
async fn burst(
    demo: &Demo,
    guard: &AccountId,
    agent: &Account,
) -> anyhow::Result<Vec<ExecutionFinalResult>> {
    let warm = request_on(demo, guard, agent, demo.mallory.id(), 25 * TST).await?;
    assert_eq!(warm["reason"], "receiver_not_allowed");
    let expires_at = expiry(&demo.env).await?;
    let mut sent = Vec::new();
    for _ in 0..10 {
        sent.push(
            agent
                .call(guard, "request_payment")
                .args_json(json!({
                    "token": tst_token(&demo.env),
                    "receiver_id": demo.api.id(),
                    "amount": (25 * TST).to_string(),
                    "msg": null,
                    "expires_at": expires_at,
                }))
                .gas(BURST_GAS)
                .transact_async()
                .await?,
        );
    }
    wait_all(&sent).await
}

/// The amount that left `guard` while `f` ran.
async fn left_during<F: Future<Output = anyhow::Result<T>>, T>(
    demo: &Demo,
    guard: &AccountId,
    f: F,
) -> anyhow::Result<(u128, T)> {
    let before = balance(&demo.env, guard).await?;
    let out = f.await?;
    Ok((before - balance(&demo.env, guard).await?, out))
}

/// One race on fresh contracts: what left the naive guard, what left ours, how
/// many of our requests wait for a human, and how many of our requests ran
/// before the first callback.
async fn race_once() -> anyhow::Result<(u128, u128, usize, usize)> {
    let demo = demo_setup().await?;
    let guard = demo.env.guard.id();
    let (naive_left, _) = left_during(
        &demo,
        demo.naive.id(),
        burst(&demo, demo.naive.id(), &demo.naive_agent),
    )
    .await?;
    let (ours_left, results) =
        left_during(&demo, guard, burst(&demo, guard, &demo.env.agent)).await?;
    let (parallel, _, _) = requests_before_first_callback(&demo.env, &results).await?;
    let waiting = results
        .into_iter()
        .map(|r| r.json::<Value>().unwrap())
        .filter(|v| v["status"] == "pending" && v["reason"] == "daily_cap")
        .count();
    Ok((naive_left, ours_left, waiting, parallel))
}

/// Attempts at a burst that is really parallel. On a busy machine the first
/// callback can land before five of our requests ran, often several runs in a
/// row; such a run didn't test concurrency, so it is repeated on fresh
/// contracts rather than counted. Each attempt sets up from scratch (about 30
/// seconds), which also spaces them out.
const RACE_ATTEMPTS: usize = 5;

#[tokio::test]
async fn race() -> anyhow::Result<()> {
    say("race: daily cap 100 TST, 10 payments of 25 TST sent in parallel");
    for attempt in 1..=RACE_ATTEMPTS {
        let (naive_left, ours_left, waiting, parallel) = race_once().await?;
        // Ours is exact on every attempt, parallel or not.
        assert_eq!(
            (ours_left, waiting),
            (DAILY_MAX, 6),
            "attempt {attempt}: our guard let {} TST leave",
            tst(ours_left)
        );
        // The naive number depends on block timing, so only "more than the
        // cap" is required, and only of a burst that really was parallel.
        if parallel >= 5 && naive_left > DAILY_MAX {
            say(&format!(
                "naive guard        {} TST left (cap 100)",
                tst(naive_left)
            ));
            say(&format!(
                "near-spend-guard   {} TST left, {waiting} waiting for a human (daily_cap)",
                tst(ours_left)
            ));
            return Ok(());
        }
        eprintln!(
            "race: attempt {attempt} wasn't parallel ({parallel} of 10 before the first callback); rerunning"
        );
    }
    anyhow::bail!("no parallel burst in {RACE_ATTEMPTS} attempts")
}

#[tokio::test]
async fn price_swap() -> anyhow::Result<()> {
    let demo = demo_setup().await?;
    let (env, api) = (&demo.env, demo.api.id());
    say("price_swap: the human agreed to 50 TST; the filed request says 190 TST");

    // The naive guard: the approver signs the request number.
    let filed = request_on(&demo, demo.naive.id(), &demo.naive_agent, api, 190 * TST).await?;
    assert_eq!(filed["reason"], "above_threshold");
    let id = filed["request_id"].as_str().unwrap().to_string();
    assert_eq!(id, "1", "a fresh guard numbers requests from 1");
    let api_before = balance(env, api).await?;
    env.approver
        .call(demo.naive.id(), "approve")
        .args_json(json!({ "request_id": id }))
        .deposit(ONE_YOCTO)
        .gas(APPROVER_GAS)
        .transact()
        .await?
        .into_result()?;
    let received = balance(env, api).await? - api_before;
    say(&format!(
        "naive guard        approve({id}): api received {} TST",
        tst(received)
    ));
    assert_eq!(received, 190 * TST);

    // Ours: the approver signs the payment they agreed to, 50 TST, and the
    // guard compares it with the stored one.
    let filed = request_on(&demo, env.guard.id(), &env.agent, api, 190 * TST).await?;
    assert_eq!(filed["reason"], "above_threshold");
    let id = filed["request_id"].as_str().unwrap().to_string();
    assert_eq!(id, "1", "a fresh guard numbers requests from 1");
    let stored = view(env, "get_request", json!({ "request_id": id })).await?["payload"].clone();
    assert_eq!(
        stored["amount"],
        (190 * TST).to_string(),
        "what a wallet shows"
    );
    let mut agreed = stored.clone();
    agreed["amount"] = json!((50 * TST).to_string());
    let (moved, result) = left_during(&demo, env.guard.id(), async {
        Ok(env
            .approver
            .call(env.guard.id(), "approve")
            .args_json(json!({ "payload": agreed }))
            .deposit(ONE_YOCTO)
            .gas(APPROVER_GAS)
            .transact()
            .await?)
    })
    .await?;
    let refusal = format!("payload does not match request {id}");
    assert!(failure_text(&result).contains(&refusal), "{result:?}");
    say(&format!(
        "near-spend-guard   approve(payload for 50): refused, {refusal}; {} TST moved",
        tst(moved)
    ));
    assert_eq!(moved, 0);
    // Still pending, unchanged: the approver can reject it or approve the 190.
    let still = view(env, "get_request", json!({ "request_id": id })).await?;
    assert_eq!(still["payload"], stored);
    Ok(())
}

/// The `payment_dispatched` event's mode in a payment's transaction.
fn dispatched_mode(result: &ExecutionFinalResult) -> Value {
    let events = guard_events(result.logs());
    let event = events
        .iter()
        .find(|e| e["event"] == "payment_dispatched")
        .unwrap_or_else(|| panic!("no payment_dispatched in {events:?}"));
    event["data"]["mode"].clone()
}

#[tokio::test]
async fn approval_flow() -> anyhow::Result<()> {
    let demo = demo_setup().await?;
    let (env, api) = (&demo.env, demo.api.id());
    say("approval_flow: an agent pays for an API through near-spend-guard");
    let api_before = balance(env, api).await?;

    let small = request(env, api, 5 * TST, None).await?;
    let mode = dispatched_mode(&small);
    let small = small.json::<Value>()?;
    say(&format!(
        "pay 5 TST to api        {} ({})",
        small["status"].as_str().unwrap(),
        mode.as_str().unwrap()
    ));
    assert_eq!(
        (small["status"].clone(), mode),
        (json!("paid"), json!("autonomous"))
    );

    let refused = request(env, demo.mallory.id(), 500 * TST, None)
        .await?
        .json::<Value>()?;
    say(&format!(
        "pay 500 TST to mallory  refused ({})",
        refused["reason"].as_str().unwrap()
    ));
    assert_eq!(refused["reason"], "receiver_not_allowed");

    let pending = request(env, api, 60 * TST, None).await?.json::<Value>()?;
    let id = pending["request_id"].as_str().unwrap().to_string();
    let hash = pending["payload_hash"].as_str().unwrap();
    say(&format!(
        "pay 60 TST to api       {} ({}), request {id}, hash {}...",
        pending["status"].as_str().unwrap(),
        pending["reason"].as_str().unwrap(),
        &hash[..8]
    ));
    assert_eq!(pending["reason"], "above_threshold");
    assert_eq!(id, "3", "refused requests use up ids too");

    // The approver reads the payment from the guard, not from the agent, and
    // signs exactly what is stored.
    let entry = view(env, "get_request", json!({ "request_id": id })).await?;
    assert_eq!(entry["payload_hash"], hash);
    let approved = env
        .approver
        .call(env.guard.id(), "approve")
        .args_json(json!({ "payload": entry["payload"] }))
        .deposit(ONE_YOCTO)
        .gas(APPROVER_GAS)
        .transact()
        .await?;
    let mode = dispatched_mode(&approved);
    let approved = approved.into_result()?.json::<Value>()?;
    say(&format!(
        "approve request {id}       {} {} TST ({})",
        approved["status"].as_str().unwrap(),
        tst(approved["sent"].as_str().unwrap().parse()?),
        mode.as_str().unwrap()
    ));
    assert_eq!(
        (approved["status"].clone(), approved["sent"].clone(), mode),
        (
            json!("paid"),
            json!((60 * TST).to_string()),
            json!("approved")
        )
    );
    assert_eq!(balance(env, api).await? - api_before, 65 * TST);
    assert_eq!(
        remaining(env).await?,
        DAILY_MAX - 5 * TST,
        "approved payments aren't counted against the cap"
    );
    Ok(())
}
