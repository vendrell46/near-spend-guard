//! The naive guard for the demo, written on day 7.
//!
//! Same policy, allowlist and approval deposit as `near-spend-guard`, with two
//! deliberate differences: it counts a payment only in the transfer callback,
//! and it approves by request number instead of by the full payload.
