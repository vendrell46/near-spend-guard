# near-spend-guard

An on-chain spending guard for AI agents on NEAR: caps that hold under parallel requests, and approvals that cover one exact payment.

Demo video: <!-- DEMO_VIDEO_LINK -->

Testnet only. Built at NEAR HACKATHON, 1–14 Nov 2026.

## The problem

AI agents are starting to pay for things on their own: API calls, data, compute. To pay, an agent needs a key. On NEAR that key is one of two kinds. A function-call key can't attach a deposit, so the agent can't pay at all. A full-access key can spend everything the account holds. There is nothing in between: no key that says "up to 100 a day, and only to these accounts".

## What it is

near-spend-guard is a contract that holds the agent's money and enforces its budget, for native NEAR and NEP-141 tokens:

- a cap per payment,
- a rolling daily cap,
- a receiver allowlist,
- a kill switch.

A payment above the approval threshold, or one that would go over the daily cap, waits for a human. The human approves the exact payment, not a request number: the approval carries the full payload, and their screen shows its receiver, amount and hash. The agent's own key can call `request_payment` on the guard and nothing else.

```
Human: owner and approver (full-access key)
   │  policy · approve(full payload) with 1 yocto · pause · reject · withdraw
   ▼
guard contract  ── holds NEAR and NEP-141 balances
   ▲  request_payment(token, receiver_id, amount, msg?, expires_at)
   │  function-call key, no deposit, request_payment only
Agent account (no contract)
```

The guard is a Rust contract, [`contracts/guard`](contracts/guard/src/lib.rs). A small TypeScript client gives an agent one MCP tool, `pay`, and gives the human approver `nsg-approve`.

## The demo

On testnet, Claude Code runs with one MCP tool, `pay`, and no shell, files or web.

- It pays a 5 TST invoice on its own: `paid`.
- It is asked to pay 500 TST to mallory, who is not on the allowlist: `refused (receiver_not_allowed)`. Nothing is sent or reserved.
- A 60 TST payment is above the approval threshold of 40: `pending`. It waits for a human.

The approver runs `nsg-approve <id>`. It reads the request from the guard, not from anything the agent printed, and starts near CLI, which shows the unsigned transaction in full. The approver signs, and the guard pays.

The testnet transactions are [below](#on-testnet); there, the refusal and the held payment were made by `scripts/demo-scene.sh 3`, which plays the same calls without a model. Each scene is one command, and the same flow runs on a local node with no testnet: [docs/demo.md](docs/demo.md).

## Why the limits hold

Naive limits fail in two ways. Both are shown in sandbox tests against a naive guard, [`contracts/naive-guard`](contracts/naive-guard/src/lib.rs): the same policy, allowlist and approval deposit, different in exactly the two places its header names, and not for use.

- **`race`.** Ten payments of 25 TST are sent in parallel against a daily cap of 100. The naive guard counts a payment only when it settles, so every request that lands before the first settlement reads the same total: in the CI run below, 250 TST left. near-spend-guard counts each payment in the receipt that sends it. Exactly 100 TST leave, and the other 6 payments wait for a human.
- **`price_swap`.** The human agreed to pay 50 TST. The request the agent filed says 190. The naive guard's approval takes a request number, so it pays 190. Ours takes the full payload and compares it field by field with the stored one. The human's 50 doesn't match, so it refuses ("payload does not match request 1"), and nothing moves.

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

The naive guard's number depends on block timing and varies between runs; its test asserts only that it is above the cap. Ours asserts exactly 100 on every run. The held payment's hash covers its expiry, which is set from the node's clock, so it differs on every run too.

## Why NEAR

NEAR's own keys can't express a spending limit. A function-call key's allowance limits gas, not value, and nearcore rejects any function call that a function-call key signs with a deposit above zero (`DepositWithFunctionCall`). NEP-141 transfers need exactly 1 yoctoNEAR attached, so a function-call key can't move tokens at all, and a full-access key can do everything.

What NEAR does give is a key that can call one method on one contract and nothing else. So the agent gets a function-call key on its own account that can call `request_payment` on the guard, the guard holds the funds, and the guard decides. The limit is enforced on chain, not promised by the agent's code.

What it uses from NEAR:

- **Function-call access keys.** The agent's only key is limited to `request_payment` on the guard. nearcore refuses any deposit signed with it, so the agent can ask but can't pay.
- **Receipts and callbacks.** The reservation and the transfer go out from one receipt, and a `#[private]` callback settles it. Receipts on the guard run one after another, which is what keeps the caps exact under a burst.
- **NEP-141 tokens.** `ft_transfer`, and `ft_transfer_call` with partial refunds; the guard attaches the 1 yoctoNEAR each one needs. The test token is built on `near-contract-standards`.
- **Full-access keys and 1 yoctoNEAR on `approve`.** Only a full-access key can attach a deposit, so an approval is signed with the approver's own key, in near CLI or a wallet that shows the payload before signing.
- **NEP-297 events.** Every request, approval, refusal, settlement and configuration change is an `EVENT_JSON` log, standard `near-spend-guard`.
- **Storage staking.** The guard checks that it can still pay for its own storage before it reserves or sends anything, and refuses with `insufficient_balance` when it can't.
- **Tooling.** near-sdk 5.30.0 for the contracts, near-workspaces 0.24.0 with nearcore's sandbox 2.14.0 for tests on a real node, cargo-near 0.22.0 for the deployed files, near-api-js 7.4.0 for the client, near CLI for the approver's signature, and testnet for the demo.

## Who it's for

- Teams whose agents pay for APIs, data or compute, and who want a hard limit on what a compromised or confused agent can spend.
- Marketplaces where agents get hired and paid, such as NEAR's Agent Market.

It ships as an MCP server, [`client/bin/nsg-pay-mcp.ts`](client/bin/nsg-pay-mcp.ts), so any agent that speaks MCP gets a `pay` tool with the guard behind it. The server holds only the agent's function-call key and refuses to start with a key that can do more. Its answers are plain text: `paid`, `refused` with the reason, `pending` with what the approver will see and the command they run, or `unknown` with the transaction hash when it can't tell. The tool's inputs and every answer are in [docs/agent-integration.md](docs/agent-integration.md).

Run a real agent against it with no testnet, on a local node that stays up:

```bash
scripts/local-node.sh /tmp/nsg-local
scripts/local-agent.sh /tmp/nsg-local "Pay the 5 TST invoice for 1,000 API calls: 5 TST to api.test.near, TST is the token at tst.test.near."
```

`scripts/local-agent.sh` runs Claude Code as the MCP host with `pay` as its only tool (no shell, files or web) and prints every tool call and answer.

Next: an independent audit, then mainnet. Today it runs on testnet only, and the client refuses mainnet.

## What it does, and what shows it

- **Caps hold under parallel requests.** Ten payments of 25 TST sent at once against a daily cap of 100: exactly 100 leave without a human and the other six wait for one, on a real NEAR node. The same policy counting each payment only when it settles let 250 leave, in CI and locally (that number varies with block timing). (`race`, above.)
- **A human approves one exact payment, not a request number.** `approve` carries the full payload, so the approver's wallet shows the receiver and the amount, and the guard compares it field by field with what the agent filed. If the request says 190 TST and the human approves the 50 they agreed to, the guard refuses and nothing moves. (`price_swap`.)
- **The agent's key can only ask.** It is a function-call key for `request_payment` on the guard, which nearcore won't let attach a deposit or call anything else. The `pay` server refuses to start with a wider key, and `nsg-approve` reads the payment from the guard rather than from anything the agent prints. ([Threats](docs/security-model.md#threats-and-the-test-for-each).)
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
| `scripts/test-sandbox.sh` | every sandbox test, including the sandbox tests named in the [threats table](docs/security-model.md#threats-and-the-test-for-each) |
| `scripts/test-client.sh` | the client's own tests (`# pass 120`), the near-api-js sender's (`# pass 9`), then `test result: ok. 5 passed`: `pay` and `nsg-approve` against the real contracts on a local node |
| `python3 -I tools/mutate.py . tools/clean /tmp/nsg-mutants/unit` | `caught` for each weakened check, then `91 mutants; survived: none` (a few minutes; the sandbox and client runners are in [`tools/`](tools/README.md)) |
| [CI](https://github.com/vendrell46/near-spend-guard/actions/workflows/ci.yml) | all of the above but the mutation runners on Linux, with the demo's lines in the step "Demo verdict lines" |
| [Testnet](#on-testnet) | the agent's payments, a refusal and a held payment, with their events |

`scripts/test-sandbox.sh` builds the guard and the test token to wasm, downloads nearcore's sandbox 2.14.0 once and checks it against a pinned SHA-256, then runs `cargo test -p sandbox-tests`; arguments go to `cargo test`. After a first fetch, `--offline --locked` skips the network, for example `scripts/test-sandbox.sh --offline --locked --test demo race -- --nocapture`. A test stops if a wasm file is older than its contract's sources. [AGENTS.md](AGENTS.md) has every build and test command, and [docs/testing.md](docs/testing.md) describes each test level.

## On testnet

The guard [`nsg2-guard.testnet`](https://testnet.nearblocks.io/address/nsg2-guard.testnet) runs the build of commit [`308f5cd`](https://github.com/vendrell46/near-spend-guard/commit/308f5cd). It holds TST, a test token of this repo (`tst.nsg2-demo.testnet`, 6 decimals), under the demo's policy: at most 200 TST per payment, a human above 40, and 100 a day without one. `api.nsg2-demo.testnet` is allowed; `mallory.nsg2-demo.testnet` is registered with the token but not allowed.

| Who | Asked | `pay` answered | Transaction |
| --- | --- | --- | --- |
| `scripts/demo-scene.sh 3` | 5 TST to `api` | `paid: 5 TST to api.nsg2-demo.testnet (request 1)` | [DtUSKS…](https://testnet.nearblocks.io/txns/DtUSKSfzQnbnSaq5R5gEbJ8gvQDUgVZJopKmdvcHUjjq) |
| `scripts/demo-scene.sh 3` | 500 TST to `mallory` | `refused (receiver_not_allowed): nothing was sent or reserved (request 2)` | [BXNf86…](https://testnet.nearblocks.io/txns/BXNf86xp9J6wfBYjoHWzZ7xXZJZcJ8V7CzP2knHo3wuj) |
| `scripts/demo-scene.sh 3` | 60 TST to `api` | `pending (above_threshold): request 3 waits for a human` | [8EdP8g…](https://testnet.nearblocks.io/txns/8EdP8g1o7Ju1wb3EbBW4oqKxEPPci2Zr35r7wBXALT9c) |
| Claude Code, `pay` its only tool | "Pay the 5 TST invoice for 1,000 API calls: 5 TST to api.nsg2-demo.testnet, TST is the token at tst.nsg2-demo.testnet." | `paid: 5 TST to api.nsg2-demo.testnet (request 4)` | [2S2rf7…](https://testnet.nearblocks.io/txns/2S2rf7tHgVK5bXzfdEizUfVjBAMtTR8cMge52hsbnYK9) |

The code hash on chain, the events in each transaction, the held request in full, and what `nsg-approve` prepared for it are in [docs/demo.md](docs/demo.md#on-testnet). To put a world of your own on testnet: `scripts/build-deployable.sh`, `scripts/test-deployable.sh`, then `scripts/deploy-testnet.sh <prefix>` ([docs/deploy.md](docs/deploy.md)).

## Documentation

- [How it works](docs/how-it-works.md): the request flow, reservations, the rolling window, pending approvals, the payload hash, expiry, outcomes and events.
- [Security model](docs/security-model.md): what is enforced on chain and off chain, trust assumptions, owner powers, the threats table with the tests for each, limitations, the self-audit.
- [Agent integration](docs/agent-integration.md): the `pay` MCP server, its inputs and answers, the agent's key, and `nsg-approve`.
- [Testing](docs/testing.md): every test level, how to run it and what it prints.
- [Demo](docs/demo.md): each demo scene as a command, in the sandbox and on testnet, with its output.
- [Deploy](docs/deploy.md): building the deployed files and putting a demo world on testnet.

## Limitations

The main ones, all of them in [docs/security-model.md](docs/security-model.md#limitations-and-owner-powers):

- **No independent audit yet.** Testnet only.
- **Caps are per token.** A cap across tokens in one currency would need an oracle.
- **The owner is trusted.** The owner can withdraw everything, change any policy, reset a token's window by removing and re-adding it, and replace the agent. In production the guard account's own full-access key must be deleted after deployment; the testnet demo keeps one.
- **Approved payments sit outside `daily_max` by design.** They are bounded by `per_payment_max`.
- **A configured token is trusted to behave as NEP-141.** Configure only tokens whose code you trust.
- **`pay` isn't idempotent.** Two calls are two payments, bounded by the caps. After `unknown`, an agent must check the transaction before paying again; the answer says so.
- **An agent can still spend within its policy.** The guard bounds what a compromised agent can move to allowed receivers; it doesn't make every allowed payment a good one.

## Libraries, templates and AI use

- **Libraries.** `near-sdk` 5.30.0 is the only dependency of the guard and the naive guard. The test token adds `near-contract-standards` 5.30.0 for NEP-141. The sandbox tests use `near-workspaces` 0.24.0, `tokio`, `serde_json` and `anyhow`. The client's only npm dependency is `near-api-js` 7.4.0; in `client/src` only `near.ts` loads it, and the testnet scripts in `client/deploy` use it too. Its MCP server is plain JSON-RPC over stdio, with no MCP SDK.
- **Templates.** None. The crates and manifests were written by hand, not generated with `cargo near new`.
- **AI-generated code.** The code, tests and documentation were written with an AI coding agent (Claude Code), working from a design and an interface spec that the author approved before the build started.
- **Narration.** The demo video's voice-over is synthetic: generated with ElevenLabs from a clone of the author's own voice.
- **When.** All of it during NEAR HACKATHON; the first commit is from 1 Nov 2026.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
