# Deploy to testnet

The scripts put a complete demo world on testnet: a guard, a test token, two receivers, an agent with its function-call key, and the files the client reads. They refuse any chain but testnet. Nothing here supports mainnet.

## What you need

- What the sandbox tests need ([Testing](testing.md#setup)), plus cargo-near 0.22.0 on `PATH`.
- Node 22.22.2 or later.
- near CLI, with its `config.toml`: the deploy reads the RPC and faucet URLs from one of near CLI's own connections, and the approver signs with near CLI.

## 1. Build the files to deploy

```bash
scripts/build-deployable.sh
scripts/test-deployable.sh
```

[`scripts/build-deployable.sh`](../scripts/build-deployable.sh) builds the guard, the test token and the naive guard with `cargo near build non-reproducible-wasm` (wasm-opt, ABI embedded), always `--locked`, into `target/deploy`, and writes `target/deploy/SHA256SUMS`. It stops unless `cargo near --version` is `cargo-near-near 0.22.0`. Pass `--offline` to build from the local crate cache. The naive guard is never deployed; it is built the same way so the demo tests compare like with like.

[`scripts/test-deployable.sh`](../scripts/test-deployable.sh) runs the sandbox tests against exactly those files (`NSG_WASM_DIR=target/deploy`). It takes the same arguments as `scripts/test-sandbox.sh`; add `--test client -- --include-ignored --test-threads 1` for the client's tests.

These builds aren't reproducible on another machine yet: the wasm embeds the builder's crate paths. `target/deploy/SHA256SUMS` and the code hash on chain match each other.

## 2. Deploy

```
usage: scripts/deploy-testnet.sh [--connection <name>] <prefix> [<dir>]
```

```bash
scripts/deploy-testnet.sh nsg
```

[`scripts/deploy-testnet.sh`](../scripts/deploy-testnet.sh) builds nothing. It checks `target/deploy` against `SHA256SUMS` and stops if they don't match, so what goes on chain is what the sandbox tests ran on. Then it runs [`client/deploy/testnet.ts`](../client/deploy/testnet.ts) through near-api-js, with only `PATH` and `HOME` in its environment, on near CLI's connection `testnet-fastnear` unless `--connection` names another. It stops unless the RPC says the chain is testnet.

The prefix is lowercase letters, digits and single hyphens, up to 40 characters. The steps, as it logs them:

1. **Faucet accounts**: `<prefix>-guard.testnet`, `<prefix>-agent.testnet`, `<prefix>-approver.testnet` (the guard's owner and approver) and `<prefix>-demo.testnet` (the token's owner).
2. **The demo account's sub-accounts**: `tst.<prefix>-demo.testnet` (the token), `api.<prefix>-demo.testnet` and `mallory.<prefix>-demo.testnet`.
3. **The test token**, deployed and initialised in one transaction.
4. **TST storage** for the guard, `api` and `mallory`.
5. **The guard**, deployed and initialised in one transaction, with a one-hour slot.
6. **The guard's TST**: 1,000 TST.
7. **The policy**: `set_token` with at most 200 TST per payment, a human above 40 and 100 a day without one, and `allow_receiver` for `api`, without `ft_call`. `mallory` is registered with the token but not allowed.
8. **Checks**: `get_config` and `get_token` against what was set, `remaining_budget`, and both code hashes against `SHA256SUMS`.
9. **The agent's function-call key**: `request_payment` on the guard only, with 1 NEAR of gas allowance.
10. **Files** (below).

Every step reads the chain first and acts only if its result isn't there yet. A second run finishes a deploy that stopped half-way and sends nothing once it's done. A key is written to disk before the transaction that puts it on chain. No secret is printed.

A new build of the guard needs a new prefix: the deploy refuses to replace code an account already runs, and it writes into a directory only once, so move the old world's directory aside first.

## What it writes

- **Full-access keys** go to `~/.near-credentials/testnet/<account>.json` (mode 0600), near CLI's legacy keychain, so the approver can sign with `sign-with-legacy-keychain`. The agent account's full-access key is there too, and is never given to the agent.
- **`<dir>`**, default `~/.config/near-spend-guard/testnet` (mode 0700):
  - `agent.env` (0600): `NSG_GUARD_ID`, `NSG_AGENT_ID`, `NSG_RPC_URL` and the agent's function-call key in `NSG_AGENT_KEY`.
  - `mcp.json`: an MCP config that starts `nsg-pay-mcp` with `node --env-file=<dir>/agent.env`.
  - `world.json`: the account names, network and RPC.
  - `config/near-spend-guard/approver.json`: `nsg-approve`'s config.

These are the same files `scripts/local-node.sh` writes, so the agent and the approver run unchanged against either:

```bash
scripts/demo-scene.sh 3
scripts/local-agent.sh ~/.config/near-spend-guard/testnet "<prompt>"
XDG_CONFIG_HOME=~/.config/near-spend-guard/testnet/config node client/bin/nsg-approve.ts <request_id>
```

## The deployed worlds

The world deployed on 11 Nov is `nsg2`: guard `nsg2-guard.testnet`, token `tst.nsg2-demo.testnet`, running commit `308f5cd`'s build. Its transactions are listed in [Demo](demo.md#on-testnet). The 10 Nov world, `nsg`, runs the build from before the 11 Nov self-audit and stays as it was.

## Configuring a guard of your own

The deploy script sets up the demo's world only. For another policy, call the guard's owner methods with near CLI or any wallet, signed by the owner with exactly 1 yoctoNEAR attached. The arguments, as the deploy sends them:

```json
{"token": {"ft": "tst.nsg2-demo.testnet"}, "per_payment_max": "200000000", "daily_max": "100000000", "approval_above": "40000000"}
```

```json
{"token": {"ft": "tst.nsg2-demo.testnet"}, "receiver_id": "api.nsg2-demo.testnet", "ft_call": false}
```

The first is `set_token`, the second `allow_receiver`. Amounts are in the token's smallest unit. [How it works](how-it-works.md#the-full-method-list) lists every method. Register the guard with each token it holds, and each receiver with the token it is paid in: the guard never calls `storage_deposit`, so a payment to an unregistered receiver fails and its budget comes back.

In production, delete the guard account's own full-access key after deployment, so its code can't be replaced ([Security model](security-model.md#trust-assumptions)). The testnet demo keeps one.
