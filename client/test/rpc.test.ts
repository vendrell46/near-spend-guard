import assert from "node:assert/strict";
import { test } from "node:test";

import { jsonRpc, refuseMainnet } from "../src/rpc.ts";

const url = "https://rpc.example/?apikey=SECRET";

function answering(body: unknown, status = 200): { fetch: typeof fetch; sent: { body: any }[] } {
  const sent: { body: any }[] = [];
  const fake = (async (_url: unknown, init: RequestInit) => {
    sent.push({ body: JSON.parse(init.body as string) });
    return new Response(JSON.stringify(body), { status });
  }) as typeof fetch;
  return { fetch: fake, sent };
}

const bytes = (value: unknown): number[] => [...Buffer.from(JSON.stringify(value))];

test("a view sends base64 JSON and reads bytes of JSON", async () => {
  const { fetch, sent } = answering({ result: { result: bytes({ hello: "world" }), logs: [] } });
  const value = await jsonRpc(url, { fetch }).view("guard.testnet", "get_request", { request_id: "7" });
  assert.deepEqual(value, { hello: "world" });
  const { method, params } = sent[0].body;
  assert.equal(method, "query");
  assert.equal(params.request_type, "call_function");
  assert.equal(params.account_id, "guard.testnet");
  assert.equal(params.method_name, "get_request");
  assert.equal(params.finality, "optimistic");
  assert.deepEqual(JSON.parse(Buffer.from(params.args_base64, "base64").toString()), { request_id: "7" });
});

test("a view that panics is an error", async () => {
  const { fetch } = answering({ result: { error: "wasm execution failed\x1b[2K", logs: [] } });
  await assert.rejects(jsonRpc(url, { fetch }).view("g.testnet", "m", {}), (e: Error) => {
    assert.match(e.message, /view failed: "wasm execution failed\\u001b\[2K"/);
    return true;
  });
});

test("a query with no bytes is an error", async () => {
  const { fetch } = answering({ result: { logs: [] } });
  await assert.rejects(jsonRpc(url, { fetch }).view("g.testnet", "m", {}), /no view result/);
});

test("an RPC error is an error, and the URL stays out of it", async () => {
  const refused = answering({ error: { name: "HANDLER_ERROR", cause: { name: "UNKNOWN_ACCOUNT" } } });
  await assert.rejects(jsonRpc(url, { fetch: refused.fetch }).view("g.testnet", "m", {}), (e: Error) => {
    assert.match(e.message, /UNKNOWN_ACCOUNT/);
    assert.doesNotMatch(e.message, /SECRET/);
    return true;
  });
  const down = answering("bad gateway", 502);
  await assert.rejects(jsonRpc(url, { fetch: down.fetch }).blockTimeNs(), (e: Error) => {
    assert.equal(e.message, "the RPC answered 502");
    return true;
  });
});

test("an answer without a result is an error", async () => {
  for (const body of [{}, { result: null }, null, 7]) {
    await assert.rejects(jsonRpc(url, { fetch: answering(body).fetch }).blockTimeNs(), /RPC sent no/);
  }
});

test("block time is the latest final block's", async () => {
  const { fetch, sent } = answering({ result: { header: { timestamp_nanosec: "1762300800000000000" } } });
  assert.equal(await jsonRpc(url, { fetch }).blockTimeNs(), "1762300800000000000");
  assert.deepEqual(sent[0].body.params, { finality: "final" });
  assert.equal(sent[0].body.method, "block");
});

test("a block with no usable time is an error", async () => {
  for (const header of [{}, { timestamp_nanosec: 5 }, { timestamp_nanosec: "soon" }]) {
    const { fetch } = answering({ result: { header } });
    await assert.rejects(jsonRpc(url, { fetch }).blockTimeNs(), /no block time/);
  }
});

test("view_access_key is asked for the account and key", async () => {
  const { fetch, sent } = answering({ result: { nonce: 1, permission: "FullAccess" } });
  const key = await jsonRpc(url, { fetch }).accessKey("agent.testnet", "ed25519:abc");
  assert.deepEqual(key, { nonce: 1, permission: "FullAccess" });
  const { params } = sent[0].body;
  assert.deepEqual(
    [params.request_type, params.account_id, params.public_key],
    ["view_access_key", "agent.testnet", "ed25519:abc"],
  );
});

test("a missing key is an error", async () => {
  const { fetch } = answering({ result: { error: "access key ed25519:abc does not exist" } });
  await assert.rejects(jsonRpc(url, { fetch }).accessKey("agent.testnet", "ed25519:abc"), /does not exist/);
});

test("an RPC that never answers times out", async () => {
  const hanging = ((_url: unknown, init: RequestInit) =>
    new Promise((_resolve, reject) => {
      init.signal?.addEventListener("abort", () => reject(init.signal?.reason));
    })) as typeof fetch;
  // AbortSignal.timeout's timer doesn't keep the process alive; a real socket does.
  const alive = setTimeout(() => {}, 1000);
  await assert.rejects(jsonRpc(url, { fetch: hanging, timeoutMs: 20 }).blockTimeNs(), /timeout|aborted/i);
  clearTimeout(alive);
});

test("mainnet is refused, and everything else isn't", async () => {
  const chain = (chain_id: unknown) => jsonRpc(url, { fetch: answering({ result: { chain_id } }).fetch });
  await assert.rejects(refuseMainnet(chain("mainnet")), /testnet only/);
  await refuseMainnet(chain("testnet"));
  await refuseMainnet(chain("test-chain-abc"));
  await assert.rejects(refuseMainnet(chain(7)), /no chain id/);
});
