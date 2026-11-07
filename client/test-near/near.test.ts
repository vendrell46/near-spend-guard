// `near.ts` against a stand-in RPC, for what a real node won't do on request:
// hang, or fail. Kept apart from test/ because it loads near-api-js, which
// `npm test` doesn't need.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { type Server, createServer } from "node:http";
import { test } from "node:test";

import { KeyPair, baseEncode } from "near-api-js";

import { nearSender } from "../src/near.ts";
import type { RequestArgs } from "../src/pay.ts";

const key = KeyPair.fromRandom("ed25519");
const guardId = "guard.testnet";
const args: RequestArgs = {
  token: { ft: "tst.testnet" },
  receiver_id: "api.testnet",
  amount: "5000000",
  msg: null,
  expires_at: "1762300800000000000",
};

type Handler = (method: string, params: any) => unknown | Promise<unknown>;
const never = (): Promise<unknown> => new Promise(() => {});

const accessKey = {
  nonce: 41,
  permission: { FunctionCall: { allowance: "1", receiver_id: guardId, method_names: ["request_payment"] } },
  block_hash: "11111111111111111111111111111111",
  block_height: 1,
};
let nonce = 41;
const block = { header: { hash: baseEncode(new Uint8Array(32).fill(7)), height: 1 } };

/** Answers access-key and block reads, and hands the rest to `handler`. */
async function rpc(
  handler: Handler,
  reads: { block?: Handler } = {},
): Promise<{ url: string; server: Server }> {
  const server = createServer((req, res) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", async () => {
      const { id, method, params } = JSON.parse(body);
      const answer =
        method === "query"
          ? { result: { ...accessKey, nonce: nonce } }
          : method === "block"
            ? await (reads.block ?? (() => ({ result: block })))(method, params)
            : await handler(method, params);
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ jsonrpc: "2.0", id, ...(answer as object) }));
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as { port: number };
  return { url: `http://127.0.0.1:${port}`, server };
}

function sender(url: string, timeoutMs = 2000) {
  return nearSender({
    guardId,
    agentId: "agent.testnet",
    rpcUrl: url,
    secretKey: key.toString(),
    timeoutMs,
  });
}

test("it signs one call: request_payment on the guard, with no deposit", async () => {
  const seen: any[] = [];
  const { url, server } = await rpc((method, params) => {
    seen.push([method, params]);
    return {
      result: {
        status: { SuccessValue: Buffer.from('{"status":"paid"}').toString("base64") },
        transaction_outcome: { outcome: { receipt_ids: ["r"] } },
        receipts_outcome: [],
      },
    };
  });
  try {
    const out = await sender(url).sender.requestPayment(args, 40);
    assert.equal(out.kind, "final");
    assert.deepEqual((out as { value: unknown }).value, { status: "paid" });
    assert.equal(seen.length, 1);
    const [method, params] = seen[0];
    assert.equal(method, "send_tx");
    assert.equal(params.wait_until, "EXECUTED_OPTIMISTIC");

    const { SignedTransaction } = await import("near-api-js");
    const tx = SignedTransaction.decode(Buffer.from(params.signed_tx_base64, "base64")).transaction;
    assert.equal(tx.signerId, "agent.testnet");
    assert.equal(tx.receiverId, guardId);
    assert.equal(tx.nonce, 42n);
    assert.equal(tx.actions.length, 1);
    const call = tx.actions[0].functionCall!;
    assert.equal(call.methodName, "request_payment");
    assert.equal(call.deposit, 0n);
    assert.equal(call.gas, 40_000_000_000_000n);
    assert.deepEqual(JSON.parse(Buffer.from(call.args).toString()), args);
    // nearcore's id for a transaction: what explorers are searched by.
    const hash = baseEncode(createHash("sha256").update(tx.encode()).digest());
    assert.equal((out as { tx_hash: string }).tx_hash, hash);
  } finally {
    server.closeAllConnections();
    server.close();
  }
});

test("a transaction that isn't answered in time is unknown, with its hash", async () => {
  const { url, server } = await rpc(never);
  try {
    const out = await sender(url, 100).sender.requestPayment(args, 40);
    assert.equal(out.kind, "unknown");
    assert.match((out as { why: string }).why, /no answer within 0\.1 s/);
    assert.match(String(out.tx_hash), /^[1-9A-HJ-NP-Za-km-z]{43,44}$/);
  } finally {
    server.closeAllConnections();
    server.close();
  }
});

test("a chain that won't answer before signing means nothing was sent", async () => {
  const seen: string[] = [];
  const { url, server } = await rpc(
    (method) => {
      seen.push(method);
      return {};
    },
    { block: never },
  );
  try {
    const out = await sender(url, 100).sender.requestPayment(args, 40);
    assert.deepEqual(out, { kind: "rejected", error: "not sent: no answer within 0.1 s", tx_hash: null });
    assert.deepEqual(seen, []);
  } finally {
    server.closeAllConnections();
    server.close();
  }
});

test("a key whose nonce moved on is read again, and that payment stays unknown", async () => {
  const nonces: bigint[] = [];
  const { SignedTransaction } = await import("near-api-js");
  const { url, server } = await rpc((_method, params) => {
    const signed = SignedTransaction.decode(Buffer.from(params.signed_tx_base64, "base64"));
    nonces.push(signed.transaction.nonce);
    return {
      error: {
        name: "HANDLER_ERROR",
        code: -32000,
        message: "Server error",
        data: { TxExecutionError: { InvalidTxError: { InvalidNonce: { ak_nonce: 50, tx_nonce: 42 } } } },
        cause: {
          name: "INVALID_TRANSACTION",
          info: { InvalidNonce: { ak_nonce: 50, tx_nonce: 42 } },
        },
      },
    };
  });
  try {
    const { sender: s } = sender(url);
    nonce = 41;
    const first = await s.requestPayment(args, 40);
    assert.equal(first.kind, "unknown");
    nonce = 50;
    await s.requestPayment(args, 40);
    assert.deepEqual(nonces, [42n, 51n]);
  } finally {
    nonce = 41;
    server.closeAllConnections();
    server.close();
  }
});

test("transactions go one at a time: a later nonce could kill an earlier one", async () => {
  const order: string[] = [];
  let release = (_: unknown) => {};
  const held = new Promise((resolve) => {
    release = resolve;
  });
  const paid = {
    result: {
      status: { SuccessValue: Buffer.from("{}").toString("base64") },
      transaction_outcome: { outcome: { receipt_ids: ["r"] } },
      receipts_outcome: [],
    },
  };
  let calls = 0;
  const { url, server } = await rpc(async () => {
    const call = ++calls;
    order.push(`start ${call}`);
    if (call === 1) {
      await held;
    }
    order.push(`end ${call}`);
    return paid;
  });
  try {
    const { sender: s } = sender(url);
    const first = s.requestPayment(args, 40);
    const second = s.requestPayment(args, 40);
    await new Promise((resolve) => setTimeout(resolve, 100));
    assert.deepEqual(order, ["start 1"]);
    release(null);
    assert.equal((await first).kind, "final");
    assert.equal((await second).kind, "final");
    assert.deepEqual(order, ["start 1", "end 1", "start 2", "end 2"]);
  } finally {
    server.closeAllConnections();
    server.close();
  }
});

test("a payment that fails doesn't hold up the next", async () => {
  const { url, server } = await rpc(() => ({
    error: { name: "INTERNAL_ERROR", code: -32000, message: "x", cause: { name: "INTERNAL_ERROR", info: {} } },
  }));
  try {
    const { sender: s } = sender(url);
    const outs = await Promise.all([s.requestPayment(args, 40), s.requestPayment(args, 40)]);
    assert.deepEqual(outs.map((o) => o.kind), ["unknown", "unknown"]);
  } finally {
    server.closeAllConnections();
    server.close();
  }
});

test("an RPC error after sending can't prove nothing happened", async () => {
  const { url, server } = await rpc(() => ({
    error: {
      name: "HANDLER_ERROR",
      code: -32000,
      message: "Server error",
      data: "whatever",
      cause: { name: "INTERNAL_ERROR", info: { error_message: "node on fire" } },
    },
  }));
  try {
    const out = await sender(url).sender.requestPayment(args, 40);
    assert.equal(out.kind, "unknown");
    assert.notEqual(out.tx_hash, null);
  } finally {
    server.closeAllConnections();
    server.close();
  }
});

test("failing before the broadcast means nothing was sent", async () => {
  const out = await sender("http://127.0.0.1:1").sender.requestPayment(args, 40);
  assert.equal(out.kind, "rejected");
  assert.equal(out.tx_hash, null);
  assert.match((out as { error: string }).error, /^not sent: /);
});

test("the public key is the signer's", async () => {
  assert.equal(await sender("http://127.0.0.1:1").publicKey(), key.getPublicKey().toString());
});
