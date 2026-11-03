//! near-workspaces tests against the compiled contracts, in `tests/`.
//!
//! Kept in their own crate: near-workspaces needs a sandbox binary that exists
//! only for Linux and Apple Silicon Macs, and the contracts' unit tests should
//! build everywhere Rust does. Run them with `scripts/test-sandbox.sh`.
