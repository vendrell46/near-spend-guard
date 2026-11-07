//! The client against the real contracts: `nsg-pay-mcp` over stdio, signing
//! with the agent's function-call key, and `nsg-approve` reading the guard.
//! Only near CLI is a stand-in: a script that records its arguments, which the
//! test then sends as the approver, so what's approved is what the helper built.
//!
//! These need Node and `client/node_modules`, which the contract tests don't,
//! so they are ignored in a plain sandbox run. `scripts/test-client.sh` runs
//! them.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use common::*;
use near_workspaces::types::Gas;
use serde_json::{Value, json};

const MALLORY: &str = "mallory.test.near";

fn client_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../client")
}

fn node() -> Command {
    let mut command = Command::new("node");
    command.current_dir(client_dir()).env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command
}

/// The MCP server, spoken to line by line.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn start(env: &Env, key: String) -> anyhow::Result<Self> {
        let mut child = node()
            .arg("bin/nsg-pay-mcp.ts")
            .env("NSG_GUARD_ID", env.guard.id().as_str())
            .env("NSG_AGENT_ID", env.agent.id().as_str())
            .env("NSG_RPC_URL", env.worker.rpc_addr())
            .env("NSG_AGENT_KEY", key)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (send, lines) = channel();
        std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            lines,
            next_id: 0,
        })
    }

    fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{message}")?;
        // Longer than the client's own 30 s deadline.
        let line = self.lines.recv_timeout(Duration::from_secs(60))?;
        let reply: Value = serde_json::from_str(&line)?;
        assert_eq!(reply["id"], id, "{reply}");
        Ok(reply)
    }

    /// `pay`'s text, and whether it came back as a tool error.
    fn pay(&mut self, arguments: Value) -> anyhow::Result<(String, bool)> {
        let reply = self.request(
            "tools/call",
            json!({ "name": "pay", "arguments": arguments }),
        )?;
        let result = &reply["result"];
        let text = result["content"][0]["text"].as_str().unwrap().to_string();
        println!("pay -> {text}");
        Ok((text, result["isError"].as_bool().unwrap()))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The approver's machine: their config, and a `near` that writes down its
/// arguments.
struct Approver {
    dir: PathBuf,
}

impl Approver {
    fn new(env: &Env, name: &str) -> anyhow::Result<Self> {
        let dir = std::env::temp_dir().join(format!("nsg-e2e-{}-{name}", std::process::id()));
        std::fs::create_dir_all(dir.join("near-spend-guard"))?;
        let near = dir.join("near");
        std::fs::write(
            &near,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n",
                dir.join("argv").display()
            ),
        )?;
        Command::new("chmod").arg("+x").arg(&near).status()?;
        std::fs::write(
            dir.join("near-spend-guard/approver.json"),
            json!({
                "guard_id": env.guard.id(),
                "approver_id": env.approver.id(),
                "network": "sandbox",
                "rpc_url": env.worker.rpc_addr(),
                "near_cli": near,
            })
            .to_string(),
        )?;
        Ok(Self { dir })
    }

    /// Runs `nsg-approve` and returns what it printed and near CLI's arguments.
    fn run(&self, request_id: &str) -> anyhow::Result<(String, Vec<String>)> {
        let output = node()
            .arg("bin/nsg-approve.ts")
            .arg(request_id)
            .env("XDG_CONFIG_HOME", &self.dir)
            .output()?;
        let printed = String::from_utf8(output.stdout)?;
        assert!(
            output.status.success(),
            "{printed}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let argv = std::fs::read_to_string(self.dir.join("argv"))?;
        Ok((printed, argv.lines().map(String::from).collect()))
    }
}

impl Drop for Approver {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn after<'a>(argv: &'a [String], flag: &str) -> &'a str {
    let at = argv.iter().position(|a| a == flag).unwrap();
    &argv[at + 1]
}

/// The id the node has for `hash`, asked over plain JSON-RPC.
fn transaction_id(env: &Env, hash: &str) -> anyhow::Result<String> {
    let body = json!({
        "jsonrpc": "2.0", "id": "nsg", "method": "tx",
        "params": { "tx_hash": hash, "sender_account_id": env.agent.id() },
    });
    let output = Command::new("curl")
        .args(["-s", "-H", "content-type: application/json", "-d"])
        .arg(body.to_string())
        .arg(env.worker.rpc_addr())
        .output()?;
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    Ok(reply["result"]["transaction_outcome"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

fn request_number(text: &str) -> String {
    let rest = text.split_once("request ").unwrap().1;
    rest.split_whitespace().next().unwrap().to_string()
}

#[test]
fn client_gas_is_what_these_tests_use() {
    let text = std::fs::read_to_string(client_dir().join("src/gas.json")).unwrap();
    let gas: Value = serde_json::from_str(&text).unwrap();
    let tgas = |name: &str| Gas::from_tgas(gas[name].as_u64().unwrap());
    assert_eq!(tgas("native"), NATIVE_GAS);
    assert_eq!(tgas("ft_transfer"), FT_TRANSFER_GAS);
    assert_eq!(tgas("ft_transfer_call"), FT_TRANSFER_CALL_GAS);
    assert_eq!(tgas("approve"), Gas::from_tgas(100));
}

#[tokio::test]
#[ignore = "needs Node and client/node_modules: scripts/test-client.sh"]
async fn agent_pays_and_the_approver_signs() -> anyhow::Result<()> {
    let env = setup().await?;
    allow(&env, env.shop.id(), true).await?;
    let token = tst_token(&env);
    let alice = env.alice.id().as_str();
    let mut mcp = Mcp::start(&env, env.agent.secret_key().to_string())?;

    let hello = mcp.request("initialize", json!({ "protocolVersion": "2025-06-18" }))?;
    assert_eq!(hello["result"]["protocolVersion"], "2025-06-18");
    let tools = mcp.request("tools/list", json!({}))?;
    assert_eq!(tools["result"]["tools"][0]["name"], "pay");

    // Small enough to go through alone.
    let before = balance(&env, env.alice.id()).await?;
    let (text, failed) = mcp.pay(json!({ "token": token, "receiver_id": alice, "amount": "5" }))?;
    assert!(!failed);
    assert!(text.starts_with(&format!("paid: 5 TST to {alice} (request 1)")));
    assert_eq!(balance(&env, env.alice.id()).await? - before, 5 * TST);
    assert_eq!(remaining(&env).await?, DAILY_MAX - 5 * TST);

    // Someone the owner never allowed: an answer, not a tool error.
    let (text, failed) =
        mcp.pay(json!({ "token": token, "receiver_id": MALLORY, "amount": "1" }))?;
    assert!(!failed);
    assert!(text.starts_with("refused (receiver_not_allowed)"), "{text}");

    // ft_transfer_call, where the shop keeps part of it.
    let shop = env.shop.id().as_str();
    let (text, _) = mcp.pay(json!({
        "token": token, "receiver_id": shop, "amount": "5", "msg": "refund:2000000",
    }))?;
    assert!(
        text.starts_with(&format!("paid: 3 TST of 5 TST to {shop}")),
        "{text}"
    );

    // Input the guard would panic on never leaves the client.
    let (text, failed) =
        mcp.pay(json!({ "token": token, "receiver_id": alice, "amount": "0.0000001" }))?;
    assert!(failed);
    assert!(
        text.starts_with("not sent: amount has more decimals"),
        "{text}"
    );

    // A token the guard has no policy for.
    let (text, failed) = mcp.pay(json!({
        "token": "native", "receiver_id": alice, "amount": "0.5",
    }))?;
    assert!(!failed);
    assert!(text.starts_with("refused (token_not_configured)"), "{text}");

    // The guard panics on this one, and the transaction says so.
    let guard = env.guard.id().as_str();
    let (text, failed) = mcp.pay(json!({ "token": token, "receiver_id": guard, "amount": "1" }))?;
    assert!(failed);
    assert!(text.starts_with("rejected, nothing changed:"), "{text}");
    assert!(text.contains("the receiver cannot be the guard"), "{text}");

    // Nobody registered with the token: the transfer fails after the guard
    // reserved, and the budget comes back.
    allow(&env, env.approver.id(), false).await?;
    let before = remaining(&env).await?;
    let (text, failed) = mcp.pay(json!({
        "token": token, "receiver_id": env.approver.id(), "amount": "5",
    }))?;
    assert!(failed);
    assert!(
        text.starts_with("failed: nothing moved, 5 TST of budget came back"),
        "{text}"
    );
    assert_eq!(remaining(&env).await?, before);

    // The longest expiry the guard takes, from the chain's clock.
    let (text, _) = mcp.pay(json!({
        "token": token, "receiver_id": alice, "amount": "1", "expires_in_s": 604800,
    }))?;
    assert!(text.starts_with("paid: 1 TST"), "{text}");
    // The hash printed is the one the chain knows the transaction by.
    let printed_hash = text.rsplit_once("transaction ").unwrap().1.trim();
    assert_eq!(
        transaction_id(&env, printed_hash)?,
        printed_hash,
        "explorer links are built from this"
    );

    // Too big to go alone: it waits, and the agent can't take it further.
    let (text, failed) =
        mcp.pay(json!({ "token": token, "receiver_id": alice, "amount": "60" }))?;
    assert!(!failed);
    assert!(text.starts_with("pending (above_threshold)"), "{text}");
    assert!(
        text.contains("amount   60 TST (60000000 base units)"),
        "{text}"
    );
    assert!(!text.contains("WARNING"), "{text}");
    let id = request_number(&text);
    let hash = view(&env, "get_request", json!({ "request_id": id })).await?["payload_hash"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains(&hash), "{text}");

    // The approver asks the guard, not the agent.
    let approver = Approver::new(&env, "pays")?;
    let (printed, argv) = approver.run(&id)?;
    println!("{printed}");
    assert!(printed.contains("Pay       60 TST"), "{printed}");
    assert!(printed.contains(&format!("Hash      {hash}")), "{printed}");
    assert_eq!(argv[3], env.guard.id().as_str());
    assert_eq!(argv[4], "approve");
    assert_eq!(after(&argv, "attached-deposit"), "1 yoctoNEAR");
    assert_eq!(after(&argv, "sign-as"), env.approver.id().as_str());
    assert_eq!(after(&argv, "prepaid-gas"), "100 Tgas");
    assert!(!argv.iter().any(|a| a == "send"), "{argv:?}");

    // What near CLI would send, sent.
    let args: Value = serde_json::from_str(after(&argv, "json-args"))?;
    let before = balance(&env, env.alice.id()).await?;
    let approved = env
        .approver
        .call(env.guard.id(), &argv[4])
        .args_json(args)
        .deposit(ONE_YOCTO)
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    assert_eq!(approved.json::<Value>()?["status"], "paid");
    assert_eq!(balance(&env, env.alice.id()).await? - before, 60 * TST);
    // The human's payment left the agent's budget alone.
    assert_eq!(remaining(&env).await?, DAILY_MAX - 5 * TST - 3 * TST - TST);

    // ft_transfer_call needs the most gas of anything `approve` dispatches.
    let (text, _) = mcp.pay(json!({
        "token": token, "receiver_id": shop, "amount": "50", "msg": "keep",
    }))?;
    assert!(text.starts_with("pending (above_threshold)"), "{text}");
    let call_id = request_number(&text);
    let (printed, argv) = approver.run(&call_id)?;
    assert!(printed.contains("Method    ft_transfer_call"), "{printed}");
    assert!(printed.contains("Message   \"keep\""), "{printed}");
    let approved = env
        .approver
        .call(env.guard.id(), "approve")
        .args_json(serde_json::from_str::<Value>(after(&argv, "json-args"))?)
        .deposit(ONE_YOCTO)
        .gas(Gas::from_tgas(100))
        .transact()
        .await?;
    assert_eq!(approved.json::<Value>()?["sent"], (50 * TST).to_string());

    // The request is gone, so the helper has nothing to start near CLI with.
    let gone = node()
        .arg("bin/nsg-approve.ts")
        .arg(&id)
        .env("XDG_CONFIG_HOME", &approver.dir)
        .output()?;
    assert_eq!(gone.status.code(), Some(1));
    let stderr = String::from_utf8(gone.stderr)?;
    assert!(
        stderr.contains(&format!("no pending request {id}")),
        "{stderr}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "needs Node and client/node_modules: scripts/test-client.sh"]
async fn pay_keeps_working_when_many_calls_arrive_at_once() -> anyhow::Result<()> {
    let env = setup().await?;
    let token = tst_token(&env);
    let alice = env.alice.id().as_str();
    let mut mcp = Mcp::start(&env, env.agent.secret_key().to_string())?;
    let before = balance(&env, env.alice.id()).await?;

    // Written before any answer is read, as a host with parallel tool calls
    // would. Nonces must not collide, and the cap must hold through the client.
    for id in 1..=6 {
        let message = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": "pay", "arguments":
                { "token": token, "receiver_id": alice, "amount": "25" } },
        });
        writeln!(mcp.stdin, "{message}")?;
    }
    let mut statuses = Vec::new();
    for _ in 0..6 {
        let line = mcp.lines.recv_timeout(Duration::from_secs(60))?;
        let reply: Value = serde_json::from_str(&line)?;
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        println!("{text}");
        statuses.push(text.split([':', ' ']).next().unwrap().to_string());
    }
    statuses.sort();
    assert_eq!(
        statuses,
        ["paid", "paid", "paid", "paid", "pending", "pending"]
    );
    assert_eq!(balance(&env, env.alice.id()).await? - before, 100 * TST);
    Ok(())
}

#[tokio::test]
#[ignore = "needs Node and client/node_modules: scripts/test-client.sh"]
async fn a_second_process_on_the_key_costs_one_answer() -> anyhow::Result<()> {
    let env = setup().await?;
    let key = env.agent.secret_key().to_string();
    let args = json!({ "token": tst_token(&env), "receiver_id": env.alice.id(), "amount": "1" });
    let mut first = Mcp::start(&env, key.clone())?;
    let mut second = Mcp::start(&env, key)?;

    assert!(first.pay(args.clone())?.0.starts_with("paid"));
    assert!(second.pay(args.clone())?.0.starts_with("paid"));
    assert!(second.pay(args.clone())?.0.starts_with("paid"));
    // `first` still counts from its own last nonce. It can't know whether its
    // transaction ran, so it says so and reads the nonce again.
    let (text, failed) = first.pay(args.clone())?;
    assert!(failed);
    assert!(text.starts_with("unknown: "), "{text}");
    assert!(first.pay(args)?.0.starts_with("paid"));
    assert_eq!(remaining(&env).await?, DAILY_MAX - 4 * TST);
    Ok(())
}

#[tokio::test]
#[ignore = "needs Node and client/node_modules: scripts/test-client.sh"]
async fn the_server_refuses_a_key_that_can_do_more() -> anyhow::Result<()> {
    let env = setup().await?;
    let mut mcp = Mcp::start(&env, env.agent_full.secret_key().to_string())?;
    let status = mcp.child.wait()?;
    assert_eq!(status.code(), Some(1));
    let mut stderr = String::new();
    std::io::Read::read_to_string(mcp.child.stderr.as_mut().unwrap(), &mut stderr)?;
    assert!(stderr.contains("refusing to run"), "{stderr}");
    Ok(())
}
