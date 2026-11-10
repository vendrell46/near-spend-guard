# Building and testing

Rust 1.97.0 with the `wasm32-unknown-unknown` target, pinned in `rust-toolchain.toml` (rustup installs it on first use). Unit tests also need a C compiler: `near-crypto` builds `aws-lc-sys`.

Unit tests, anywhere Rust runs:

```bash
cargo fmt --all --check
cargo test --workspace --exclude sandbox-tests
```

Sandbox tests, on Linux x86_64 and Apple Silicon Macs (the platforms `scripts/sandbox-bin.sh` pins a sandbox binary for):

```bash
scripts/test-sandbox.sh
```

It builds the guard and the test token to wasm (`scripts/build-wasm.sh`, the compile step of `cargo near build` before wasm-opt), downloads nearcore's sandbox 2.14.0 once into `target/near-sandbox/` and checks it against a pinned SHA-256 (`scripts/sandbox-bin.sh`), then runs `cargo test -p sandbox-tests`. Arguments go to `cargo test`. A test stops if a wasm file is older than its contract's sources.

Anywhere else with Docker, both of the above in a container:

```bash
scripts/test-container.sh
```

The TypeScript client (Node 22.22.2 or later), its own tests and then the client against the contracts on a local node:

```bash
scripts/test-client.sh
```

`client/` has no build step: Node runs its TypeScript directly. `npm test` needs no install, and only `client/src/near.ts` loads `near-api-js`, so the approver's helper runs on Node alone.

Anything that compiles `sandbox-tests` needs the binary's path, or near-workspaces' build script downloads its own unchecked copy:

```bash
NEAR_SANDBOX_BIN_PATH=$(scripts/sandbox-bin.sh) cargo clippy --workspace --all-targets -- -D warnings
```

The files we deploy to testnet, built with cargo-near 0.22.0 (wasm-opt, ABI embedded) into `target/deploy/` with a `SHA256SUMS`, and the sandbox tests run against exactly those files:

```bash
scripts/build-deployable.sh
scripts/test-deployable.sh
```

`scripts/test-deployable.sh` takes the same arguments as `scripts/test-sandbox.sh`; add `--test client -- --include-ignored --test-threads 1` for the client's tests. The tests read `$NSG_WASM_DIR` instead of `target/wasm32-unknown-unknown/release` when it is set. These builds aren't reproducible on another machine yet: the wasm embeds the builder's crate paths.

A real agent on a local node, with no testnet: one terminal keeps a node up with the demo's accounts (`guard`, `tst`, `api` and `bob` allowed, `mallory` registered but not allowed; DEMO.md), another runs Claude Code as the MCP host with `pay` as its only tool:

```bash
scripts/local-node.sh /tmp/nsg-local
scripts/local-agent.sh /tmp/nsg-local "Pay the 5 TST invoice for 1,000 API calls: 5 TST to api.test.near, TST is the token at tst.test.near."
XDG_CONFIG_HOME=/tmp/nsg-local/config node client/bin/nsg-approve.ts <request_id>
```

The directory must be empty or new; `touch /tmp/nsg-local/stop` ends the node (it ends itself after `NSG_LOCAL_MINUTES`, default 120). `scripts/local-agent.sh` gives the host no built-in tools and no MCP server but ours, and prints its stream-json events; extra arguments go to `claude`. The agent's key stays in `agent.env` (mode 0600, removed when the node stops), which the server reads with `node --env-file`, so it is never in the host's environment. With no near CLI on a local node, `nsg-approve` starts a stand-in that asks before signing, as near CLI does, and the node signs only the exact arguments `nsg-approve` builds (unit tests in `sandbox-tests/examples/local_node.rs`).

The demo on testnet. `scripts/deploy-testnet.sh` builds nothing: it deploys `target/deploy` only if the files still match `SHA256SUMS`, through near-api-js (`client/deploy/testnet.ts`) on one of near CLI's own connections (default `testnet-fastnear`, read from near CLI's `config.toml`; it stops unless the chain is testnet). It creates four faucet accounts: `<prefix>-guard`, `-agent`, `-approver` (owner and approver) and `-demo` (the token's owner, with `tst.`, `api.` and `mallory.` under it). Then the token and the guard, each deployed and initialised in one transaction; 1,000 TST for the guard; the demo's policy with `api.` allowed; a check of `get_config`, `get_token` and both code hashes against `SHA256SUMS`; and the agent's function-call key (`request_payment` on the guard only, 1 NEAR of gas). Every step reads the chain first, so a second run finishes a deploy that stopped half-way and sends nothing once it's done:

```bash
scripts/deploy-testnet.sh nsg
```

Full-access keys go to `~/.near-credentials/testnet/<account>.json` (0600), near CLI's legacy keychain (`sign-with-legacy-keychain`); the agent account's full-access key, never given to the agent, is there too. `~/.config/near-spend-guard/testnet` (0700) gets the agent's function-call key in `agent.env` (0600), `mcp.json`, `world.json` and `config/near-spend-guard/approver.json`, the same files `scripts/local-node.sh` writes. So `scripts/local-agent.sh ~/.config/near-spend-guard/testnet "<prompt>"` and `XDG_CONFIG_HOME=~/.config/near-spend-guard/testnet/config node client/bin/nsg-approve.ts <request_id>` run unchanged. The world deployed on 10 Nov is `nsg`: guard `nsg-guard.testnet`, token `tst.nsg-demo.testnet`. A new build of the guard needs a new prefix: the deploy refuses to replace code an account already runs.

Each scene of the demo video is one command (DEMO.md). Scenes 1 and 2 are the sandbox tests `race` and `price_swap`. Scene 3 speaks MCP to `pay` as a host does, against testnet by default or a local node's directory, and fails unless the answers are paid, refused (`receiver_not_allowed`) and pending (`above_threshold`, expiring in 15 minutes):

```bash
scripts/demo-scene.sh 1
scripts/demo-scene.sh 2
scripts/demo-scene.sh 3
```

Workspace:

- `contracts/guard`: the guard contract (`near-spend-guard`) and its unit tests.
- `contracts/naive-guard`: the demo's naive guard, "DO NOT USE" in its header.
- `contracts/test-token`: a NEP-141 test token (TST, 6 decimals) whose owner can make transfers fail or answer oddly (`set_switch`); a second deployment receives `ft_transfer_call` and does what `msg` says (`keep`, `refund:<n>`, `fail`).
- `client`: the agent's side and the approver's. `bin/nsg-pay-mcp.ts` is the MCP server with the one tool `pay` (settings in `NSG_GUARD_ID`, `NSG_AGENT_ID`, `NSG_RPC_URL`, `NSG_AGENT_KEY`; it won't start with a key that can do more than call `request_payment` on the guard). `bin/nsg-approve.ts <request_id>` reads `~/.config/near-spend-guard/approver.json` (`guard_id`, `approver_id`, `network`, `rpc_url`, optional `near_cli`), fetches the request from that guard, prints it and starts near CLI at the point where it asks how to sign. `deploy/testnet.ts` puts the demo world on testnet and `deploy/scene3.ts` plays scene 3 against it; `deploy/world.ts` holds the world as data (names, policy, call arguments, files) and, like `src/` apart from `near.ts`, loads nothing from npm.
- `sandbox-tests`: near-workspaces tests against the compiled contracts. `tests/wrapper.rs`: the checks only the compiled contract and nearcore make (`#[init]`, `#[private]`, deposits, access keys). `tests/settlement.rs`: settling against the test token, including every switch, what the guard's NEAR can't afford, and native payments to accounts that don't exist. `tests/burst.rs`: ten requests in one block against the cap, and what a request costs the agent key's allowance. `tests/gas.rs`: the gas the guard reserves against what each receipt burns (at least 2x). `tests/limits.rs`: gas and storage at the limits (8 tokens, 32 receivers, 16 pending, the longest ids and `msg`). `tests/window.rs`: the rolling window on the sandbox clock with `fast_forward`. `tests/demo.rs`: the demo's scenes, printing their verdict lines. `tests/client.rs`: the client's two programs against the real contracts, ignored in a plain sandbox run because they need Node; `scripts/test-client.sh` runs them. `examples/local_node.rs`: the local node above.
