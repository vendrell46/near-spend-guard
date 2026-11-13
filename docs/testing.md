# Testing

Run everything from the repo root. [AGENTS.md](../AGENTS.md) has the same commands in one list.

## Setup

- Rust through rustup. `rust-toolchain.toml` brings 1.97.0 and the `wasm32-unknown-unknown` target on first use.
- A C compiler: near-sdk's unit-testing feature builds `aws-lc-sys`.
- For sandbox tests: Linux x86_64 or an Apple Silicon Mac, the systems nearcore publishes a sandbox binary for. Elsewhere, Docker and `scripts/test-container.sh`.
- For the client: Node 22.22.2 or later. No build step.
- For the deployed files only: cargo-near 0.22.0.

After a first run has fetched the crates, add `--offline --locked` to any `cargo test` or `scripts/test-*.sh` command.

## The levels

| Level | Command | Runs | Prints |
| --- | --- | --- | --- |
| Unit | `cargo test --workspace --exclude sandbox-tests` | the guard's, the naive guard's and the test token's unit tests, on near-sdk's mocked runtime | `test result: ok. 9 passed` (naive guard), `test result: ok. 109 passed` (guard), `test result: ok. 13 passed` (test token) |
| Client, unit | `cd client && npm test` | `node --test` on `client/test`, no network, no install needed | `# pass 120`, `# fail 0` |
| Sandbox | `scripts/test-sandbox.sh` | near-workspaces tests against the compiled contracts on nearcore's sandbox | one `test result: ok.` line per test file |
| Client, end to end | `scripts/test-client.sh` | `npm ci`, the client's unit tests, the near-api-js sender's tests, then `pay` and `nsg-approve` against the real contracts on a local node | `# pass 120`, `# pass 9`, then `test result: ok. 5 passed` |
| Deployed files | `scripts/test-deployable.sh` | builds `target/deploy` with cargo-near, then the sandbox tests against those files | as the sandbox tests |
| Container | `scripts/test-container.sh` | unit and sandbox tests in a pinned `rust:1.97.0` image (linux/amd64) | as the two above |
| Mutation | `python3 -I tools/mutate.py . tools/clean /tmp/nsg-mutants/unit` | weakens one check at a time and requires a test to fail | `caught` per mutant, then `91 mutants; survived: none` |

To run only the guard and the naive guard's unit tests: `cargo test -p near-spend-guard -p naive-guard`.

## Unit tests

In [`contracts/guard/src/tests.rs`](../contracts/guard/src/tests.rs) and [`contracts/guard/src/tests/`](../contracts/guard/src/tests). They cover the methods' checks and panics, the window and its slot boundaries, how the callback reads transfer results, approvals with each payload field changed in turn, and the limits. Some are property-based: `window_never_exceeds_cap` and `reservation_interleavings` in [`payments.rs`](../contracts/guard/src/tests/payments.rs). The payload hash is checked against fixed vectors in [`payload_vectors.json`](../contracts/guard/src/tests/payload_vectors.json), which the client's tests read too.

## Sandbox tests

[`scripts/test-sandbox.sh`](../scripts/test-sandbox.sh) builds the guard, the naive guard and the test token to wasm (`scripts/build-wasm.sh`, the compile step of `cargo near build` before wasm-opt), downloads nearcore's sandbox 2.14.0 once into `target/near-sandbox/` and checks it against a pinned SHA-256 (`scripts/sandbox-bin.sh`), then runs `cargo test -p sandbox-tests`. Arguments go to `cargo test`; `--offline`, `--locked` and `--frozen` also go to the wasm build. A test stops if a wasm file is older than its contract's sources.

The files in [`sandbox-tests/tests/`](../sandbox-tests/tests):

| File | What it checks |
| --- | --- |
| `wrapper.rs` | what only the compiled contract and nearcore show: `#[init]`, `#[private]`, deposits, access keys (`function_call_key_cannot_approve`, `agent_key_reaches_only_request_payment`, `pause_from_limited_key`) |
| `settlement.rs` | settling against the test token with each of its failure switches; what the guard's NEAR can't afford; native payments to accounts that don't exist |
| `burst.rs` | ten requests in one block against the cap (`race_guard_exact`), and what a request costs the agent key's allowance |
| `gas.rs` | the gas the guard reserves against what each receipt burns, at least 2x |
| `limits.rs` | gas and storage at the limits: 8 tokens, 32 receivers, 16 pending, the longest ids and `msg` |
| `window.rs` | the rolling window on the sandbox clock with `fast_forward` |
| `demo.rs` | the demo's scenes, printing their verdict lines ([Demo](demo.md)) |
| `client.rs` | the client's two programs against the real contracts; ignored in a plain sandbox run because they need Node, run by `scripts/test-client.sh` |

The test token, [`contracts/test-token`](../contracts/test-token/src/lib.rs), is a NEP-141 token (TST, 6 decimals) whose owner can make transfers fail or answer oddly (`set_switch`). A second deployment receives `ft_transfer_call` and does what `msg` says (`keep`, `refund:<n>`, `fail`).

## The demo tests

Each prints its verdict lines and asserts them. `--nocapture` shows them:

```bash
scripts/test-sandbox.sh --test demo race -- --nocapture
scripts/test-sandbox.sh --test demo price_swap -- --nocapture
scripts/test-sandbox.sh --test demo approval_flow -- --nocapture
```

`approval_flow`, run locally on 13 Nov with `--offline --locked`:

```
running 1 test
approval_flow: an agent pays for an API through near-spend-guard
pay 5 TST to api        paid (autonomous)
pay 500 TST to mallory  refused (receiver_not_allowed)
pay 60 TST to api       pending (above_threshold), request 3, hash 3ddd5bee...
approve request 3       paid 60 TST (approved)
test approval_flow ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 16.39s
```

The hash covers the expiry, which comes from the node's clock, so it differs on every run. [Demo](demo.md) has the output of all three and what each asserts.

## Client tests

- `client/test/*.test.ts` (`npm test`): the MCP server, `pay`, `nsg-approve`, amounts, the payload encoding and hash, the RPC client and the deploy's world data. They need no install.
- `client/test-near/*.test.ts` (`npm run test:near`): the near-api-js sender. Needs `npm ci --ignore-scripts` in `client/` first.
- `sandbox-tests/tests/client.rs`: both programs as processes against the contracts on a local node, through `scripts/test-client.sh`.

## CI

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs on every push to `main` and every pull request, on `ubuntu-24.04`, with no secrets: format check, unit tests, client tests, sandbox tests, the client against the contracts, the step "Demo verdict lines" (`scripts/test-sandbox.sh --locked --test demo -- --nocapture --test-threads 1`, whose output the README quotes), and clippy with `-D warnings`. It doesn't run the mutation runners.

[`.github/workflows/container.yml`](../.github/workflows/container.yml) runs `scripts/test-container.sh` from a clean image. It is started by hand.

## Format and lint

```bash
cargo fmt --all --check
NEAR_SANDBOX_BIN_PATH=$(scripts/sandbox-bin.sh) cargo clippy --workspace --all-targets -- -D warnings
```

Anything that compiles `sandbox-tests` needs `NEAR_SANDBOX_BIN_PATH`, or near-workspaces' build script downloads its own unchecked copy of the sandbox.

## Mutation runners

The runners in [`tools/`](../tools/README.md) weaken one check at a time, run the tests that should notice, and put the file back. Every mutant must make a test fail; one that doesn't compile counts as surviving.

| Command | Weakens | Last result (12 Nov) |
| --- | --- | --- |
| `python3 -I tools/mutate.py . tools/clean /tmp/nsg-mutants/unit` | the guard's checks in `lib.rs` and `types.rs` | `91 mutants; survived: none` |
| `python3 -I tools/mutate_sandbox.py . tools/clean /tmp/nsg-mutants/sandbox` | what only the compiled contract shows | `31 mutants; survived: none` |
| `python3 -I tools/mutate_client.py . tools/clean /tmp/nsg-mutants/client` | every file in `client/src` and `client/deploy/world.ts` | `173 mutants; survived: none` |

Run them one at a time on a clean checkout: they edit files in the working tree. The third argument receives a copy of each file before it is changed. [`tools/README.md`](../tools/README.md) has the timings and options.
