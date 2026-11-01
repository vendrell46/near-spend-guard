# Building and testing

Rust 1.97.0 with the `wasm32-unknown-unknown` target, pinned in `rust-toolchain.toml` (rustup installs it on first use). Unit tests also need a C compiler: `near-crypto` builds `aws-lc-sys`.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo near build non-reproducible-wasm --manifest-path contracts/guard/Cargo.toml
```

Workspace:

- `contracts/guard`: the guard contract (`near-spend-guard`) and its unit tests.
- `contracts/naive-guard`: the demo's naive guard (not written yet).
- `contracts/test-token`: a NEP-141 test token with failure switches (not written yet).
- `sandbox-tests`: near-workspaces tests against the compiled contracts (not written yet).
