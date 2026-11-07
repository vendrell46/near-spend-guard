import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import { test } from "node:test";

import { PAY_TOOL, PROTOCOL_VERSIONS, serve } from "../src/mcp.ts";
import type { PayReport } from "../src/pay.ts";

const paid: PayReport = { status: "paid", text: "paid: 5 TST", request_id: "7", tx_hash: "TX" };

/** Sends `lines`, ends the input and returns every answer in order. */
async function session(lines: unknown[], run: (args: unknown) => Promise<PayReport> = async () => paid) {
  const input = new PassThrough();
  const output = new PassThrough();
  const done = serve(input, output, run);
  for (const line of lines) {
    input.write(typeof line === "string" ? `${line}\n` : `${JSON.stringify(line)}\n`);
  }
  input.end();
  await done;
  output.end();
  const text: string = (await output.toArray()).join("");
  return text.split("\n").filter(Boolean).map((l) => JSON.parse(l));
}

const call = (id: unknown, args: unknown = {}, name = "pay") => ({
  jsonrpc: "2.0",
  id,
  method: "tools/call",
  params: { name, arguments: args },
});

test("initialize takes a version we speak, and offers tools", async () => {
  for (const version of PROTOCOL_VERSIONS) {
    const [reply] = await session([{ id: 1, method: "initialize", params: { protocolVersion: version } }]);
    assert.equal(reply.id, 1);
    assert.equal(reply.jsonrpc, "2.0");
    assert.equal(reply.result.protocolVersion, version);
    assert.deepEqual(reply.result.capabilities, { tools: {} });
    assert.equal(reply.result.serverInfo.name, "near-spend-guard");
  }
});

test("another version gets the newest we speak", async () => {
  const [reply] = await session([{ id: 1, method: "initialize", params: { protocolVersion: "1999-01-01" } }]);
  assert.equal(reply.result.protocolVersion, PROTOCOL_VERSIONS[0]);
});

test("the one tool is pay", async () => {
  const [reply] = await session([{ id: 1, method: "tools/list" }]);
  assert.deepEqual(reply.result.tools, [PAY_TOOL]);
  assert.deepEqual(PAY_TOOL.inputSchema.required, ["token", "receiver_id", "amount"]);
  assert.equal(PAY_TOOL.inputSchema.additionalProperties, false);
});

test("ping answers", async () => {
  const [reply] = await session([{ id: "p", method: "ping" }]);
  assert.deepEqual(reply, { jsonrpc: "2.0", id: "p", result: {} });
});

test("a call reaches pay with its arguments", async () => {
  const seen: unknown[] = [];
  const [reply] = await session([call(3, { amount: "5" })], async (args) => {
    seen.push(args);
    return paid;
  });
  assert.deepEqual(seen, [{ amount: "5" }]);
  assert.equal(reply.id, 3);
  assert.deepEqual(reply.result, { content: [{ type: "text", text: "paid: 5 TST" }], isError: false });
});

test("a call without arguments gets an empty object", async () => {
  const seen: unknown[] = [];
  await session([{ id: 1, method: "tools/call", params: { name: "pay" } }], async (args) => {
    seen.push(args);
    return paid;
  });
  assert.deepEqual(seen, [{}]);
});

test("a held or refused payment isn't a tool error; the rest are", async () => {
  const statuses: [PayReport["status"], boolean][] = [
    ["paid", false],
    ["pending", false],
    ["refused", false],
    ["failed", true],
    ["uncertain", true],
    ["rejected", true],
    ["unknown", true],
  ];
  for (const [status, isError] of statuses) {
    const [reply] = await session([call(1)], async () => ({ ...paid, status }));
    assert.equal(reply.result.isError, isError, status);
  }
});

test("input pay refuses comes back as not sent", async () => {
  const [reply] = await session([call(1)], async () => {
    throw new Error("amount must be above 0");
  });
  assert.equal(reply.result.isError, true);
  assert.equal(reply.result.content[0].text, "not sent: amount must be above 0");
});

test("an error's text is escaped on its way to the model", async () => {
  const [reply] = await session([call(1)], async () => {
    throw new Error("bad\x1b[2K");
  });
  assert.equal(reply.result.content[0].text, "not sent: bad\\u001b[2K");
});

test("there is no other tool", async () => {
  const [reply] = await session([call(1, {}, "approve")], async () => {
    throw new Error("must not run");
  });
  assert.equal(reply.error.code, -32602);
  assert.match(reply.error.message, /unknown tool: approve/);
});

test("an unknown method is an error", async () => {
  const [reply] = await session([{ id: 1, method: "resources/list" }]);
  assert.equal(reply.error.code, -32601);
});

test("notifications get no answer", async () => {
  const replies = await session([
    { method: "notifications/initialized" },
    { method: "notifications/cancelled", params: { requestId: 1 } },
    { method: "tools/call", params: { name: "pay", arguments: {} } },
  ], async () => {
    throw new Error("a notification must not pay");
  });
  assert.deepEqual(replies, []);
});

test("bad lines don't stop the server", async () => {
  const replies = await session(["not json", "", "   ", { id: 9, result: {} }, null, { id: 2, method: "ping" }]);
  assert.equal(replies.length, 2);
  assert.equal(replies[0].error.code, -32700);
  assert.equal(replies[0].id, null);
  assert.deepEqual(replies[1].result, {});
});

test("calls run side by side and are answered by id", async () => {
  let release = () => {};
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const replies = await session([call(1, { slow: true }), call(2, {})], async (args) => {
    if ((args as { slow?: boolean }).slow) {
      await held;
      return { ...paid, text: "slow" };
    }
    release();
    return { ...paid, text: "fast" };
  });
  assert.deepEqual(replies.map((r) => [r.id, r.result.content[0].text]), [
    [2, "fast"],
    [1, "slow"],
  ]);
});

test("the end of input waits for calls still running", async () => {
  const replies = await session([call(1)], async () => {
    await new Promise((resolve) => setTimeout(resolve, 20));
    return paid;
  });
  assert.equal(replies.length, 1);
});
