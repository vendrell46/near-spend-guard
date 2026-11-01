//! The NEP-141 test token (TST, 6 decimals), written on day 4.
//!
//! Built on `near-contract-standards`, with switches that make
//! `ft_on_transfer` or `ft_resolve_transfer` fail, so the guard's handling of
//! failed and partly refunded transfers is tested rather than assumed.
