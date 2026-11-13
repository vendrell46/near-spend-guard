# Demo

The demo video has three scenes. Each is one command, [`scripts/demo-scene.sh`](../scripts/demo-scene.sh):

```
usage: scripts/demo-scene.sh 1|2|3|tidy [...]
```

| Scene | Command | Runs | Where |
| --- | --- | --- | --- |
| 1, `race` | `scripts/demo-scene.sh 1 [cargo flags]` | `scripts/test-sandbox.sh --test demo race -- --nocapture` | a local sandbox node |
| 2, `price_swap` | `scripts/demo-scene.sh 2 [cargo flags]` | `scripts/test-sandbox.sh --test demo price_swap -- --nocapture` | a local sandbox node |
| 3, an agent pays | `scripts/demo-scene.sh 3 [<dir>]` | `node client/deploy/scene3.ts <dir>` | testnet by default, or a local node |
| before recording | `scripts/demo-scene.sh tidy [<dir>]` | `node client/deploy/tidy.ts <dir>` | testnet |

Scenes 1 and 2 need what the sandbox tests need ([Testing](testing.md#setup)). After a first fetch, pass `--offline --locked`, for example `scripts/demo-scene.sh 1 --offline --locked`, or run the test directly:

```bash
scripts/test-sandbox.sh --offline --locked --test demo race -- --nocapture
scripts/test-sandbox.sh --offline --locked --test demo price_swap -- --nocapture
scripts/test-sandbox.sh --offline --locked --test demo approval_flow -- --nocapture
```

Code comments that cite `DEMO.md` mean this page. The tests are in [`sandbox-tests/tests/demo.rs`](../sandbox-tests/tests/demo.rs). The naive guard they compare against is [`contracts/naive-guard`](../contracts/naive-guard/src/lib.rs): the same policy, allowlist and approval deposit, different in exactly the two places its header names, and not for use.

## The demo policy

Both the sandbox world and the testnet world use the same policy for TST (a test token of this repo, 6 decimals): at most 200 TST per payment, a human above 40, and 100 a day without one. The guard holds 1,000 TST. `api` is allowed; `mallory` is registered with the token but not allowed. The local node also allows `bob`.

## Scene 1: `race`

Ten payments of 25 TST sent in parallel, against a daily cap of 100. Both guards start fresh with the same policy. The test first sends one refused request to warm the key's nonce cache, then broadcasts all ten before polling any.

```
race: daily cap 100 TST, 10 payments of 25 TST sent in parallel
naive guard        250 TST left (cap 100)
near-spend-guard   100 TST left, 6 waiting for a human (daily_cap)
```

What it asserts: on our guard, exactly 100 TST leave and 6 requests are `pending` with reason `daily_cap`, on every attempt. On the naive guard, more than 100 leave. That number depends on block timing and varies between runs; 250 is what CI printed. An attempt is reported only if at least 5 of our 10 requests ran before the first callback and the naive guard went over the cap. Otherwise the burst wasn't really parallel, and the test repeats on fresh contracts, up to 5 attempts, printing `race: attempt <n> wasn't parallel (<k> of 10 before the first callback); rerunning` to stderr.

## Scene 2: `price_swap`

The human agreed to pay 50 TST. The request the agent filed says 190.

```
price_swap: the human agreed to 50 TST; the filed request says 190 TST
naive guard        approve(1): api received 190 TST
near-spend-guard   approve(payload for 50): refused, payload does not match request 1; 0 TST moved
```

The naive guard's `approve` takes a request number, so the approver signs `1` and the guard pays whatever request 1 says. Ours takes the full payload. The approver signs the payment they agreed to, 50 TST, the guard compares it with the stored 190, and the call fails with `payload does not match request 1`. Nothing moves, and the request stays pending, unchanged: the approver can still reject it, or approve the 190 deliberately.

## Scene 3: an agent pays for an API

The agent's side is one MCP tool, `pay`. A 5 TST invoice is paid on its own. 500 TST to mallory, who is not on the allowlist, is refused, and nothing is sent or reserved. A 60 TST payment, above the approval threshold, waits for a human. The approver runs `nsg-approve <id>`, near CLI shows the unsigned transaction in full, and once they sign, the guard pays.

The demo video shows `scripts/demo-scene.sh 3`, which makes these three calls without a model. It starts `nsg-pay-mcp` with the world's `agent.env`, speaks MCP to it as a host does, prints each answer, and fails unless they are `paid`, `refused (receiver_not_allowed)` and `pending (above_threshold)`. The held request expires in 15 minutes, so a rehearsal leaves nothing behind. Its output has this shape (the answers are `pay`'s, [Agent integration](agent-integration.md#the-answers)):

```
> pay 5 TST to api.<prefix>-demo.testnet
paid: 5 TST to api.<prefix>-demo.testnet (request <n>)
transaction <hash>

> pay 500 TST to mallory.<prefix>-demo.testnet
refused (receiver_not_allowed): nothing was sent or reserved (request <n>)
transaction <hash>

> pay 60 TST to api.<prefix>-demo.testnet
pending (above_threshold): request <n> waits for a human
amount   60 TST (60000000 base units)
...
The approver reads it from the guard with: nsg-approve <n>
transaction <hash>

The approver, in their own terminal:
  XDG_CONFIG_HOME=<dir>/config node client/bin/nsg-approve.ts <n>

scene 3: paid, refused, pending, as DEMO.md has it
```

`<dir>` defaults to `~/.config/near-spend-guard/testnet`, which [`scripts/deploy-testnet.sh`](../scripts/deploy-testnet.sh) writes ([Deploy](deploy.md)). Without one it stops with `<dir> has no demo world: run scripts/deploy-testnet.sh <prefix> (or scripts/local-node.sh <dir>)`.

With a model instead, Claude Code takes the host's place, with `pay` as its only tool (request 4 in the [README's testnet table](../README.md#on-testnet) came from this):

```bash
scripts/local-agent.sh ~/.config/near-spend-guard/testnet "Pay the 5 TST invoice for 1,000 API calls: 5 TST to api.nsg2-demo.testnet, TST is the token at tst.nsg2-demo.testnet."
```

### Scene 3 with no testnet

`approval_flow` is scene 3 on a sandbox node, including the approval, as a test:

```
approval_flow: an agent pays for an API through near-spend-guard
pay 5 TST to api        paid (autonomous)
pay 500 TST to mallory  refused (receiver_not_allowed)
pay 60 TST to api       pending (above_threshold), request 3, hash 0e126a07...
approve request 3       paid 60 TST (approved)
```

It asserts each status, that the refused request used up id 2, that the approver signs exactly the stored payload, that `api` received 65 TST, and that the approved 60 didn't count against the daily cap. The hash covers the expiry, which comes from the node's clock, so it differs on every run.

A real agent on a local node that stays up, in two terminals:

```bash
scripts/local-node.sh /tmp/nsg-local
scripts/local-agent.sh /tmp/nsg-local "Pay the 5 TST invoice for 1,000 API calls: 5 TST to api.test.near, TST is the token at tst.test.near."
```

`scripts/demo-scene.sh 3 /tmp/nsg-local` runs scene 3 against the same node. The directory must be empty or new. `touch /tmp/nsg-local/stop` ends the node; it also ends itself after `NSG_LOCAL_MINUTES`, default 120.

## Before recording: `tidy`

`scripts/demo-scene.sh tidy` removes expired requests from the testnet guard with `prune_expired`, signed by the demo account, which holds no role on the guard. It lists what is still pending and checks that scene 3 can run: not paused, 5 TST of today's budget, 65 TST in the guard, 0.1 NEAR above storage for the guard and for the approver, 0.1 NEAR of gas allowance left on the agent's key, and nothing pending. It prints:

```
nothing expired
nothing pending
paused          no
budget left     <n> TST today without a human
guard holds     <n> TST, <n> NEAR above its storage
agent's key     <n> NEAR of gas allowance left
approver        <n> NEAR above its storage
ready to record
```

When something expired, the first line is `pruned <n> expired: <explorer link>`. It ends with `ready to record`, or with one `not ready: <problem>` line per problem and exit code 1.

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

To put a world of your own on testnet: `scripts/build-deployable.sh`, `scripts/test-deployable.sh`, then `scripts/deploy-testnet.sh <prefix>`. It uses testnet's faucet and the connection from near CLI's `config.toml`, refuses any chain but testnet, and stops if the files no longer match `SHA256SUMS`. [Deploy](deploy.md) has the details.

