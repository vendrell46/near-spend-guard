# Security model

What the guard enforces, what it trusts, and what it leaves to you. The guard has had no independent audit. It runs on testnet only, and the client refuses mainnet.

## On chain and off chain

**Enforced by the contract and nearcore**, whatever the agent's code does:

- The agent's key can call `request_payment` on the guard and nothing else, and can't attach a deposit. nearcore enforces this for a function-call key.
- Per-payment ceiling, rolling daily cap, receiver allowlist, `ft_transfer_call` permission per receiver, and the pause flag, per token.
- A payment is counted in the receipt that sends it, so parallel requests can't overspend the cap.
- An approval executes only the exact stored payload, once, before its expiry, and only from the approver with 1 yoctoNEAR attached (so a full-access key).
- The callback is `#[private]`, reads the transfer's result bounded to 64 bytes, and gives budget back only when nothing moved.
- The guard refuses what it can't afford before reserving anything, so it never fails a payment for lack of storage NEAR after counting it.

**Done by the client, off chain**, and only as good as the machine it runs on:

- `nsg-pay-mcp` refuses to start unless its key is a function-call key for `request_payment` on its guard only, and unless the guard's `agent_id` is the agent it was given. This guards against a mistake in set-up. A compromised agent with a wider key isn't stopped by it.
- `pay` reports `unknown`, not `failed`, whenever it can't prove nothing happened, so an agent that follows its answers doesn't pay twice.
- `nsg-approve` takes only a request number, reads the guard's id from the approver's own config and the payment from that guard, prints `msg` escaped, and leaves signing to near CLI.
- Both programs refuse mainnet.

## Trust assumptions

- **The owner is trusted.** The owner can move every coin the guard holds (see [Owner powers](#owner-powers)).
- **The approver is trusted to read before signing.** `approve` checks that the payload is the stored one, not that it is a good payment.
- **Configured tokens are trusted to behave as NEP-141**, with `ft_transfer` as one receipt that either moves the tokens or doesn't. See [Limitations](#limitations-and-owner-powers).
- **nearcore's access-key rules hold**: a function-call key reaches only its contract and methods, and can't attach a deposit.
- **The guard account's own full-access key is deleted after deployment** in production, so nobody can replace its code. The testnet demo keeps one.
- **The agent and the approver don't share an OS user.** `nsg-approve` protects against what the agent prints, not against code running as the approver.

## Owner powers

The owner can withdraw everything, change any policy, reset a token's window by removing and re-adding it, and replace the agent, which cancels its pending requests. `withdraw` is allowed while paused and for any token, configured or not, so nothing can get stuck in the guard. It is not counted in any cap. These powers are listed again, with the other limitations, [below](#limitations-and-owner-powers).

## What the guard does not protect against

- **Spending within the policy.** A compromised or prompt-injected agent can still pay allowed receivers up to the caps: up to `daily_max` a window without a human, each payment up to `approval_above`. Keep the allowlist short and the caps small.
- **An allowed receiver that misbehaves.** The guard checks who is paid and how much, not whether the service was delivered.
- **A bad approval.** If the approver signs a payment they shouldn't have, it executes, up to `per_payment_max`.
- **A stolen owner or approver key.** Either can move money: the owner directly, the approver through held requests.
- **A token whose code is hostile or changes.** An upgradeable token's code can change after you configure it.
- **Caps across tokens.** Caps are per token. A cap across tokens in one currency would need an oracle.
- **Typos in implicit account ids.** A native payment to an allowlisted implicit account that doesn't exist creates it, as any transfer would.

## Threats and the test for each

Each row names the defence and the tests that exercise it. Separately, the mutation runners in [`tools/`](../tools/README.md) weaken the checks one at a time and require a test to fail ([Testing](testing.md)).

| Threat | Defence | Test |
| --- | --- | --- |
| A compromised agent fires a burst to beat the daily cap | Reserve in the receipt that dispatches; release only in the callback | [`race_guard_exact`](../sandbox-tests/tests/burst.rs#L23) (sandbox), [`reservation_interleavings`](../contracts/guard/src/tests/payments.rs#L1503) (unit, property-based) |
| The agent splits a big payment into many small ones | Every payment counts for at least 24 hours | [`window_never_exceeds_cap`](../contracts/guard/src/tests/payments.rs#L1418) (property-based), [`bucket_boundary`](../contracts/guard/src/tests/payments.rs#L771) |
| The payload is swapped after the human agreed | `approve` carries the full payload, compared field by field | [`payload_swap_guard_refuses`](../contracts/guard/src/tests/payments/approvals.rs#L158), [`approval_requires_exact_payload`](../contracts/guard/src/tests/payments/approvals.rs#L106) (each field changed in turn) |
| An approval is reused or used late | The entry is deleted on approve; expiry checked; at most 7 days out | [`approval_single_use`](../contracts/guard/src/tests/payments/approvals.rs#L176), [`approval_at_exact_expiry`](../contracts/guard/src/tests/payments/approvals.rs#L187) |
| The agent approves its own request | The agent can't be the approver; `approve` checks the caller and needs 1 yocto | [`new_refuses_agent_as_approver`](../contracts/guard/src/tests.rs#L215), [`set_approver_refuses_agent`](../contracts/guard/src/tests.rs#L549), [`function_call_key_cannot_approve`](../sandbox-tests/tests/wrapper.rs#L213) (nearcore's `DepositWithFunctionCall`) |
| A compromised agent prints an approval command for a look-alike contract | `nsg-approve` takes only a request number and reads the payment from the guard named in the approver's own config; near CLI shows receiver, method, arguments and deposit before signing | [`nsg_approve_ignores_agent_output`](../client/test/approve.test.ts#L326) (the look-alike's words on the command line, on stdin and in the environment) |
| An agent retries after a timeout and pays twice | Once sent, `pay` never says "failed" unless the first receipt failed; it says `unknown`, with the hash and "do not pay again before checking" | [`giving up never reads as 'nothing happened'`](../client/test/pay.test.ts#L186), [`a failure after the first receipt doesn't say nothing moved`](../client/test/pay.test.ts#L290), [`a_second_process_on_the_key_costs_one_answer`](../sandbox-tests/tests/client.rs#L412) |
| A wider key is handed to the agent by mistake | `nsg-pay-mcp` won't start unless its key is a function-call key for `request_payment` on this guard only | [`the_server_refuses_a_key_that_can_do_more`](../sandbox-tests/tests/client.rs#L434), [`the agent's key has to be the narrow one`](../client/test/pay.test.ts#L327) |
| `msg` hides something on the approver's screen | Printed escaped, with a warning if anything needed escaping | [`a message can't hide in the terminal`](../client/test/approve.test.ts#L146) |
| The owner tightened the policy after a request was made | `approve` re-checks pause, token, allowlist and ceiling | [`approve_after_disallow_refuses_and_removes`](../contracts/guard/src/tests/payments/approvals.rs#L239), [`approval_rechecks_the_current_policy`](../contracts/guard/src/tests/payments/approvals.rs#L268) |
| The agent fills the guard's storage with pending requests | At most 16 pending; `msg` at most 1024 bytes; allowlist checked at request time; expired entries removed by anyone, and by the next held request when the list is full | [`seventeenth_pending_is_refused`](../contracts/guard/src/tests/payments.rs#L664), [`a_full_list_makes_room_from_expired_entries`](../contracts/guard/src/tests/payments.rs#L706), [`prune_expired_removes_only_expired`](../contracts/guard/src/tests/payments/approvals.rs#L408), [`pending_entry_storage`](../sandbox-tests/tests/limits.rs#L99) |
| The agent makes the guard pay receivers' storage | The guard never calls `storage_deposit`; an unregistered receiver makes `ft_transfer` fail and the callback releases the budget | [`unregistered_receiver_releases_budget`](../contracts/guard/src/tests/payments.rs#L958) |
| `ft_transfer_call` fails after the tokens moved | Counted as spent (`uncertain`); a partial refund gives back exactly what came back | [`ft_transfer_call_failure_keeps_reservation`](../contracts/guard/src/tests/payments.rs#L1144), [`ft_transfer_call_partial_refund`](../contracts/guard/src/tests/payments.rs#L1123) |
| The callback runs out of gas | Callback gas reserved at dispatch, prepaid gas checked before reserving; a failed callback leaves the reservation (lost budget, never lost funds) | [`callback_gas_headroom`](../sandbox-tests/tests/gas.rs#L21) (at least twice the measured gas), [`gas_with_a_full_policy`](../sandbox-tests/tests/limits.rs#L121) |
| Someone else calls the callback | `#[private]` | [`callback_is_private`](../sandbox-tests/tests/wrapper.rs#L108) |
| A release lands after its hourly slot rotated out | The callback carries the slot's epoch; a mismatch is a no-op | [`release_after_slot_rotated_is_noop`](../contracts/guard/src/tests/payments.rs#L1189) |
| Overflow near `u128::MAX` | Checked arithmetic; an overflowing total is over the cap | [`caps_near_u128_max`](../contracts/guard/src/tests/payments.rs#L831) |
| A token returns an over-long or malformed result after moving the funds | The callback reads the result itself, bounded to 64 bytes; only a plain failure of a single-receipt transfer releases budget | [`callback_too_long_result_keeps_reservation`](../contracts/guard/src/tests/payments.rs#L1067), [`callback_unparseable_transfer_call_counts_as_used`](../contracts/guard/src/tests/payments.rs#L1162) |
| A compromised agent pays itself | The agent can never be a receiver, the owner or the approver | [`agent_cannot_be_receiver`](../contracts/guard/src/tests.rs#L448) |
| A payment would leave the guard unable to pay for its own storage | Refused before anything is reserved; `approve` and `withdraw` panic and change nothing | [`guard_refuses_what_it_cannot_afford`](../sandbox-tests/tests/settlement.rs#L180), [`approve_waits_until_the_guard_can_pay`](../sandbox-tests/tests/settlement.rs#L238), [`withdraw_stops_at_the_storage_cost`](../contracts/guard/src/tests/payments.rs#L1291) |
| A native payment to an account that doesn't exist | A named account: `failed`, refunded, budget released. An implicit account: created and `paid` | [`native_payment_to_an_account_that_does_not_exist`](../sandbox-tests/tests/settlement.rs#L336) |
| A full guard runs out of gas where it matters | Measured at 8 tokens, 32 receivers each and 16 pending entries, with the longest ids and `msg` | [`gas_with_a_full_policy`](../sandbox-tests/tests/limits.rs#L121), [`full_guard_stays_manageable`](../sandbox-tests/tests/limits.rs#L218) |

## Limitations and owner powers

- **Caps are per token.** A cap across tokens in one currency would need an oracle.
- **Amounts on chain are base units.** The wallet and near CLI show `amount` in the token's smallest unit; `pay` and `nsg-approve` print both forms, using the token's own `ft_metadata.decimals`.
- **`pay` isn't idempotent.** Two calls are two payments, bounded by the caps. After `unknown`, an agent must check the transaction before paying again; the answer says so.
- **`nsg-approve` protects against what the agent prints, not against code running as the approver.** The agent and the approver shouldn't share an OS user.
- **Approved payments sit outside `daily_max` by design.** `daily_max` bounds what the agent moves alone; a human's approval is bounded by `per_payment_max` and counted separately in views and events.
- **A configured token is trusted to behave as NEP-141.** The guard gives budget back when an `ft_transfer` fails, which is right only if `ft_transfer` is one receipt that either moves the tokens or doesn't. A token whose `ft_transfer` moved tokens and then failed in a later receipt would get the budget back. Configure only tokens whose code you trust; an upgradeable token's code can change.
- **An implicit receiver that doesn't exist yet is created.** A native payment to an allowlisted 64-hex, `0x`, `0s` or `0u` account creates it, so a typo in an implicit id loses the money as any transfer would. A named account that doesn't exist fails, and the NEAR comes back.
- **The guard needs NEAR above its storage cost.** Below that it refuses with `insufficient_balance`. A full policy stores about 27 KB more than an empty one, and each pending entry up to 1.5 KB.
- **`withdraw` has no callback.** Its `withdrawal` event marks the transfer being sent; the transfer's own receipt shows whether it moved.
- **Expired requests stay listed until removed.** `get_pending` shows them until `prune_expired` (anyone), a late `approve`, or a held request on a full list removes them.
- **The owner can withdraw everything, change any policy, reset a token's window by removing and re-adding it, and replace the agent**, which cancels its pending requests. In production the guard account's own full-access key must be deleted after deployment, so its code can't be replaced; the testnet demo keeps one.
- **The deployed wasm isn't reproducible on another machine yet.** It embeds the builder's crate paths. `target/deploy/SHA256SUMS` and the code hash on chain match each other.

## Self-audit

On 11 Nov the contract was read line by line for rounding, storage, callback failure paths, what can run on the guard between a payment and its callback, and pending requests. What changed:

- **A full pending list now makes room from its expired entries.** An expired request can never be approved, but it kept its place among the 16 until someone called `prune_expired`, and the agent's key can't. So a guard whose approver let 16 requests lapse would have refused every later held payment with `too_many_pending`. Now a held request that finds the list full removes the expired entries first, each with a `request_expired` event, and is refused only if none had expired. On a full guard (8 tokens of 32 receivers, 16 entries with the longest ids and `msg`) that request burns 9.25 TGas of the 30 the client attaches ([`full_guard_stays_manageable`](../sandbox-tests/tests/limits.rs#L218)).
- **Two limitations written down** (above): a configured token is trusted to implement `ft_transfer` as one receipt, and `withdraw`'s event marks the transfer being sent, not its outcome.

What was checked and left as it is:

- **Rounding.** The only division is the hour slot, so a payment counts for between 24 and 25 hours ([`bucket_boundary`](../contracts/guard/src/tests/payments.rs#L771)). An `ft_transfer_call` refund gives back exactly what came back, capped at the amount. Release builds keep overflow checks on.
- **Storage.** Every check reads the guard's balance before the receipt's own writes, against what that receipt adds: a pending entry (1,500 bytes allowed, 1,369 measured), the amount for a native payment, or 1 yocto for a token transfer. Dispatch adds no storage, and the callback only rewrites or shrinks.
- **Callbacks.** The callback reads the transfer's result bounded to 64 bytes and can't panic on it; out of gas, the reservation stays spent.
- **Between a payment and its callback.** Another request reserves first and sees this one; `approve` counts separately; removing and re-adding a token makes the late callback a no-op; a `withdraw` of tokens in flight makes the transfer fail, and the release is right. An allowlisted contract called through `ft_transfer_call` finds only views, `prune_expired` and methods that check their caller.

## Alongside OutLayer

[OutLayer](https://outlayer.fastnear.com/docs/agent-custody) runs a full agent policy engine in a TEE: per-transaction limits, hourly to monthly caps, allowlists, multisig and freeze. Its documentation is candid about two edges. Velocity caps are "best-effort under concurrency": the counter comes from its API gateway, and concurrent requests "can each read the same pre-spend counter and all pass". And for NEAR Intents operations the TEE "does not itself re-check the token or amount". The guard does those two things on chain, with no TEE to trust: exact accounting under concurrency, and approvals bound to the full payload. It can stand alone, or sit behind an off-chain engine as a hard backstop on the account that holds the funds. (OutLayer's docs read 11 Nov 2026.)

