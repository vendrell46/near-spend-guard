# Building and testing

Rust 1.97.0 with the `wasm32-unknown-unknown` target, pinned in `rust-toolchain.toml` (rustup installs it on first use). Unit tests also need a C compiler: `near-crypto` builds `aws-lc-sys`.

Unit tests, anywhere Rust runs:

```bash
cargo fmt --all --check
cargo test --workspace --exclude sandbox-tests
```

Sandbox tests, on Linux x86_64 and Apple Silicon Macs (the only platforms nearcore ships a sandbox binary for):

```bash
scripts/test-sandbox.sh
```

It builds the guard and the test token to wasm (`scripts/build-wasm.sh`, the compile step of `cargo near build` before wasm-opt), downloads nearcore's sandbox 2.14.0 once into `target/near-sandbox/` and checks it against a pinned SHA-256 (`scripts/sandbox-bin.sh`), then runs `cargo test -p sandbox-tests`. Arguments go to `cargo test`. A test stops if a wasm file is older than its contract's sources.

Anything that compiles `sandbox-tests` needs the binary's path, or near-workspaces' build script downloads its own unchecked copy:

```bash
NEAR_SANDBOX_BIN_PATH=$(scripts/sandbox-bin.sh) cargo clippy --workspace --all-targets -- -D warnings
```

The deployable wasm (testnet):

```bash
cargo near build non-reproducible-wasm --manifest-path contracts/guard/Cargo.toml
```

Workspace:

- `contracts/guard`: the guard contract (`near-spend-guard`) and its unit tests.
- `contracts/naive-guard`: the demo's naive guard (not written yet).
- `contracts/test-token`: a NEP-141 test token (TST, 6 decimals) whose owner can make transfers fail or answer oddly (`set_switch`); a second deployment receives `ft_transfer_call` and does what `msg` says (`keep`, `refund:<n>`, `fail`).
- `sandbox-tests`: near-workspaces tests against the compiled contracts. `tests/wrapper.rs`: the checks only the compiled contract and nearcore make (`#[init]`, `#[private]`, deposits, access keys). `tests/settlement.rs`: settling against the test token, including every switch, and a native payment the guard's storage can't afford. `tests/burst.rs`: ten requests in one block against the cap, and what a request costs the agent key's allowance. `tests/gas.rs`: the gas the guard reserves against what each receipt burns (at least 2x). `tests/window.rs`: the rolling window on the sandbox clock with `fast_forward`.
