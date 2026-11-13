# How it works

The guard is one Rust contract, [`contracts/guard`](../contracts/guard/src/lib.rs). It holds the money and keeps the ledger. This page follows a payment from the agent's request to the guard's ledger. The source is the reference: constants, error strings and event names below are quoted from it.

## Accounts and roles

```
Human: owner and approver (full-access key)
   │  policy · approve(full payload) with 1 yocto · pause · reject · withdraw
   ▼
guard contract  ── holds NEAR and NEP-141 balances
   ▲  request_payment(token, receiver_id, amount, msg?, expires_at)
   │  function-call key, no deposit, request_payment only
Agent account (no contract)
```

- **Owner.** Configures tokens, receivers, the agent and the approver, withdraws, and unpauses. Every owner method needs exactly 1 yoctoNEAR attached, which only a full-access key can sign.
- **Approver.** Approves or rejects held payments, and can pause. `approve` needs exactly 1 yoctoNEAR; `pause` and `reject` need none.
- **Agent.** Calls `request_payment` and nothing else. Its key is a function-call key for `request_payment` on the guard. nearcore refuses a deposit from a function-call key (`DepositWithFunctionCall`), so the key can ask but can't pay.
- **The guard itself.** It can't be the owner, the approver, the agent, a receiver or a token. The agent can't be the owner, the approver or a receiver (`check_agent`, `allow_receiver`, `set_agent`).

The roles are set in `new(owner_id, agent_id, approver_id, slot_ns)`, which is `#[init]` and `#[private]`: deploy and initialise in one transaction. The owner and the approver may be the same account. On the testnet demo they are.

## Policy per token

A token is `"native"` (NEAR) or `{"ft": "<token contract>"}` (NEP-141). `set_token` sets three numbers per token, all in the token's smallest unit:

| Field | Meaning |
| --- | --- |
| `per_payment_max` | A hard ceiling. A payment above it is refused, approved or not. |
| `approval_above` | A payment above it waits for a human. It can't exceed `per_payment_max`. |
| `daily_max` | What the agent may move without a human over the rolling window. A payment that would take the window over it waits for a human. |

Each token has up to 32 allowed receivers (`allow_receiver`). A receiver can be allowed `ft_transfer_call` (`ft_call: true`); then a payment to it may carry a `msg`. The guard holds up to 8 tokens. One `paused` flag covers all of them.

The limits, as `get_config` returns them in `limits`:

| Constant | Value |
| --- | --- |
| `WINDOW_SLOTS` | 25 |
| `MAX_PENDING` | 16 |
| `MAX_RECEIVERS` | 32 per token |
| `MAX_TOKENS` | 8 |
| `MAX_MSG_BYTES` | 1024 |
| `MAX_EXPIRY_NS` | 7 days |
| `MIN_SLOT_NS` | 1 second |

## `request_payment`, step by step

`request_payment(token, receiver_id, amount, msg, expires_at)` runs these steps in order ([`lib.rs`](../contracts/guard/src/lib.rs)).

1. **Malformed input panics, and nothing is recorded.** The caller must be the agent. `amount` must be above 0. The receiver can't be the guard. `expires_at` must be in the future and at most 7 days ahead. `msg` needs an FT token and at most 1024 bytes. The prepaid gas must cover the transfer, the callback and a margin (`not enough gas for the transfer and its callback`).
2. **The request gets an id.** Every request that passes step 1 gets the next id, refused ones included.
3. **Policy refusals.** In this order: `paused`, `token_not_configured`, `receiver_not_allowed`, `transfer_call_not_allowed` (a `msg` to a receiver without `ft_call`), `above_per_payment_max`. A refusal returns `refused` with the reason, emits `payment_refused`, and sends and reserves nothing.
4. **Held for a human.** If `amount > approval_above`, the reason is `above_threshold`. Otherwise, if the window's spending plus `amount` would exceed `daily_max`, the reason is `daily_cap`. See [Pending approvals](#pending-approvals).
5. **Can the guard afford it?** The guard checks that its NEAR above its storage cost covers what the transfer takes: the amount for a native payment, or 1 yoctoNEAR for a token transfer. If not, it refuses with `insufficient_balance` before reserving anything.
6. **Reserve and dispatch, in the same receipt.** The guard adds `amount` to the current slot of the window, sends the transfer, and chains the callback `on_payment_settled`. It emits `payment_dispatched` and returns the promise, so the agent's transaction ends with the callback's answer.

The transfer is one of:

| Method | When | What the guard sends |
| --- | --- | --- |
| `native` | `token` is `"native"` | A NEAR transfer of `amount` yoctoNEAR |
| `ft_transfer` | FT, no `msg` | `ft_transfer(receiver_id, amount, memo: "nsg:<request_id>")` with 1 yoctoNEAR and 10 TGas |
| `ft_transfer_call` | FT with `msg` | `ft_transfer_call(receiver_id, amount, memo, msg)` with 1 yoctoNEAR and 60 TGas |

The callback gets 10 TGas of static gas. The guard never calls `storage_deposit` for a receiver.

## Reserved and spent

The guard keeps no separate "reserved" ledger. A payment is added to its slot when it is dispatched, before anyone knows whether it will succeed. From then on it counts against the cap. Only the callback can give budget back, and only for what didn't move.

This is what keeps the cap exact under a burst. Receipts on one contract run one after another, so each request sees every reservation made before it. Ten requests in the same block can't all read the same unspent total. The naive guard in [`contracts/naive-guard`](../contracts/naive-guard/src/lib.rs) counts a payment only in its callback, and that is the difference the `race` demo shows.

Approved payments are counted in the same slots, in a separate `approved` total. They are bounded by `per_payment_max`, not by `daily_max`.

## The rolling daily window

Time is cut into slots of `slot_ns` nanoseconds (one hour on testnet; `slot_ns` must be at least one second). A slot's epoch is `block_timestamp / slot_ns`. The window is 25 slots: the current one and the 24 before it. Spending counted against `daily_max` is the sum of the `autonomous` totals of the slots in the window.

So a payment counts for between 24 and 25 hours, depending on where in its hour it landed. Budget comes back when the payment's slot leaves the window. A slot that is reused for a later hour is emptied first.

The views show the window at the current block:

- `remaining_budget(token)`: `daily_max` minus the window's autonomous spending; 0 for a token that isn't configured.
- `get_token(token)` and `get_tokens()`: the policy, the receivers, `autonomous_spent`, `approved_spent`, `remaining`, and `next_release_at`, the end of the oldest slot with autonomous spending (`null` when nothing counts).

## The callback and its outcomes

`on_payment_settled` is `#[private]`: only the guard can call it. It reads the transfer's result itself, bounded to 64 bytes, rather than through `#[callback_result]`, so an over-long or malformed result can't make it panic. What the result means for the ledger (`settle` in `lib.rs`):

| Transfer | Result | Outcome | Budget given back |
| --- | --- | --- | --- |
| `native` or `ft_transfer` | success | `paid` | none |
| `ft_transfer_call` | success, a used amount | `paid`, `sent` = the used amount (at most `amount`) | `amount - sent`, what the receiver refunded |
| `ft_transfer_call` | success, unparseable | `paid` | none (counted as fully used) |
| `native` or `ft_transfer` | failure | `failed`, `sent` 0 | all of it |
| `ft_transfer_call` | failure | `uncertain` | none: the tokens may have moved |
| any | result longer than 64 bytes | `paid` | none |

Budget is given back only if two things still hold:

- **The token has the same generation.** Each time a token is added it gets a new generation. A late callback from before `remove_token` can't free budget in a re-added token's window.
- **The payment's slot still holds that hour.** If the slot rotated out, the payment already left the window, and the release is a no-op.

The `payment_settled` event records `outcome`, `sent` (`null` when unknown) and `released`, what the callback actually gave back.

### When the callback itself fails

The guard checks before reserving that the prepaid gas covers the transfer, 10 TGas for the callback and a 5 TGas margin. If the callback still fails, for example out of gas, its release never runs. The reservation stays: the budget is lost until the slot leaves the window, and no funds are lost. `callback_gas_headroom` in [`sandbox-tests/tests/gas.rs`](../sandbox-tests/tests/gas.rs) checks that the reserved gas is at least twice what each receipt burns.

## Pending approvals

A held request stores a `PendingEntry`: the full payload, its hash, the reason and the creation time. Nothing is reserved yet. The answer is `pending` with the request id, the reason and the payload hash, and the guard emits `approval_requested`.

The payload is:

```json
{
  "request_id": "3",
  "token": {"ft": "tst.nsg2-demo.testnet"},
  "receiver_id": "api.nsg2-demo.testnet",
  "amount": "60000000",
  "msg": null,
  "expires_at": "1794365391053650914"
}
```

Before storing, the guard makes room and checks limits:

- At most 16 entries. If the list is full, the guard first removes the entries that have expired, each with a `request_expired` event. If none had expired, the request is refused with `too_many_pending`.
- The guard must be able to pay for the entry's storage (1,500 bytes allowed per entry). If not, it refuses with `insufficient_balance`.

`get_pending(from_index, limit)` lists the entries in id order. `get_request(request_id)` returns one, or `null`.

### `approve`

`approve(payload)` takes the full payload, not a request id. The approver's wallet or near CLI therefore shows the receiver and the amount in plain text. The steps:

1. Exactly 1 yoctoNEAR attached, and the caller is the approver.
2. The stored entry for `payload.request_id` must exist and equal the payload field by field. Otherwise `approve` panics with `no pending request <id>` or `payload does not match request <id>`, and the entry stays, so the approver can still reject it.
3. If the request has expired, the entry is removed, `request_expired` is emitted, and `approve` returns `expired`.
4. If the guard is paused, `approve` panics (`the guard is paused`) and the entry waits. The prepaid gas is checked as in `request_payment`.
5. The current policy is checked again: token configured, receiver still allowed, `ft_call` still allowed, amount within `per_payment_max`. If the policy no longer allows the payment, the entry is removed, `request_rejected` is emitted with the refusal reason, and `approve` returns `refused`.
6. If the guard can't afford the transfer, `approve` panics (`the guard cannot afford this payment and its storage`) and the entry waits for the owner to top up.
7. The entry is removed (single use), the amount is counted as approved, and the payment is dispatched as above, with mode `approved`.

### Other ways an entry leaves

- `reject(request_id)`: the owner or the approver removes it, with `request_rejected` and reason `rejected`. No deposit, like `pause`.
- `prune_expired(limit)`: anyone removes expired entries, oldest id first, up to `limit` (default 16). It returns how many it removed. The agent's key can't call it; a held request on a full list does the same.
- `set_agent`: replacing the agent cancels every pending request, each with `request_rejected` and reason `agent_changed`.

## The payload hash

```
sha256(borsh((PAYLOAD_DOMAIN, guard account id, payload)))
```

`PAYLOAD_DOMAIN` is `near-spend-guard/payload/v1`. The guard's own account id is part of the hash, so a hash from one guard means nothing on another. The view `payload_hash(payload)` returns the hash as the guard computes it, in lowercase hex.

The hash is shown to the human, in `pay`'s `pending` answer and in `nsg-approve`, but it is not what binds the approval: `approve` compares the full payload. The client recomputes the hash itself (`client/src/payload.ts`) and warns if it differs from the guard's. Both implementations are checked against the same vectors, [`payload_vectors.json`](../contracts/guard/src/tests/payload_vectors.json).

## Expiry

The agent sets `expires_at` on every request, in `block_timestamp` nanoseconds, at most 7 days ahead. It matters only for held requests: `approve` at or after `expires_at` removes the entry instead of paying. `pay` sets `expires_at` from the chain's latest block time plus `expires_in_s` (default 600 seconds), not from the local clock.

## Return values

`request_payment` and `approve` return a JSON object tagged by `status`:

| `status` | Fields | Meaning |
| --- | --- | --- |
| `refused` | `request_id`, `reason` | Policy said no. Nothing reserved or stored. |
| `pending` | `request_id`, `reason`, `payload_hash` | Waiting for the approver. |
| `paid` | `request_id`, `sent` | Final, from the callback. `sent` is below the amount only after an `ft_transfer_call` refund. |
| `failed` | `request_id`, `released` | A single-receipt transfer failed: nothing moved, budget released. |
| `uncertain` | `request_id` | `ft_transfer_call` reported failure; tokens may have moved, so the budget stays spent. |
| `expired` | `request_id` | `approve` arrived after `expires_at`; the request is removed. |

Refusal reasons: `paused`, `token_not_configured`, `receiver_not_allowed`, `transfer_call_not_allowed`, `above_per_payment_max`, `too_many_pending`, `insufficient_balance`. Pending reasons: `above_threshold`, `daily_cap`.

## Events

Every event is a NEP-297 `EVENT_JSON:` log with standard `near-spend-guard` and version `1.0.0` ([`events.rs`](../contracts/guard/src/events.rs)). Each is emitted only from a call that succeeds, so an event always describes state that stuck.

| Event | Emitted by | Data |
| --- | --- | --- |
| `payment_dispatched` | a payment sent, by `request_payment` or `approve` | `request_id`, `mode` (`autonomous` or `approved`), `token`, `receiver_id`, `amount`, `method`, `payload_hash`, `window_spent`, `daily_max` |
| `payment_settled` | `on_payment_settled` | `request_id`, `outcome` (`paid`, `failed`, `uncertain`), `sent`, `released` |
| `payment_refused` | a policy refusal in `request_payment` | `request_id`, `reason`, `token`, `receiver_id`, `amount` |
| `approval_requested` | a held request | `request_id`, `reason`, `payload`, `payload_hash`, `window_spent` (before this payment), `daily_max` |
| `request_rejected` | `reject`, `set_agent`, or `approve` re-checking the policy | `request_id`, `by`, `reason` (`rejected`, `agent_changed`, or a refusal reason) |
| `request_expired` | `approve` too late, `prune_expired`, or a held request on a full list | `request_id` |
| `withdrawal` | `withdraw` | `token`, `amount`, `receiver_id` |
| `paused` / `unpaused` | `pause` / `unpause`, only when the state changes | `by` |
| `config_changed` | `set_token`, `remove_token`, `allow_receiver`, `disallow_receiver`, `set_agent`, `set_approver` | `change` (`token`, `token_removed`, `receiver_allowed`, `receiver_removed`, `agent`, `approver`), `token`, `account_id` |

A token transfer's own NEP-141 `ft_transfer` event carries the memo `nsg:<request_id>`, which ties it to the guard's events.

## The full method list

| Method | Who | Deposit | Notes |
| --- | --- | --- | --- |
| `new` | the guard account, at deploy | none | `#[init]`, `#[private]` |
| `set_token` | owner | 1 yocto | creates or updates a policy; an update keeps receivers and the window |
| `remove_token` | owner | 1 yocto | deletes policy, receivers and window |
| `allow_receiver` | owner | 1 yocto | adds a receiver or updates its `ft_call` flag |
| `disallow_receiver` | owner | 1 yocto | pending requests to it are refused at `approve` |
| `set_agent` | owner | 1 yocto | cancels every pending request |
| `set_approver` | owner | 1 yocto | pending requests stay |
| `withdraw` | owner | 1 yocto | any token, configured or not, also while paused; not counted in any cap; no callback |
| `pause` | owner or approver | none | works from a function-call key limited to `pause` |
| `unpause` | owner | 1 yocto | |
| `request_payment` | agent | none | |
| `approve` | approver | 1 yocto | |
| `reject` | owner or approver | none | |
| `prune_expired` | anyone | none | |
| `on_payment_settled` | the guard | none | `#[private]` |
| `get_config`, `get_tokens`, `get_token`, `remaining_budget`, `get_pending`, `get_request`, `payload_hash` | anyone | view | |
