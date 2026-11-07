import assert from "node:assert/strict";
import { test } from "node:test";

import {
  APPROVE_TGAS,
  agentGas,
  guardConfig,
  isoTime,
  methodOf,
  parseConfig,
  parseOutcome,
  parsePending,
  pendingRequest,
  tokenInfo,
} from "../src/guard.ts";
import { type PaymentPayload, payloadHash } from "../src/payload.ts";
import type { Rpc } from "../src/rpc.ts";

const guardId = "guard.testnet";
const hash = "ab".repeat(32);
const payload: PaymentPayload = {
  request_id: "7",
  token: { ft: "tst.testnet" },
  receiver_id: "alice.testnet",
  amount: "50000000",
  msg: null,
  expires_at: "1762300800000000000",
};
const view = {
  payload,
  payload_hash: payloadHash(guardId, payload),
  reason: "above_threshold",
  created_at: "1762300000000000000",
};

function rpcWith(view: (contract: string, method: string, args: object) => unknown): Rpc {
  return {
    view: async (contract, method, args) => view(contract, method, args),
    blockTimeNs: async () => "0",
    chainId: async () => "testnet",
    accessKey: async () => null,
  };
}

test("every outcome the guard returns is read", () => {
  const outcomes = [
    { status: "refused", request_id: "1", reason: "paused" },
    { status: "refused", request_id: "1", reason: "token_not_configured" },
    { status: "refused", request_id: "1", reason: "receiver_not_allowed" },
    { status: "refused", request_id: "1", reason: "transfer_call_not_allowed" },
    { status: "refused", request_id: "1", reason: "above_per_payment_max" },
    { status: "refused", request_id: "1", reason: "too_many_pending" },
    { status: "refused", request_id: "1", reason: "insufficient_balance" },
    { status: "pending", request_id: "2", reason: "above_threshold", payload_hash: hash },
    { status: "pending", request_id: "2", reason: "daily_cap", payload_hash: hash },
    { status: "paid", request_id: "3", sent: "5" },
    { status: "failed", request_id: "4", released: "0" },
    { status: "uncertain", request_id: "5" },
    { status: "expired", request_id: "6" },
  ];
  for (const outcome of outcomes) {
    assert.deepEqual(parseOutcome(outcome), outcome);
  }
});

test("an outcome this code doesn't know is an error, not a guess", () => {
  const bad: [unknown, RegExp][] = [
    [null, /not an object/],
    ["paid", /not an object/],
    [[], /not an object/],
    [{ status: "paid", sent: "5" }, /request_id/],
    [{ status: "paid", request_id: 3, sent: "5" }, /request_id/],
    [{ status: "paid", request_id: "3" }, /sent/],
    [{ status: "paid", request_id: "3", sent: 5 }, /sent/],
    [{ status: "paid", request_id: "3", sent: "05" }, /sent/],
    [{ status: "failed", request_id: "3" }, /released/],
    [{ status: "refused", request_id: "3", reason: "because" }, /unknown reason/],
    [{ status: "pending", request_id: "3", reason: "paused", payload_hash: hash }, /unknown reason/],
    [{ status: "pending", request_id: "3", reason: "daily_cap", payload_hash: "AB" }, /64 hex/],
    [{ status: "pending", request_id: "3", reason: "daily_cap", payload_hash: hash.toUpperCase() }, /64 hex/],
    [{ status: "settled", request_id: "3" }, /unknown status/],
  ];
  for (const [value, message] of bad) {
    assert.throws(() => parseOutcome(value), message, JSON.stringify(value));
  }
});

test("a pending request is read whole", () => {
  assert.deepEqual(parsePending(guardId, view), view);
  const native = { ...payload, token: "native" as const, msg: null };
  const v = { ...view, payload: native, payload_hash: payloadHash(guardId, native) };
  assert.deepEqual(parsePending(guardId, v).payload.token, "native");
});

test("a hash that isn't this payload's stops the read", () => {
  const swapped = { ...view, payload: { ...payload, amount: "190000000" } };
  assert.throws(() => parsePending(guardId, swapped), /not this payload's/);
  assert.throws(() => parsePending("other.testnet", view), /not this payload's/);
});

test("a malformed pending request is refused", () => {
  const bad: [object, RegExp][] = [
    [{ ...view, payload: null }, /not an object/],
    [{ ...view, payload: { ...payload, receiver_id: 7 } }, /receiver_id/],
    [{ ...view, payload: { ...payload, msg: 7 } }, /msg/],
    [{ ...view, payload: { ...payload, amount: 7 } }, /amount/],
    [{ ...view, payload: { ...payload, expires_at: 7 } }, /expires_at/],
    [{ ...view, payload: { ...payload, request_id: "07" } }, /request_id/],
    [{ ...view, payload: { ...payload, token: { ft: 7 } } }, /token/],
    [{ ...view, payload: { ...payload, token: "near" } }, /not an object/],
    [{ ...view, reason: "paused" }, /unknown reason/],
    [{ ...view, created_at: 7 }, /created_at/],
    [{ ...view, payload_hash: "ab" }, /64 hex/],
  ];
  for (const [value, message] of bad) {
    assert.throws(() => parsePending(guardId, value), message, JSON.stringify(value));
  }
});

test("the config needs every field the client relies on", () => {
  const config = {
    owner_id: "o.testnet",
    agent_id: "a.testnet",
    approver_id: "h.testnet",
    paused: false,
    version: "1.0.0",
  };
  assert.deepEqual(parseConfig(config), config);
  for (const field of ["owner_id", "agent_id", "approver_id", "version", "paused"]) {
    const { [field as "paused"]: _, ...rest } = config;
    assert.throws(() => parseConfig(rest), new RegExp(field === "paused" ? "paused" : field));
  }
  assert.throws(() => parseConfig({ ...config, paused: "no" }), /paused/);
  assert.throws(() => parseConfig({ ...config, approver_id: "\x1b[2Jhuman" }), /not a NEAR account id/);
  assert.throws(() => parseConfig(null), /not an object/);
});

test("the method follows the token and msg", () => {
  assert.equal(methodOf("native", null), "native");
  assert.equal(methodOf({ ft: "t.testnet" }, null), "ft_transfer");
  assert.equal(methodOf({ ft: "t.testnet" }, ""), "ft_transfer_call");
  assert.equal(methodOf({ ft: "t.testnet" }, "go"), "ft_transfer_call");
});

test("gas by method is decision 24's", () => {
  assert.equal(agentGas("native"), 30);
  assert.equal(agentGas("ft_transfer"), 40);
  assert.equal(agentGas("ft_transfer_call"), 100);
  assert.equal(APPROVE_TGAS, 100);
});

test("NEAR needs no call; an FT says how it writes amounts", async () => {
  const rpc = rpcWith((contract, method) => {
    assert.deepEqual([contract, method], ["tst.testnet", "ft_metadata"]);
    return { spec: "ft-1.0.0", symbol: "TST", decimals: 6 };
  });
  assert.deepEqual(await tokenInfo(rpc, "native"), { symbol: "NEAR", decimals: 24 });
  assert.deepEqual(await tokenInfo(rpc, { ft: "tst.testnet" }), { symbol: "TST", decimals: 6 });
});

test("a token can't fill the screen with its symbol", async () => {
  const rpc = rpcWith(() => ({ symbol: "T".repeat(1000), decimals: 6 }));
  assert.equal((await tokenInfo(rpc, { ft: "tst.testnet" })).symbol.length, 32);
});

test("metadata without decimals is refused", async () => {
  for (const meta of [{ symbol: "TST" }, { decimals: 6 }, { symbol: "TST", decimals: "6" }, { symbol: "TST", decimals: 1.5 }, null]) {
    await assert.rejects(tokenInfo(rpcWith(() => meta), { ft: "tst.testnet" }));
  }
});

test("get_request is asked by id, and null means none", async () => {
  const asked: unknown[] = [];
  const rpc = rpcWith((contract, method, args) => {
    asked.push([contract, method, args]);
    return (args as { request_id: string }).request_id === "7" ? view : null;
  });
  assert.deepEqual(await pendingRequest(rpc, guardId, "7"), view);
  assert.equal(await pendingRequest(rpc, guardId, "8"), null);
  assert.deepEqual(asked[0], [guardId, "get_request", { request_id: "7" }]);
});

test("the guard's config comes from the guard", async () => {
  const rpc = rpcWith((contract, method) => {
    assert.deepEqual([contract, method], [guardId, "get_config"]);
    return { owner_id: "o.t", agent_id: "a.t", approver_id: "h.t", paused: true, version: "1.0.0" };
  });
  assert.equal((await guardConfig(rpc, guardId)).paused, true);
});

test("nanoseconds become a date", () => {
  assert.equal(isoTime("1762300800000000000"), "2025-11-05T00:00:00.000Z");
  assert.equal(isoTime("18446744073709551615"), "2554-07-21T23:34:33.709Z");
});
