//! Set-up shared by the sandbox tests: a fresh sandbox per test, the guard
//! and the test token deployed from `target/`, and the roles from the spec.
//!
//! The contracts are built by `scripts/build-wasm.sh` (run by
//! `scripts/test-sandbox.sh`). A wasm file older than its crate's sources
//! stops the test, so a test never runs against yesterday's contract.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::task::Poll;
use std::time::{Duration, Instant, SystemTime};

use near_workspaces::network::Sandbox;
use near_workspaces::operations::TransactionStatus;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{AccessKey, Gas, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{Value, json};

/// TST has 6 decimals.
pub const TST: u128 = 1_000_000;
pub const HOUR_NS: u64 = 3_600_000_000_000;
/// What the agent attaches to `request_payment` (INTERFACE.md).
pub const AGENT_GAS: Gas = Gas::from_tgas(100);
pub const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

/// The demo's policy for TST (DEMO.md): at most 200 per payment, a human
/// above 40, 100 a day without one.
pub const PER_PAYMENT_MAX: u128 = 200 * TST;
pub const DAILY_MAX: u128 = 100 * TST;
pub const APPROVAL_ABOVE: u128 = 40 * TST;

pub struct Env {
    pub worker: Worker<Sandbox>,
    pub guard: Contract,
    pub tst: Contract,
    /// A second deployment of the test token, as the `ft_transfer_call`
    /// receiver.
    pub shop: Contract,
    pub owner: Account,
    /// The agent's account with its full-access key: only for checks that
    /// need one, such as attaching a deposit.
    pub agent_full: Account,
    /// The agent as deployed: a function-call key on its account, limited
    /// to `request_payment` on the guard.
    pub agent: Account,
    pub approver: Account,
    pub alice: Account,
}

/// The newest contract build: `target/wasm32-unknown-unknown/release/<name>.wasm`.
pub fn wasm(crate_dir: &str, name: &str) -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let path = target
        .join("wasm32-unknown-unknown/release")
        .join(format!("{name}.wasm"));
    let built = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .unwrap_or_else(|e| panic!("{}: {e}; run scripts/build-wasm.sh", path.display()));
    let sources = newest(&root.join(crate_dir));
    assert!(
        built >= sources,
        "{} is older than {crate_dir}; run scripts/build-wasm.sh",
        path.display()
    );
    std::fs::read(&path).unwrap()
}

fn newest(dir: &Path) -> SystemTime {
    let mut latest = SystemTime::UNIX_EPOCH;
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let meta = entry.metadata().unwrap();
        let time = if meta.is_dir() {
            newest(&entry.path())
        } else {
            meta.modified().unwrap()
        };
        latest = latest.max(time);
    }
    latest
}

pub fn guard_wasm() -> Vec<u8> {
    wasm("contracts/guard", "near_spend_guard")
}

pub fn token_wasm() -> Vec<u8> {
    wasm("contracts/test-token", "test_token")
}

pub fn naive_wasm() -> Vec<u8> {
    wasm("contracts/naive-guard", "naive_guard")
}

pub async fn subaccount(root: &Account, name: &str, near: u128) -> anyhow::Result<Account> {
    Ok(root
        .create_subaccount(name)
        .initial_balance(NearToken::from_near(near))
        .transact()
        .await?
        .into_result()?)
}

/// Adds a function-call key for `methods` on `contract` to `account` and
/// returns the account signing with it.
pub async fn limited_key(
    worker: &Worker<Sandbox>,
    account: &Account,
    contract: &AccountId,
    methods: &[&str],
) -> anyhow::Result<Account> {
    let sk = SecretKey::from_random(KeyType::ED25519);
    account
        .batch(account.id())
        .add_key(
            sk.public_key(),
            AccessKey::function_call_access(contract, methods, Some(NearToken::from_near(1))),
        )
        .transact()
        .await?
        .into_result()?;
    Ok(Account::from_secret_key(account.id().clone(), sk, worker))
}

pub fn tst_token(env: &Env) -> Value {
    json!({ "ft": env.tst.id() })
}

/// A sandbox with every role, the guard initialised with one-hour slots,
/// TST configured with the demo's policy and Alice allowed, the guard holding
/// 1,000 TST, and Alice and the shop registered with the token.
pub async fn setup() -> anyhow::Result<Env> {
    setup_with_slot(HOUR_NS).await
}

pub async fn setup_with_slot(slot_ns: u64) -> anyhow::Result<Env> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let owner = subaccount(&root, "owner", 50).await?;
    let agent_full = subaccount(&root, "agent", 10).await?;
    let approver = subaccount(&root, "approver", 10).await?;
    let alice = subaccount(&root, "alice", 10).await?;

    let guard = subaccount(&root, "guard", 20)
        .await?
        .deploy(&guard_wasm())
        .await?
        .into_result()?;
    guard
        .call("new")
        .args_json(json!({
            "owner_id": owner.id(),
            "agent_id": agent_full.id(),
            "approver_id": approver.id(),
            "slot_ns": slot_ns.to_string(),
        }))
        .transact()
        .await?
        .into_result()?;

    let token_code = token_wasm();
    let tst = subaccount(&root, "tst", 20)
        .await?
        .deploy(&token_code)
        .await?
        .into_result()?;
    tst.call("new")
        .args_json(json!({ "owner_id": owner.id(), "total_supply": (1_000_000 * TST).to_string() }))
        .transact()
        .await?
        .into_result()?;
    let shop = subaccount(&root, "shop", 20)
        .await?
        .deploy(&token_code)
        .await?
        .into_result()?;
    shop.call("new")
        .args_json(json!({ "owner_id": owner.id(), "total_supply": "0" }))
        .transact()
        .await?
        .into_result()?;

    for account in [guard.id(), alice.id(), shop.id()] {
        owner
            .call(tst.id(), "storage_deposit")
            .args_json(json!({ "account_id": account }))
            .deposit(NearToken::from_millinear(10))
            .transact()
            .await?
            .into_result()?;
    }
    owner
        .call(tst.id(), "ft_transfer")
        .args_json(json!({ "receiver_id": guard.id(), "amount": (1_000 * TST).to_string() }))
        .deposit(ONE_YOCTO)
        .transact()
        .await?
        .into_result()?;

    let agent = limited_key(&worker, &agent_full, guard.id(), &["request_payment"]).await?;
    let env = Env {
        worker,
        guard,
        tst,
        shop,
        owner,
        agent_full,
        agent,
        approver,
        alice,
    };
    owner_call(
        &env,
        "set_token",
        json!({
            "token": tst_token(&env),
            "per_payment_max": PER_PAYMENT_MAX.to_string(),
            "daily_max": DAILY_MAX.to_string(),
            "approval_above": APPROVAL_ABOVE.to_string(),
        }),
    )
    .await?
    .into_result()?;
    allow(&env, env.alice.id(), false).await?;
    Ok(env)
}

/// An owner configuration call, with the 1 yocto it needs.
pub async fn owner_call(
    env: &Env,
    method: &str,
    args: Value,
) -> anyhow::Result<ExecutionFinalResult> {
    Ok(env
        .owner
        .call(env.guard.id(), method)
        .args_json(args)
        .deposit(ONE_YOCTO)
        .transact()
        .await?)
}

pub async fn allow(env: &Env, receiver: &AccountId, ft_call: bool) -> anyhow::Result<()> {
    owner_call(
        env,
        "allow_receiver",
        json!({ "token": tst_token(env), "receiver_id": receiver, "ft_call": ft_call }),
    )
    .await?
    .into_result()?;
    Ok(())
}

/// Ten minutes after the latest block.
pub async fn expiry(env: &Env) -> anyhow::Result<String> {
    let now = env.worker.view_block().await?.timestamp();
    Ok((now + 600_000_000_000).to_string())
}

/// The agent asks for `amount` TST base units to `receiver`, from its
/// function-call key.
pub async fn request(
    env: &Env,
    receiver: &AccountId,
    amount: u128,
    msg: Option<&str>,
) -> anyhow::Result<ExecutionFinalResult> {
    Ok(env
        .agent
        .call(env.guard.id(), "request_payment")
        .args_json(json!({
            "token": tst_token(env),
            "receiver_id": receiver,
            "amount": amount.to_string(),
            "msg": msg,
            "expires_at": expiry(env).await?,
        }))
        .gas(AGENT_GAS)
        .transact()
        .await?)
}

pub async fn view(env: &Env, method: &str, args: Value) -> anyhow::Result<Value> {
    Ok(env
        .guard
        .view(method)
        .args_json(args)
        .await?
        .json::<Value>()?)
}

pub async fn remaining(env: &Env) -> anyhow::Result<u128> {
    let v = view(env, "remaining_budget", json!({ "token": tst_token(env) })).await?;
    Ok(v.as_str().unwrap().parse()?)
}

pub async fn balance(env: &Env, account: &AccountId) -> anyhow::Result<u128> {
    let v = env
        .tst
        .view("ft_balance_of")
        .args_json(json!({ "account_id": account }))
        .await?
        .json::<Value>()?;
    Ok(v.as_str().unwrap().parse()?)
}

/// The failure text of a receipt that failed, for matching panic messages.
pub fn failure_text(result: &ExecutionFinalResult) -> String {
    assert!(result.is_failure(), "expected a failure, got {result:?}");
    format!("{:?}", result.clone().into_result().unwrap_err())
}

/// The guard's events (`EVENT_JSON:` logs) in a transaction, parsed. Takes
/// the result's `logs()`.
pub fn guard_events(logs: Vec<&str>) -> Vec<Value> {
    logs.iter()
        .filter_map(|log| log.strip_prefix("EVENT_JSON:"))
        .map(|json| serde_json::from_str::<Value>(json).unwrap())
        .filter(|event| event["standard"] == "near-spend-guard")
        .collect()
}

/// Gas for each request in the burst. nearcore fills a chunk until the
/// transactions' prepaid gas would pass its 1,000 TGas limit; at the agent's
/// usual 100 TGas only nine of ten fitted, and the tenth was never included
/// (3 Nov: its status never resolved, and the key's next transaction skipped
/// its nonce). 40 TGas covers what `request_payment` checks for an
/// `ft_transfer` (25) plus its own work, and ten fit in one chunk.
pub const BURST_GAS: Gas = Gas::from_tgas(40);

/// Polls every transaction until it's final, and fails with the hash of one
/// that isn't within 30 seconds rather than waiting on it forever. A status
/// query for a transaction the node never includes blocks until the RPC
/// client times out, about 50 seconds, so each query gets 3.
pub async fn wait_all(sent: &[TransactionStatus]) -> anyhow::Result<Vec<ExecutionFinalResult>> {
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

/// The height of the block in which the guard logged `event` for this
/// transaction.
pub async fn height_of(
    env: &Env,
    result: &ExecutionFinalResult,
    event: &str,
) -> anyhow::Result<u64> {
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

/// How many of a burst's requests the guard ran before the first payment's
/// callback, with the block heights of the requests and of the callbacks:
/// each paid request is placed by its `payment_dispatched` event, each held
/// one by its `approval_requested`.
pub async fn requests_before_first_callback(
    env: &Env,
    results: &[ExecutionFinalResult],
) -> anyhow::Result<(usize, Vec<u64>, Vec<u64>)> {
    let mut requested = Vec::new();
    let mut settled = Vec::new();
    for result in results {
        let paid = result.clone().json::<Value>()?["status"] == "paid";
        let event = if paid {
            "payment_dispatched"
        } else {
            "approval_requested"
        };
        requested.push(height_of(env, result, event).await?);
        if paid {
            settled.push(height_of(env, result, "payment_settled").await?);
        }
    }
    let first_callback = *settled.iter().min().expect("some payments");
    let before = requested.iter().filter(|&&h| h < first_callback).count();
    Ok((before, requested, settled))
}
