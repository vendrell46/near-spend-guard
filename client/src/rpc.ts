// Reading the chain over NEAR's JSON-RPC, with Node's own fetch. The approver's
// helper runs on this alone, so nothing from npm sits between the approver and
// what they're about to sign.

import { printable } from "./display.ts";

export interface Rpc {
  view(contractId: string, method: string, args: object): Promise<unknown>;
  /** The latest final block's time, in the guard's units. */
  blockTimeNs(): Promise<string>;
  chainId(): Promise<string>;
  accessKey(accountId: string, publicKey: string): Promise<unknown>;
}

export interface RpcOptions {
  timeoutMs?: number;
  fetch?: typeof globalThis.fetch;
}

function record(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null) {
    throw new Error(`the RPC sent no ${what}`);
  }
  return value as Record<string, unknown>;
}

export function jsonRpc(url: string, options: RpcOptions = {}): Rpc {
  const { timeoutMs = 10_000, fetch = globalThis.fetch } = options;

  async function call(method: string, params: unknown): Promise<Record<string, unknown>> {
    const response = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: "nsg", method, params }),
      signal: AbortSignal.timeout(timeoutMs),
    });
    // Not the URL: a provider's key can sit in it.
    if (!response.ok) {
      throw new Error(`the RPC answered ${response.status}`);
    }
    const body = record(await response.json(), "answer");
    if (body.error !== undefined) {
      throw new Error(`the RPC refused: ${printable(JSON.stringify(body.error)).slice(0, 300)}`);
    }
    return record(body.result, "result");
  }

  async function query(params: object): Promise<Record<string, unknown>> {
    const result = await call("query", { finality: "optimistic", ...params });
    // A view that panics comes back as a result with `error` in it.
    if (typeof result.error === "string") {
      throw new Error(`the view failed: ${printable(result.error).slice(0, 300)}`);
    }
    return result;
  }

  return {
    async view(contractId, method, args) {
      const result = await query({
        request_type: "call_function",
        account_id: contractId,
        method_name: method,
        args_base64: Buffer.from(JSON.stringify(args)).toString("base64"),
      });
      if (!Array.isArray(result.result)) {
        throw new Error("the RPC sent no view result");
      }
      return JSON.parse(Buffer.from(result.result as number[]).toString("utf8"));
    },

    async blockTimeNs() {
      const block = await call("block", { finality: "final" });
      const time = record(block.header, "block header").timestamp_nanosec;
      if (typeof time !== "string" || !/^\d+$/.test(time)) {
        throw new Error("the RPC sent no block time");
      }
      return time;
    },

    async chainId() {
      const status = await call("status", []);
      if (typeof status.chain_id !== "string") {
        throw new Error("the RPC sent no chain id");
      }
      return status.chain_id;
    },

    async accessKey(accountId, publicKey) {
      return query({
        request_type: "view_access_key",
        account_id: accountId,
        public_key: publicKey,
      });
    },
  };
}

/** Testnet only: this code hasn't had an audit, and mainnet isn't ours to try
 * it on. */
export async function refuseMainnet(rpc: Rpc): Promise<void> {
  if ((await rpc.chainId()) === "mainnet") {
    throw new Error("near-spend-guard's client runs on testnet only");
  }
}
