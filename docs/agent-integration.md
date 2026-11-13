# Agent integration

The client in [`client/`](../client) has two programs: the agent's MCP server, `nsg-pay-mcp`, and the approver's helper, `nsg-approve`. Both are declared as `bin` entries in `client/package.json`. Node 22.22.2 or later runs their TypeScript directly; there is no build step. Run them as `node client/bin/nsg-pay-mcp.ts` and `node client/bin/nsg-approve.ts <request_id>`.

In `client/src`, only `near.ts` loads npm code (`near-api-js` 7.4.0), and of the two programs only `nsg-pay-mcp` imports it, so `nsg-approve` runs on Node alone. The testnet scripts in `client/deploy` also use near-api-js. The MCP server is plain newline-delimited JSON-RPC over stdio, written by hand, with no MCP SDK.

## The `pay` MCP server

[`client/bin/nsg-pay-mcp.ts`](../client/bin/nsg-pay-mcp.ts) serves one tool, `pay`. It holds only the agent's function-call key.

### Settings

From the environment:

| Variable | Meaning |
| --- | --- |
| `NSG_GUARD_ID` | the guard contract |
| `NSG_AGENT_ID` | the agent's account |
| `NSG_RPC_URL` | a NEAR RPC endpoint |
| `NSG_AGENT_KEY` | the agent's function-call secret key; removed from the process environment once read |

A missing setting stops it with, for example, `nsg-pay-mcp: NSG_GUARD_ID is not set`, and exit code 1.

The local node and the testnet deploy write these into an `agent.env` file (mode 0600) and an `mcp.json` that starts the server with `node --env-file`, so the key reaches the server and never the MCP host's environment:

```json
{
  "mcpServers": {
    "near-spend-guard": {
      "type": "stdio",
      "command": "node",
      "args": ["--env-file=<dir>/agent.env", "<repo>/client/bin/nsg-pay-mcp.ts"]
    }
  }
}
```

### What it checks before serving

1. The RPC's chain isn't mainnet (`near-spend-guard's client runs on testnet only`).
2. The guard's `get_config` names this agent as its `agent_id`.
3. The key is a function-call key whose receiver is the guard and whose only method is `request_payment`. Anything else, a full-access key included, stops it with `refusing to run: this key must be a function-call key for request_payment on <guard>, and it is <permission>`.

Tests: [`the_server_refuses_a_key_that_can_do_more`](../sandbox-tests/tests/client.rs) (sandbox) and `the agent's key has to be the narrow one` in [`client/test/pay.test.ts`](../client/test/pay.test.ts).

### The key the agent needs

A function-call access key on the agent's account, with receiver the guard and the single method `request_payment`. The testnet deploy adds one with a gas allowance of 1 NEAR (`addFunctionCallAccessKey` in [`client/deploy/testnet.ts`](../client/deploy/testnet.ts)). Keep the account's full-access key away from the agent. The agent's account needs no contract.

### The tool

`pay(token, receiver_id, amount, msg?, expires_in_s?)`, from `PAY_TOOL` in [`client/src/mcp.ts`](../client/src/mcp.ts):

| Input | Type | Meaning |
| --- | --- | --- |
| `token` | `"native"` or `{"ft": "<token contract>"}` | what to pay in |
| `receiver_id` | string | the NEAR account to pay |
| `amount` | string | in whole tokens, as the invoice writes it, such as `"5.25"`; converted with the token's `ft_metadata.decimals` |
| `msg` | string, optional | pays through `ft_transfer_call` with this message; FT only |
| `expires_in_s` | integer, optional, 1 to 604800, default 600 | how long a human has to approve, if it comes to that |

Any other input field is refused. The server speaks MCP protocol versions `2025-11-25`, `2025-06-18`, `2025-03-26` and `2024-11-05`, and answers `initialize`, `ping`, `tools/list` and `tools/call`. Calls run side by side; the near-api-js sender sends one transaction at a time, so nonces arrive in order.

`pay` attaches gas by method, from [`client/src/gas.json`](../client/src/gas.json): 30 TGas for native, 40 for `ft_transfer`, 100 for `ft_transfer_call`.

### The answers

Each answer is plain text, and its first line starts with the status. The answers that carry the guard's own result (`paid`, `refused`, `pending`, `failed`, `uncertain`) end with a line `transaction <hash>`; `unknown` gives the hash inside its text when it has one.

| Status | First line | MCP `isError` |
| --- | --- | --- |
| `paid` | `paid: 5 TST to api.nsg2-demo.testnet (request 4)`; after a partial `ft_transfer_call` refund, `paid: <sent> of <amount> to <receiver>; the receiver sent the rest back (request <id>)` | false |
| `refused` | `refused (receiver_not_allowed): nothing was sent or reserved (request 2)` | false |
| `pending` | `pending (above_threshold): request 3 waits for a human`, then the payment in full (below) | false |
| `failed` | `failed: nothing moved, <amount> of budget came back (request <id>)` | true |
| `uncertain` | `uncertain: the transfer of <amount> to <receiver> reported a failure after the tokens may have moved (request <id>)`, then `Do not pay again before checking the receiver's balance.` | true |
| `rejected` | `rejected, nothing changed: <error>`: the transaction was never sent, or the guard panicked on the request itself (malformed input, wrong caller) | true |
| `unknown` | `unknown: <why>. The payment may still go through. Transaction <hash>.`, then `Do not pay again before checking.` | true |

Input that `pay` refuses before sending anything comes back as `not sent: <reason>`, with `isError` true.

A `pending` answer prints what the approver will see and the command they run. This is request 3 on testnet ([Demo](demo.md#on-testnet)), with the final line's hash left as a placeholder:

```
pending (above_threshold): request 3 waits for a human
amount   60 TST (60000000 base units)
receiver api.nsg2-demo.testnet
expires  2026-11-11T02:49:51.053Z
hash     5d60f001aeb93966e92bd001e6e23596c1e8fcec84d17bd4fed8f48d28fe2080
payload  {"request_id":"3","token":{"ft":"tst.nsg2-demo.testnet"},"receiver_id":"api.nsg2-demo.testnet","amount":"60000000","msg":null,"expires_at":"1794365391053650914"}
The approver reads it from the guard with: nsg-approve 3
transaction <hash>
```

`pay` recomputes the payload hash itself. If it differs from the guard's, it adds `WARNING: the guard's hash is not this payload's (<hash>). Do not approve.`

### `unknown` and retries

`pay` is not idempotent: two calls are two payments, bounded by the caps. Once a transaction has been sent, `pay` never says nothing happened unless the first receipt, `request_payment` itself, failed. A timeout (30 seconds for inclusion), an RPC error, or a failure in a later receipt is `unknown`, with the transaction hash. The agent must check the transaction before paying again. Tests: `giving up never reads as 'nothing happened'` and `a failure after the first receipt doesn't say nothing moved` in [`client/test/pay.test.ts`](../client/test/pay.test.ts), and [`a_second_process_on_the_key_costs_one_answer`](../sandbox-tests/tests/client.rs).

## The approver: `nsg-approve`

[`client/bin/nsg-approve.ts`](../client/bin/nsg-approve.ts) runs on the approver's own machine. It holds no key and signs nothing.

### Config

It reads `$XDG_CONFIG_HOME/near-spend-guard/approver.json`, or `~/.config/near-spend-guard/approver.json`:

```json
{"guard_id": "nsg2-guard.testnet", "approver_id": "nsg2-approver.testnet", "network": "testnet-fastnear", "rpc_url": "https://..."}
```

| Field | Meaning |
| --- | --- |
| `guard_id` | the guard to read from and approve on |
| `approver_id` | the account that signs |
| `network` | near CLI's name for the connection; a name containing `mainnet` is refused |
| `rpc_url` | the RPC it reads the request from |
| `near_cli` | optional: the near CLI binary, if `near` on `PATH` isn't it |

### What it does

`nsg-approve <request_id>` takes exactly one argument, a request number. There is no way to pass a guard, an amount or a payload, so nothing a compromised agent prints can point it at a look-alike contract. Then it:

1. Refuses mainnet.
2. Checks that the guard's approver is `approver_id`, and that the guard isn't paused.
3. Reads the request from the guard with `get_request`, and checks that it exists and hasn't expired.
4. Prints it:

```
Guard     <guard_id> (from your config)
Request   <id> (<reason>)
Pay       <amount> <symbol>
          <base units> in the token's smallest unit
To        <receiver>
Token     <token contract, or NEAR>
Method    <native | ft_transfer | ft_transfer_call>
Message   <msg, JSON-escaped, or none>
Expires   <ISO time>
Hash      <payload hash>

near CLI shows the same payment next and asks how to sign. Nothing is signed before that.
```

5. Starts near CLI, with no shell, as `near contract call-function as-transaction <guard> approve json-args '{"payload": ...}' prepaid-gas '100 Tgas' attached-deposit '1 yoctoNEAR' sign-as <approver> network-config <network>`. No signing option is given, so near CLI shows the unsigned transaction with its arguments in full and asks how to sign.

`msg` is printed JSON-escaped, with a warning if it isn't plain ASCII, so control characters or right-to-left overrides can't hide part of it. Tests: [`nsg_approve_ignores_agent_output`](../client/test/approve.test.ts) and `a message can't hide in the terminal` in [`client/test/approve.test.ts`](../client/test/approve.test.ts).

On a local node there is no near CLI. `scripts/local-node.sh` writes a stand-in that asks before signing, as near CLI does, and the node signs only the exact arguments `nsg-approve` builds.

### Kill switch

`pause` needs no deposit, so the approver can keep a function-call key limited to `pause` on the guard for a quick stop ([`pause_from_limited_key`](../sandbox-tests/tests/wrapper.rs)). Only the owner can `unpause`, with 1 yoctoNEAR.

## Running a real agent

[`scripts/local-agent.sh`](../scripts/local-agent.sh) runs Claude Code as the MCP host with `pay` as its only tool: no built-in tools (no shell, files or web), no MCP server but ours, and `mcp__near-spend-guard__pay` the only tool allowed without asking. It runs from a temporary directory, so no project settings come along, and prints the host's stream-json events.

Against a local node, with no testnet:

```bash
scripts/local-node.sh /tmp/nsg-local
scripts/local-agent.sh /tmp/nsg-local "Pay the 5 TST invoice for 1,000 API calls: 5 TST to api.test.near, TST is the token at tst.test.near."
XDG_CONFIG_HOME=/tmp/nsg-local/config node client/bin/nsg-approve.ts <request_id>
```

Against the testnet world that `scripts/deploy-testnet.sh` wrote ([Deploy](deploy.md)):

```bash
scripts/local-agent.sh ~/.config/near-spend-guard/testnet "<prompt>"
XDG_CONFIG_HOME=~/.config/near-spend-guard/testnet/config node client/bin/nsg-approve.ts <request_id>
```

Any other MCP host can use the same `mcp.json`.
