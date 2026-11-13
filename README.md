# near-spend-guard

An on-chain spending guard for AI agents on NEAR: caps that hold under parallel requests, and approvals that cover one exact payment.

An agent that pays for APIs on its own needs access to money, and NEAR's keys alone can't say how much it may spend ([why](#why-a-contract)). The guard is a Rust contract that holds the funds. The agent calls it through a function-call key, and the contract enforces per-payment caps, a rolling daily cap, receiver allowlists, human approvals bound to the full payment, and a kill switch, for native NEAR and NEP-141 tokens. A small TypeScript client gives an agent one MCP tool, `pay`, and gives the human approver `nsg-approve`. Testnet only. Built at NEAR HACKATHON, 1–14 Nov 2026.

## What it does, and what shows it

- **Caps hold under parallel requests.** Ten payments of 25 TST sent at once against a daily cap of 100: exactly 100 leave without a human and the other six wait for one, on a real NEAR node. The same policy counting each payment only when it settles let 250 leave, in CI and locally (that number varies with block timing). (`race`, below.)
- **A human approves one exact payment, not a request number.** `approve` carries the full payload, so the approver's wallet shows the receiver and the amount, and the guard compares it field by field with what the agent filed. If the request says 190 TST and the human approves the 50 they agreed to, the guard refuses and nothing moves. (`price_swap`.)
- **The agent's key can only ask.** It is a function-call key for `request_payment` on the guard, which nearcore won't let attach a deposit or call anything else. The `pay` server refuses to start with a wider key, and `nsg-approve` reads the payment from the guard rather than from anything the agent prints. ([Threats](#threats-and-the-test-for-each).)
- **Native NEAR and NEP-141 tokens**, `ft_transfer_call` with partial refunds included, and every request, approval and settlement as a NEP-297 event.
- **Each check is tested by removing it.** The runners in [`tools/`](tools/README.md) weaken one check at a time and require a test to fail: 91 mutants against the guard's unit tests, 31 against the sandbox tests and 173 against the client's, every one caught.
- **Running on testnet**, with the agent's transactions linked [below](#on-testnet).

## Verify it in five minutes

Setup: Rust through rustup (`rust-toolchain.toml` brings 1.97.0 and the `wasm32-unknown-unknown` target on first use) and a C compiler, since near-sdk's unit-testing feature builds `aws-lc-sys`. The sandbox tests run on Linux x86_64 and Apple Silicon Macs, the systems nearcore publishes a sandbox binary for; anywhere else with Docker, `scripts/test-container.sh` runs the unit and sandbox tests in a pinned `rust:1.97.0` image. The client needs Node 22.22.2 or later and no build step. cargo-near is needed only to build the files deployed to testnet.

| Do | See |
| --- | --- |
| `cargo test --workspace --exclude sandbox-tests` | `test result: ok. 109 passed` for the guard, 9 for the naive guard, 13 for the test token |
| `scripts/test-sandbox.sh --test demo race -- --nocapture` | `near-spend-guard   100 TST left, 6 waiting for a human (daily_cap)`, and the naive guard above 100 |
| `scripts/test-sandbox.sh --test demo price_swap -- --nocapture` | `refused, payload does not match request 1; 0 TST moved`, and the naive guard paying 190 |
| `scripts/test-sandbox.sh --test demo approval_flow -- --nocapture` | the small payment `paid`, 500 to mallory `refused`, the large one `pending`, then `approve` and `paid` |
| `scripts/test-sandbox.sh` | every sandbox test, including one per row of the threats table below |
| `scripts/test-client.sh` | the client's own tests (`# pass 120`), the near-api-js sender's (`# pass 9`), then `test result: ok. 5 passed`: `pay` and `nsg-approve` against the real contracts on a local node |
| `python3 -I tools/mutate.py . tools/clean /tmp/nsg-mutants/unit` | `caught` for each weakened check, then `91 mutants; survived: none` (a few minutes; the sandbox and client runners are in [`tools/`](tools/README.md)) |
| [CI](https://github.com/vendrell46/near-spend-guard/actions/workflows/ci.yml) | all of the above but the mutation runners on Linux, with the demo's lines in the step "Demo verdict lines" |
| [Testnet](#on-testnet) | the agent's payments, a refusal and a held payment, with their events |

`scripts/test-sandbox.sh` builds the guard and the test token to wasm, downloads nearcore's sandbox 2.14.0 once and checks it against a pinned SHA-256, then runs `cargo test -p sandbox-tests`; arguments go to `cargo test`. A test stops if a wasm file is older than its contract's sources. [AGENTS.md](AGENTS.md) has every build and test command.

The three demo tests, as CI printed them on 11 Nov for commit `81295fd` ([run 42947234082](https://github.com/vendrell46/near-spend-guard/actions/runs/42947234082), step "Demo verdict lines"). A local run on 12 Nov printed the same lines apart from the hash:

```
race: daily cap 100 TST, 10 payments of 25 TST sent in parallel
naive guard        250 TST left (cap 100)
near-spend-guard   100 TST left, 6 waiting for a human (daily_cap)

price_swap: the human agreed to 50 TST; the filed request says 190 TST
naive guard        approve(1): api received 190 TST
near-spend-guard   approve(payload for 50): refused, payload does not match request 1; 0 TST moved

approval_flow: an agent pays for an API through near-spend-guard
pay 5 TST to api        paid (autonomous)
pay 500 TST to mallory  refused (receiver_not_allowed)
pay 60 TST to api       pending (above_threshold), request 3, hash 0e126a07...
approve request 3       paid 60 TST (approved)
```

The naive guard's number depends on block timing and varies between runs; its test asserts only that it is above the cap. Ours asserts exactly 100 on every run. The held payment's hash covers its expiry, which is set from the node's clock, so it differs on every run too. The naive guard is [`contracts/naive-guard`](contracts/naive-guard/src/lib.rs): the same policy, allowlist and approval deposit, different in exactly the two places its header names, and not for use.

## Why a contract

NEAR's own keys can't express a spending limit. A function-call key's allowance limits gas, not value, and nearcore rejects any function call that a function-call key signs with a deposit above zero (`DepositWithFunctionCall`). NEP-141 transfers need exactly 1 yoctoNEAR attached, so a function-call key can't move tokens at all, and a full-access key can do everything. The limit therefore has to live in a contract that holds the funds. The agent gets a function-call key on its own account that can call `request_payment` on the guard and nothing else, and the guard decides.

## What it uses from NEAR

- **Function-call access keys.** The agent's only key is limited to `request_payment` on the guard. nearcore refuses any deposit signed with it, so the agent can ask but can't pay.
- **Receipts and callbacks.** The reservation and the transfer go out from one receipt, and a `#[private]` callback settles it. Receipts on the guard run one after another, which is what keeps the caps exact under a burst.
- **NEP-141 tokens.** `ft_transfer`, and `ft_transfer_call` with partial refunds; the guard attaches the 1 yoctoNEAR each one needs. The test token is built on `near-contract-standards`.
- **Full-access keys and 1 yoctoNEAR on `approve`.** Only a full-access key can attach a deposit, so an approval is signed with the approver's own key, in near CLI or a wallet that shows the payload before signing.
- **NEP-297 events.** Every request, approval, refusal, settlement and configuration change is an `EVENT_JSON` log, standard `near-spend-guard`.
- **Storage staking.** The guard checks that it can still pay for its own storage before it reserves or sends anything, and refuses with `insufficient_balance` when it can't.
- **Tooling.** near-sdk 5.30.0 for the contracts, near-workspaces 0.24.0 with nearcore's sandbox 2.14.0 for tests on a real node, cargo-near 0.22.0 for the deployed files, near-api-js 7.4.0 for the client, near CLI for the approver's signature, and testnet for the demo.

## How it works

```
Human: owner and approver (full-access key)
   │  policy · approve(full payload) with 1 yocto · pause · reject · withdraw
   ▼
guard contract  ── holds NEAR and NEP-141 balances
   ▲  request_payment(token, receiver_id, amount, msg?, expires_at)
   │  function-call key, no deposit, request_payment only
Agent account (no contract)
```

- **Policy per token:** `per_payment_max` (a hard ceiling, approved payments included), `daily_max` (a rolling 24 hours, for payments made without a human), `approval_above`, and up to 32 allowed receivers. A receiver can be allowed `ft_transfer_call`, with `msg` as part of the payment. Up to 8 tokens, and one `paused` flag for all.
- **The budget is reserved in the receipt that sends the payment.** `request_payment` checks the caps, adds the amount to the current hour's slot of a 25-slot window, and sends the transfer with a callback, all in one receipt. Only the callback gives budget back, and only for what didn't move: a plain failure of a single-receipt transfer, or the part an `ft_transfer_call` refunded. Receipts on the guard run one after another, so each request sees every reservation made before it, and a burst can't read the same unspent total.
- **An approval covers one exact payment.** Above `approval_above`, or over the daily cap, the request waits. The guard stores the full payload (request id, token, receiver, amount, `msg`, expiry) and reserves nothing. The approver calls `approve` with that full payload from their own wallet, so the wallet shows the receiver and amount in plain text rather than a hash. The guard compares it field by field with the stored one, re-checks the current policy, and executes only that payload, before its expiry, once. `approve` needs exactly 1 yoctoNEAR, which takes a full-access key and a wallet confirmation.
- **Every outcome is an answer and an event.** Policy refusals return `refused` with a reason, held payments return `pending` with the request id and payload hash, and dispatched payments end with the callback's `paid`, `failed` or `uncertain`. NEP-297 events (standard `near-spend-guard`, version 1.0.0) record every request, approval, rejection, payment and configuration change.

## Threats and the test for each

| Threat | Defence | Test |
| --- | --- | --- |
| A compromised agent fires a burst to beat the daily cap | Reserve in the receipt that dispatches; release only in the callback | [`race_guard_exact`](sandbox-tests/tests/burst.rs#L23) (sandbox), [`reservation_interleavings`](contracts/guard/src/tests/payments.rs#L1503) (unit, property-based) |
| The agent splits a big payment into many small ones | Every payment counts for at least 24 hours | [`window_never_exceeds_cap`](contracts/guard/src/tests/payments.rs#L1418) (property-based), [`bucket_boundary`](contracts/guard/src/tests/payments.rs#L771) |
| The payload is swapped after the human agreed | `approve` carries the full payload, compared field by field | [`payload_swap_guard_refuses`](contracts/guard/src/tests/payments/approvals.rs#L158), [`approval_requires_exact_payload`](contracts/guard/src/tests/payments/approvals.rs#L106) (each field changed in turn) |
| An approval is reused or used late | The entry is deleted on approve; expiry checked; at most 7 days out | [`approval_single_use`](contracts/guard/src/tests/payments/approvals.rs#L176), [`approval_at_exact_expiry`](contracts/guard/src/tests/payments/approvals.rs#L187) |
| The agent approves its own request | The agent can't be the approver; `approve` checks the caller and needs 1 yocto | [`new_refuses_agent_as_approver`](contracts/guard/src/tests.rs#L215), [`set_approver_refuses_agent`](contracts/guard/src/tests.rs#L549), [`function_call_key_cannot_approve`](sandbox-tests/tests/wrapper.rs#L213) (nearcore's `DepositWithFunctionCall`) |
| A compromised agent prints an approval command for a look-alike contract | `nsg-approve` takes only a request number and reads the payment from the guard named in the approver's own config; near CLI shows receiver, method, arguments and deposit before signing | [`nsg_approve_ignores_agent_output`](client/test/approve.test.ts#L326) (the look-alike's words on the command line, on stdin and in the environment) |
| An agent retries after a timeout and pays twice | Once sent, `pay` never says "failed" unless the first receipt failed; it says `unknown`, with the hash and "do not pay again before checking" | [`giving up never reads as 'nothing happened'`](client/test/pay.test.ts#L186), [`a failure after the first receipt doesn't say nothing moved`](client/test/pay.test.ts#L290), [`a_second_process_on_the_key_costs_one_answer`](sandbox-tests/tests/client.rs#L412) |
| A wider key is handed to the agent by mistake | `nsg-pay-mcp` won't start unless its key is a function-call key for `request_payment` on this guard only | [`the_server_refuses_a_key_that_can_do_more`](sandbox-tests/tests/client.rs#L434), [`the agent's key has to be the narrow one`](client/test/pay.test.ts#L327) |
| `msg` hides something on the approver's screen | Printed escaped, with a warning if anything needed escaping | [`a message can't hide in the terminal`](client/test/approve.test.ts#L146) |
| The owner tightened the policy after a request was made | `approve` re-checks pause, token, allowlist and ceiling | [`approve_after_disallow_refuses_and_removes`](contracts/guard/src/tests/payments/approvals.rs#L239), [`approval_rechecks_the_current_policy`](contracts/guard/src/tests/payments/approvals.rs#L268) |
| The agent fills the guard's storage with pending requests | At most 16 pending; `msg` at most 1024 bytes; allowlist checked at request time; expired entries removed by anyone, and by the next held request when the list is full | [`seventeenth_pending_is_refused`](contracts/guard/src/tests/payments.rs#L664), [`a_full_list_makes_room_from_expired_entries`](contracts/guard/src/tests/payments.rs#L706), [`prune_expired_removes_only_expired`](contracts/guard/src/tests/payments/approvals.rs#L408), [`pending_entry_storage`](sandbox-tests/tests/limits.rs#L99) |
| The agent makes the guard pay receivers' storage | The guard never calls `storage_deposit`; an unregistered receiver makes `ft_transfer` fail and the callback releases the budget | [`unregistered_receiver_releases_budget`](contracts/guard/src/tests/payments.rs#L958) |
| `ft_transfer_call` fails after the tokens moved | Counted as spent (`uncertain`); a partial refund gives back exactly what came back | [`ft_transfer_call_failure_keeps_reservation`](contracts/guard/src/tests/payments.rs#L1144), [`ft_transfer_call_partial_refund`](contracts/guard/src/tests/payments.rs#L1123) |
| The callback runs out of gas | Callback gas reserved at dispatch, prepaid gas checked before reserving; a failed callback leaves the reservation (lost budget, never lost funds) | [`callback_gas_headroom`](sandbox-tests/tests/gas.rs#L21) (at least twice the measured gas), [`gas_with_a_full_policy`](sandbox-tests/tests/limits.rs#L121) |
| Someone else calls the callback | `#[private]` | [`callback_is_private`](sandbox-tests/tests/wrapper.rs#L108) |
| A release lands after its hourly slot rotated out | The callback carries the slot's epoch; a mismatch is a no-op | [`release_after_slot_rotated_is_noop`](contracts/guard/src/tests/payments.rs#L1189) |
| Overflow near `u128::MAX` | Checked arithmetic; an overflowing total is over the cap | [`caps_near_u128_max`](contracts/guard/src/tests/payments.rs#L831) |
| A token returns an over-long or malformed result after moving the funds | The callback reads the result itself, bounded to 64 bytes; only a plain failure of a single-receipt transfer releases budget | [`callback_too_long_result_keeps_reservation`](contracts/guard/src/tests/payments.rs#L1067), [`callback_unparseable_transfer_call_counts_as_used`](contracts/guard/src/tests/payments.rs#L1162) |
| A compromised agent pays itself | The agent can never be a receiver, the owner or the approver | [`agent_cannot_be_receiver`](contracts/guard/src/tests.rs#L448) |
| A payment would leave the guard unable to pay for its own storage | Refused before anything is reserved; `approve` and `withdraw` panic and change nothing | [`guard_refuses_what_it_cannot_afford`](sandbox-tests/tests/settlement.rs#L180), [`approve_waits_until_the_guard_can_pay`](sandbox-tests/tests/settlement.rs#L238), [`withdraw_stops_at_the_storage_cost`](contracts/guard/src/tests/payments.rs#L1291) |
| A native payment to an account that doesn't exist | A named account: `failed`, refunded, budget released. An implicit account: created and `paid` | [`native_payment_to_an_account_that_does_not_exist`](sandbox-tests/tests/settlement.rs#L336) |
| A full guard runs out of gas where it matters | Measured at 8 tokens, 32 receivers each and 16 pending entries, with the longest ids and `msg` | [`gas_with_a_full_policy`](sandbox-tests/tests/limits.rs#L121), [`full_guard_stays_manageable`](sandbox-tests/tests/limits.rs#L218) |

## Letting an agent pay for APIs

`client/bin/nsg-pay-mcp.ts` is an MCP server with one tool, `pay(token, receiver_id, amount, msg?, expires_in_s?)`, with `amount` in whole tokens as an invoice writes it (`"5.25"`). It holds only the agent's function-call key, refuses to start with a key that can do more than call `request_payment` on its guard, and answers in plain text: `paid`, `refused` with the reason, `pending` with what the approver will see and the command they run (`nsg-approve <id>`), or `unknown` with the transaction hash when it can't tell. The approver's `nsg-approve <id>` reads the request from the guard, prints it, and starts near CLI at the point where it asks how to sign; near CLI then shows the unsigned transaction with its arguments in full.

Run a real agent against it with no testnet, on a local node that stays up:

```bash
scripts/local-node.sh /tmp/nsg-local
scripts/local-agent.sh /tmp/nsg-local "Pay the 5 TST invoice for 1,000 API calls: 5 TST to api.test.near, TST is the token at tst.test.near."
```

`scripts/local-agent.sh` runs Claude Code as the MCP host with `pay` as its only tool (no shell, files or web) and prints every tool call and answer.

## On testnet

The guard [`nsg2-guard.testnet`](https://testnet.nearblocks.io/address/nsg2-guard.testnet) runs the build of commit [`308f5cd`](https://github.com/vendrell46/near-spend-guard/commit/308f5cd): its code hash on chain, `9M3HCJ5Jan6ZtkpVLvD7juS7Tk3cHfUgc9MWjzyW5HiS`, is the `near_spend_guard.wasm` line of `target/deploy/SHA256SUMS` (`7bffb04f…`) in base58. It holds TST, a test token of this repo (`tst.nsg2-demo.testnet`, 6 decimals; its owner can make transfers fail, for the tests), under the demo's policy: at most 200 TST per payment, a human above 40, and 100 a day without one. `api.nsg2-demo.testnet` is allowed; `mallory.nsg2-demo.testnet` is registered with the token but not allowed.

| Who | Asked | `pay` answered | Transaction |
| --- | --- | --- | --- |
| `scripts/demo-scene.sh 3` | 5 TST to `api` | `paid: 5 TST to api.nsg2-demo.testnet (request 1)` | [DtUSKS…](https://testnet.nearblocks.io/txns/DtUSKSfzQnbnSaq5R5gEbJ8gvQDUgVZJopKmdvcHUjjq) |
| `scripts/demo-scene.sh 3` | 500 TST to `mallory` | `refused (receiver_not_allowed): nothing was sent or reserved (request 2)` | [BXNf86…](https://testnet.nearblocks.io/txns/BXNf86xp9J6wfBYjoHWzZ7xXZJZcJ8V7CzP2knHo3wuj) |
| `scripts/demo-scene.sh 3` | 60 TST to `api` | `pending (above_threshold): request 3 waits for a human` | [8EdP8g…](https://testnet.nearblocks.io/txns/8EdP8g1o7Ju1wb3EbBW4oqKxEPPci2Zr35r7wBXALT9c) |
| Claude Code, `pay` its only tool | "Pay the 5 TST invoice for 1,000 API calls: 5 TST to api.nsg2-demo.testnet, TST is the token at tst.nsg2-demo.testnet." | `paid: 5 TST to api.nsg2-demo.testnet (request 4)` | [2S2rf7…](https://testnet.nearblocks.io/txns/2S2rf7tHgVK5bXzfdEizUfVjBAMtTR8cMge52hsbnYK9) |

The last transaction's logs, read from the RPC: the guard's `payment_dispatched` (mode `autonomous`, method `ft_transfer`), the token's NEP-141 `ft_transfer` with memo `nsg:4`, and the guard's `payment_settled` (outcome `paid`, `released` 0). Its return value is `{"status":"paid","request_id":"4","sent":"5000000"}`.

The held request in full, as `pay` printed it for the agent:

```
pending (above_threshold): request 3 waits for a human
amount   60 TST (60000000 base units)
receiver api.nsg2-demo.testnet
expires  2026-11-11T02:49:51.053Z
hash     5d60f001aeb93966e92bd001e6e23596c1e8fcec84d17bd4fed8f48d28fe2080
payload  {"request_id":"3","token":{"ft":"tst.nsg2-demo.testnet"},"receiver_id":"api.nsg2-demo.testnet","amount":"60000000","msg":null,"expires_at":"1794365391053650914"}
The approver reads it from the guard with: nsg-approve 3
```

`nsg-approve 3` read the same request from `nsg2-guard.testnet` and prepared near CLI's `contract call-function as-transaction nsg2-guard.testnet approve` with that payload as its JSON arguments, 100 Tgas and 1 yoctoNEAR, signed as `nsg2-approver.testnet`. Request 3 then expired unapproved, and on 12 Nov `scripts/demo-scene.sh tidy` removed it with `prune_expired` ([FQsHac…](https://testnet.nearblocks.io/txns/FQsHacEo5rbgPSxeVNuTDWk5H7KmiJRWwke2qsBCU6u1), event `request_expired`, signed by `nsg2-demo.testnet`, which holds no role on the guard).

To put a world of your own on testnet: `scripts/build-deployable.sh`, `scripts/test-deployable.sh`, then `scripts/deploy-testnet.sh <prefix>`. It uses testnet's faucet and the connection from near CLI's `config.toml`, refuses any chain but testnet, and stops if the files no longer match `SHA256SUMS`. [AGENTS.md](AGENTS.md) has the details.

## Alongside OutLayer

[OutLayer](https://outlayer.fastnear.com/docs/agent-custody) runs a full agent policy engine in a TEE: per-transaction limits, hourly to monthly caps, allowlists, multisig and freeze. Its documentation is candid about two edges. Velocity caps are "best-effort under concurrency": the counter comes from its API gateway, and concurrent requests "can each read the same pre-spend counter and all pass". And for NEAR Intents operations the TEE "does not itself re-check the token or amount". The guard does those two things on chain, with no TEE to trust: exact accounting under concurrency, and approvals bound to the full payload. It can stand alone, or sit behind an off-chain engine as a hard backstop on the account that holds the funds. (OutLayer's docs read 11 Nov 2026.)

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

- **A full pending list now makes room from its expired entries.** An expired request can never be approved, but it kept its place among the 16 until someone called `prune_expired`, and the agent's key can't. So a guard whose approver let 16 requests lapse would have refused every later held payment with `too_many_pending`. Now a held request that finds the list full removes the expired entries first, each with a `request_expired` event, and is refused only if none had expired. On a full guard (8 tokens of 32 receivers, 16 entries with the longest ids and `msg`) that request burns 9.25 TGas of the 30 the client attaches ([`full_guard_stays_manageable`](sandbox-tests/tests/limits.rs#L218)).
- **Two limitations written down** (above): a configured token is trusted to implement `ft_transfer` as one receipt, and `withdraw`'s event marks the transfer being sent, not its outcome.

What was checked and left as it is:

- **Rounding.** The only division is the hour slot, so a payment counts for between 24 and 25 hours ([`bucket_boundary`](contracts/guard/src/tests/payments.rs#L771)). An `ft_transfer_call` refund gives back exactly what came back, capped at the amount. Release builds keep overflow checks on.
- **Storage.** Every check reads the guard's balance before the receipt's own writes, against what that receipt adds: a pending entry (1,500 bytes allowed, 1,369 measured), the amount for a native payment, or 1 yocto for a token transfer. Dispatch adds no storage, and the callback only rewrites or shrinks.
- **Callbacks.** The callback reads the transfer's result bounded to 64 bytes and can't panic on it; out of gas, the reservation stays spent.
- **Between a payment and its callback.** Another request reserves first and sees this one; `approve` counts separately; removing and re-adding a token makes the late callback a no-op; a `withdraw` of tokens in flight makes the transfer fail, and the release is right. An allowlisted contract called through `ft_transfer_call` finds only views, `prune_expired` and methods that check their caller.

## Libraries, templates and AI use

- **Libraries.** `near-sdk` 5.30.0 is the only dependency of the guard and the naive guard. The test token adds `near-contract-standards` 5.30.0 for NEP-141. The sandbox tests use `near-workspaces` 0.24.0, `tokio`, `serde_json` and `anyhow`. The client's only npm dependency is `near-api-js` 7.4.0, loaded only by `client/src/near.ts`; its MCP server is plain JSON-RPC over stdio, with no MCP SDK.
- **Templates.** None. The crates and manifests were written by hand, not generated with `cargo near new`.
- **AI-generated code.** The code, tests and documentation were written with an AI coding agent (Claude Code), working from a design and an interface spec that the author approved before the build started.
- **Narration.** The demo video's voice-over is synthetic: generated with ElevenLabs from a clone of the author's own voice.
- **When.** All of it during NEAR HACKATHON; the first commit is from 1 Nov 2026.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
