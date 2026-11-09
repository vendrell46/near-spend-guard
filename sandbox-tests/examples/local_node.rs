//! A local node that stays up, with the demo's accounts, so a real MCP host
//! (Claude Code) can use `nsg-pay-mcp` without testnet. `scripts/local-node.sh`
//! runs it; `scripts/local-agent.sh` points Claude Code at it.
//!
//! The world is the sandbox tests' (`setup()`): the guard holding 1,000 TST
//! under the demo's policy, plus `api` and `bob` allowed and `mallory`
//! registered with the token but not allowed (DEMO.md). It writes, into
//! `$NSG_LOCAL_DIR`:
//!
//! - `agent.env`: the `pay` server's settings, the agent's key among them
//!   (mode 0600; removed on exit). The key can call nothing but
//!   `request_payment` on the guard.
//! - `mcp.json`: an MCP config that starts the server with `node --env-file`,
//!   so the key reaches the server and never the host's environment.
//! - `world.json`: the account names, for prompts.
//! - `config/near-spend-guard/approver.json` and `near`: `nsg-approve`'s
//!   config (`XDG_CONFIG_HOME=$NSG_LOCAL_DIR/config`) and a stand-in for near
//!   CLI. The stand-in asks before signing, then hands its arguments to this
//!   process, which signs them as the approver only if they are exactly what
//!   `nsg-approve` builds (`client/src/approve.ts`, `nearArgs`).
//!
//! It stops when a file named `stop` appears in the directory, or after
//! `NSG_LOCAL_MINUTES` (default 120), and the sandbox stops with it.

#[path = "../tests/common/mod.rs"]
mod common;

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use common::*;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId};
use serde_json::{Value, json};

/// Matches `APPROVE_TGAS` in `client/src/gas.json`.
const APPROVE_GAS: Gas = Gas::from_tgas(100);

fn client_bin(name: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../client/bin")
        .join(name);
    Ok(path.canonicalize()?)
}

/// Creates `path` with `contents`, refusing to replace anything already there.
fn create(path: &Path, contents: &str, mode: u32) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .with_context(|| format!("{}: not created", path.display()))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

/// A single-quoted shell word.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn stand_in(dir: &Path, approver: &AccountId) -> String {
    let dir = sh_quote(&dir.display().to_string());
    let approver = sh_quote(approver.as_str());
    format!(
        r#"#!/bin/sh
# near CLI's stand-in on a local node (sandbox-tests/examples/local_node.rs).
# Like near CLI, it asks before anything is signed. Then it hands its arguments
# to the local node, which signs them as the approver only if they are exactly
# what nsg-approve builds, and prints what came of it.
set -eu
DIR={dir}
printf '\nlocal node: sign this as %s and send it? [y/N] ' {approver}
read -r ANSWER || ANSWER=
case "$ANSWER" in
    y | Y | yes) ;;
    *) echo "not signed"; exit 1 ;;
esac
rm -f "$DIR/near-result"
TMP="$DIR/near-argv.$$"
for a in "$@"; do printf '%s\0' "$a"; done > "$TMP"
mv "$TMP" "$DIR/near-argv"
i=0
while [ ! -f "$DIR/near-result" ]; do
    i=$((i + 1))
    if [ "$i" -gt 600 ]; then echo "no answer from the local node" >&2; exit 1; fi
    sleep 0.1
done
cat "$DIR/near-result"
rm -f "$DIR/near-result"
"#
    )
}

/// The approval the stand-in was given, if it is exactly `nearArgs`'s shape
/// (`client/src/approve.ts`) for this guard and approver. The local node
/// signs with its own deposit and gas, so this check is what keeps a local
/// run honest: if `nsg-approve` ever builds something else, it fails here
/// instead of being signed as something it isn't.
fn approval_args(argv: &[String], guard: &str, approver: &str) -> anyhow::Result<Value> {
    let fixed = [
        (0, "contract"),
        (1, "call-function"),
        (2, "as-transaction"),
        (3, guard),
        (4, "approve"),
        (5, "json-args"),
        (7, "prepaid-gas"),
        (8, "100 Tgas"),
        (9, "attached-deposit"),
        (10, "1 yoctoNEAR"),
        (11, "sign-as"),
        (12, approver),
        (13, "network-config"),
        (14, "sandbox"),
    ];
    if argv.len() != 15 {
        bail!("{} arguments, expected 15", argv.len());
    }
    for (at, expected) in fixed {
        if argv[at] != expected {
            bail!("argument {at} is {:?}, expected {expected:?}", argv[at]);
        }
    }
    let args: Value = serde_json::from_str(&argv[6]).context("json-args")?;
    if args.as_object().map(|o| o.len()) != Some(1) || !args["payload"].is_object() {
        bail!("json-args must be {{\"payload\": {{...}}}} and nothing else");
    }
    Ok(args)
}

/// Signs and sends what the stand-in was given; the text it prints back.
async fn sign(env: &Env, argv: &[String]) -> String {
    let args = match approval_args(argv, env.guard.id().as_str(), env.approver.id().as_str()) {
        Ok(args) => args,
        Err(e) => return format!("refused to sign: {e}\n"),
    };
    let sent = env
        .approver
        .call(env.guard.id(), "approve")
        .args_json(args)
        .deposit(NearToken::from_yoctonear(1))
        .gas(APPROVE_GAS)
        .transact()
        .await;
    match sent {
        Err(e) => format!("not sent: {e}\n"),
        Ok(result) => {
            let hash = result.outcome().transaction_hash.to_string();
            let mut text = format!("transaction {hash}\n");
            for event in guard_events(result.logs()) {
                text.push_str(&format!("event {event}\n"));
            }
            match result.into_result() {
                Ok(done) => {
                    let value = done.json::<Value>().unwrap_or(Value::Null);
                    text.push_str(&format!("approve returned {value}\n"));
                }
                Err(e) => text.push_str(&format!("failed: {e}\n")),
            }
            text
        }
    }
}

/// The arguments the stand-in left, if any, taken so they're handled once.
fn take_argv(dir: &Path) -> anyhow::Result<Option<Vec<String>>> {
    let path = dir.join("near-argv");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    std::fs::remove_file(&path)?;
    let text = String::from_utf8(bytes).context("near-argv is not UTF-8")?;
    let mut argv: Vec<String> = text.split('\0').map(String::from).collect();
    // printf '%s\0' ends every argument with a NUL, the last one included.
    if argv.last().is_some_and(String::is_empty) {
        argv.pop();
    }
    Ok(Some(argv))
}

async fn register(env: &Env, account: &AccountId) -> anyhow::Result<()> {
    env.owner
        .call(env.tst.id(), "storage_deposit")
        .args_json(json!({ "account_id": account }))
        .deposit(NearToken::from_millinear(10))
        .transact()
        .await?
        .into_result()?;
    Ok(())
}

fn write_files(
    dir: &Path,
    env: &Env,
    api: &Account,
    bob: &Account,
    mallory: &Account,
) -> anyhow::Result<()> {
    let rpc = env.worker.rpc_addr();
    let agent_env = dir.join("agent.env");
    create(
        &agent_env,
        &format!(
            "NSG_GUARD_ID={}\nNSG_AGENT_ID={}\nNSG_RPC_URL={rpc}\nNSG_AGENT_KEY={}\n",
            env.guard.id(),
            env.agent.id(),
            env.agent.secret_key(),
        ),
        0o600,
    )?;
    let mcp = json!({
        "mcpServers": {
            "near-spend-guard": {
                "type": "stdio",
                "command": "node",
                "args": [
                    format!("--env-file={}", agent_env.display()),
                    client_bin("nsg-pay-mcp.ts")?,
                ],
            }
        }
    });
    create(
        &dir.join("mcp.json"),
        &serde_json::to_string_pretty(&mcp)?,
        0o644,
    )?;
    let world = json!({
        "rpc_url": rpc,
        "guard": env.guard.id(),
        "token": env.tst.id(),
        "agent": env.agent.id(),
        "approver": env.approver.id(),
        "allowed": [api.id(), bob.id(), env.alice.id()],
        "not_allowed": [mallory.id()],
        "api": api.id(),
        "bob": bob.id(),
        "mallory": mallory.id(),
    });
    create(
        &dir.join("world.json"),
        &serde_json::to_string_pretty(&world)?,
        0o644,
    )?;
    let near = dir.join("near");
    create(&near, &stand_in(dir, env.approver.id()), 0o755)?;
    let config_dir = dir.join("config/near-spend-guard");
    std::fs::create_dir_all(&config_dir)?;
    let approver = json!({
        "guard_id": env.guard.id(),
        "approver_id": env.approver.id(),
        "network": "sandbox",
        "rpc_url": rpc,
        "near_cli": near,
    });
    create(
        &config_dir.join("approver.json"),
        &approver.to_string(),
        0o644,
    )?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let dir = PathBuf::from(std::env::var_os("NSG_LOCAL_DIR").context("NSG_LOCAL_DIR is not set")?);
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let dir = dir.canonicalize()?;
    if std::fs::read_dir(&dir)?.next().is_some() {
        bail!("{} is not empty", dir.display());
    }
    let minutes: u64 = match std::env::var("NSG_LOCAL_MINUTES") {
        Ok(text) => text.parse().context("NSG_LOCAL_MINUTES")?,
        Err(_) => 120,
    };

    let env = setup().await?;
    let root = env.worker.root_account()?;
    let api = subaccount(&root, "api", 10).await?;
    let bob = subaccount(&root, "bob", 10).await?;
    let mallory = subaccount(&root, "mallory", 10).await?;
    for account in [api.id(), bob.id(), mallory.id()] {
        register(&env, account).await?;
    }
    allow(&env, api.id(), false).await?;
    allow(&env, bob.id(), false).await?;
    write_files(&dir, &env, &api, &bob, &mallory)?;

    println!(
        "local node ready at {} for {minutes} min: guard {}, TST {}, {} TST left today",
        env.worker.rpc_addr(),
        env.guard.id(),
        env.tst.id(),
        remaining(&env).await? / TST,
    );
    println!(
        "files in {}; touch {}/stop to end",
        dir.display(),
        dir.display()
    );

    let deadline = Instant::now() + Duration::from_secs(minutes * 60);
    let outcome = loop {
        if dir.join("stop").exists() {
            break Ok(());
        }
        if Instant::now() >= deadline {
            println!("local node: {minutes} min are up");
            break Ok(());
        }
        match take_argv(&dir) {
            Ok(Some(argv)) => {
                let text = sign(&env, &argv).await;
                print!("local node, approval: {text}");
                let tmp = dir.join("near-result.tmp");
                if let Err(e) = std::fs::write(&tmp, &text)
                    .and_then(|()| std::fs::rename(&tmp, dir.join("near-result")))
                {
                    break Err(e.into());
                }
            }
            Ok(None) => {}
            Err(e) => break Err(e),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    // The key goes with the node.
    let _ = std::fs::remove_file(dir.join("agent.env"));
    println!("local node stopped");
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUARD: &str = "guard.test.near";
    const APPROVER: &str = "approver.test.near";
    const PAYLOAD: &str = r#"{"payload":{"request_id":"3","token":{"ft":"tst.test.near"},"receiver_id":"api.test.near","amount":"60000000","msg":null,"expires_at":"1"}}"#;

    /// What `nearArgs` builds for the sandbox (client/src/approve.ts).
    fn argv() -> Vec<String> {
        [
            "contract",
            "call-function",
            "as-transaction",
            GUARD,
            "approve",
            "json-args",
            PAYLOAD,
            "prepaid-gas",
            "100 Tgas",
            "attached-deposit",
            "1 yoctoNEAR",
            "sign-as",
            APPROVER,
            "network-config",
            "sandbox",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn takes_what_nsg_approve_builds() {
        let args = approval_args(&argv(), GUARD, APPROVER).unwrap();
        assert_eq!(args, serde_json::from_str::<Value>(PAYLOAD).unwrap());
    }

    #[test]
    fn refuses_any_other_fixed_argument() {
        for at in (0..15).filter(|&at| at != 6) {
            let mut changed = argv();
            changed[at].push('x');
            let error = approval_args(&changed, GUARD, APPROVER).unwrap_err();
            assert!(
                error.to_string().starts_with(&format!("argument {at} ")),
                "{error}"
            );
        }
    }

    #[test]
    fn refuses_more_or_fewer_arguments() {
        let mut longer = argv();
        longer.push("send".into());
        assert!(approval_args(&longer, GUARD, APPROVER).is_err());
        let mut shorter = argv();
        shorter.pop();
        assert!(approval_args(&shorter, GUARD, APPROVER).is_err());
    }

    #[test]
    fn refuses_json_args_other_than_one_payload() {
        for json_args in [
            "not json",
            "[]",
            r#"{"payload":"3"}"#,
            r#"{"request_id":"3"}"#,
            r#"{"payload":{},"extra":1}"#,
        ] {
            let mut changed = argv();
            changed[6] = json_args.into();
            assert!(
                approval_args(&changed, GUARD, APPROVER).is_err(),
                "{json_args}"
            );
        }
    }

    #[test]
    fn reads_the_stand_ins_arguments_once() {
        let dir = std::env::temp_dir().join(format!("nsg-local-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // As `printf '%s\0'` writes them: one argument holds a newline.
        std::fs::write(dir.join("near-argv"), "a\0two\nlines\0\0").unwrap();
        assert_eq!(
            take_argv(&dir).unwrap(),
            Some(vec![
                "a".to_string(),
                "two\nlines".to_string(),
                String::new()
            ])
        );
        assert_eq!(take_argv(&dir).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(sh_quote("/tmp/it's"), r"'/tmp/it'\''s'");
    }
}
