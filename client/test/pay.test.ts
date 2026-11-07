import assert from "node:assert/strict";
import { test } from "node:test";

import { payloadHash } from "../src/payload.ts";
import {
  MAX_EXPIRES_IN_S,
  type PayInput,
  type RequestArgs,
  type Submission,
  checkAgent,
  parsePayInput,
  pay,
  submissionOf,
  withDeadline,
} from "../src/pay.ts";
import type { Rpc } from "../src/rpc.ts";

const guardId = "guard.testnet";
const now = 1762300000000000000n;
const tst = { ft: "tst.testnet" };
const input: PayInput = { token: tst, receiver_id: "api.testnet", amount: "5" };

function chain(view?: Rpc["view"], accessKey?: Rpc["accessKey"]): Rpc {
  return {
    view: view ?? (async () => ({ symbol: "TST", decimals: 6 })),
    blockTimeNs: async () => now.toString(),
    chainId: async () => "testnet",
    accessKey: accessKey ?? (async () => null),
  };
}

function sending(submission: Submission) {
  const calls: { args: RequestArgs; tgas: number }[] = [];
  return {
    calls,
    sender: {
      requestPayment: async (args: RequestArgs, tgas: number) => {
        calls.push({ args, tgas });
        return submission;
      },
    },
  };
}

const final = (value: unknown): Submission => ({ kind: "final", value, tx_hash: "TX" });
const paid = final({ status: "paid", request_id: "7", sent: "5000000" });

test("pay asks the guard for exactly what was asked", async () => {
  const { calls, sender } = sending(paid);
  const report = await pay(input, { guardId, rpc: chain(), sender });
  assert.deepEqual(calls, [
    {
      args: {
        token: tst,
        receiver_id: "api.testnet",
        amount: "5000000",
        msg: null,
        expires_at: (now + 600_000_000_000n).toString(),
      },
      tgas: 40,
    },
  ]);
  assert.equal(report.status, "paid");
  assert.equal(report.request_id, "7");
  assert.equal(report.tx_hash, "TX");
  assert.equal(report.text, "paid: 5 TST to api.testnet (request 7)\ntransaction TX");
});

test("gas follows the method (decision 24)", async () => {
  const cases: [PayInput, number][] = [
    [{ ...input, token: "native", amount: "0.5" }, 30],
    [input, 40],
    [{ ...input, msg: "go" }, 100],
    [{ ...input, msg: "" }, 100],
  ];
  for (const [i, gas] of cases) {
    const { calls, sender } = sending(paid);
    await pay(i, { guardId, rpc: chain(), sender });
    assert.equal(calls[0].tgas, gas);
  }
});

test("NEAR is counted in yoctoNEAR", async () => {
  const { calls, sender } = sending(final({ status: "paid", request_id: "1", sent: "500000000000000000000000" }));
  const report = await pay({ ...input, token: "native", amount: "0.5" }, { guardId, rpc: chain(), sender });
  assert.equal(calls[0].args.amount, "500000000000000000000000");
  assert.match(report.text, /^paid: 0\.5 NEAR to /);
});

test("expiry is chain time plus the seconds asked for", async () => {
  const { calls, sender } = sending(paid);
  await pay({ ...input, expires_in_s: 60 }, { guardId, rpc: chain(), sender });
  await pay({ ...input, expires_in_s: MAX_EXPIRES_IN_S }, { guardId, rpc: chain(), sender });
  assert.equal(calls[0].args.expires_at, (now + 60_000_000_000n).toString());
  assert.equal(calls[1].args.expires_at, (now + 604_800_000_000_000n).toString());
});

test("nothing is sent for input the guard couldn't read", async () => {
  const bad: [PayInput, RegExp][] = [
    [{ ...input, amount: "0" }, /above 0/],
    [{ ...input, amount: "0.0000001" }, /more decimals/],
    [{ ...input, amount: "five" }, /decimal number/],
    [{ ...input, receiver_id: "Not An Account" }, /account id/],
    [{ ...input, token: { ft: "Bad" } }, /account id/],
    [{ ...input, token: "native", msg: "go" }, /FT token/],
    [{ ...input, token: "native", msg: "" }, /FT token/],
    [{ ...input, msg: "m".repeat(1025) }, /at most/],
    [{ ...input, expires_in_s: 0 }, /expires_in_s/],
    [{ ...input, expires_in_s: -1 }, /expires_in_s/],
    [{ ...input, expires_in_s: 1.5 }, /expires_in_s/],
    [{ ...input, expires_in_s: MAX_EXPIRES_IN_S + 1 }, /expires_in_s/],
  ];
  for (const [i, message] of bad) {
    const { calls, sender } = sending(paid);
    await assert.rejects(pay(i, { guardId, rpc: chain(), sender }), message, JSON.stringify(i));
    assert.equal(calls.length, 0);
  }
});

test("a token that gives no decimals stops the payment before it's sent", async () => {
  const { calls, sender } = sending(paid);
  await assert.rejects(pay(input, { guardId, rpc: chain(async () => ({})), sender }), /ft_metadata/);
  assert.equal(calls.length, 0);
});

test("a held payment prints what the approver will see", async () => {
  const args = {
    token: tst,
    receiver_id: "api.testnet",
    amount: "60000000",
    msg: null,
    expires_at: (now + 600_000_000_000n).toString(),
  };
  const payload = { request_id: "9", ...args };
  const hash = payloadHash(guardId, payload);
  const { sender } = sending(
    final({ status: "pending", request_id: "9", reason: "above_threshold", payload_hash: hash }),
  );
  const report = await pay({ ...input, amount: "60" }, { guardId, rpc: chain(), sender });
  assert.equal(report.status, "pending");
  const lines = report.text.split("\n");
  assert.equal(lines[0], "pending (above_threshold): request 9 waits for a human");
  assert.equal(lines[1], "amount   60 TST (60000000 base units)");
  assert.equal(lines[2], "receiver api.testnet");
  assert.match(lines[3], /^expires  \d{4}-/);
  assert.equal(lines[4], `hash     ${hash}`);
  assert.deepEqual(JSON.parse(lines[5].slice("payload  ".length)), payload);
  assert.equal(lines[6], "The approver reads it from the guard with: nsg-approve 9");
  assert.doesNotMatch(report.text, /WARNING/);
});

test("a hash that isn't the asked payload's says don't approve", async () => {
  const { sender } = sending(
    final({ status: "pending", request_id: "9", reason: "daily_cap", payload_hash: "ab".repeat(32) }),
  );
  const report = await pay(input, { guardId, rpc: chain(), sender });
  assert.match(report.text, /WARNING: the guard's hash is not this payload's/);
  assert.match(report.text, /Do not approve/);
});

test("each final outcome says what it means", async () => {
  const cases: [unknown, RegExp][] = [
    [{ status: "paid", request_id: "7", sent: "2000000" }, /^paid: 2 TST of 5 TST to api\.testnet; the receiver sent the rest back/],
    [{ status: "paid", request_id: "7", sent: "0" }, /^paid: 0 TST of 5 TST/],
    [{ status: "refused", request_id: "7", reason: "receiver_not_allowed" }, /^refused \(receiver_not_allowed\): nothing was sent or reserved/],
    [{ status: "failed", request_id: "7", released: "5000000" }, /^failed: nothing moved, 5 TST of budget came back/],
    [{ status: "uncertain", request_id: "7" }, /^uncertain: .*\nDo not pay again/],
    [{ status: "expired", request_id: "7" }, /^expired \(request 7\)/],
  ];
  for (const [value, text] of cases) {
    const report = await pay(input, { guardId, rpc: chain(), sender: sending(final(value)).sender });
    assert.match(report.text, text);
    assert.equal(report.status, (value as { status: string }).status);
    assert.equal(report.request_id, "7");
  }
});

test("a transaction the guard panicked on is rejected", async () => {
  const { sender } = sending({ kind: "rejected", error: "only the agent can call this\x1b[2K", tx_hash: "TX" });
  const report = await pay(input, { guardId, rpc: chain(), sender });
  assert.equal(report.status, "rejected");
  assert.equal(report.tx_hash, "TX");
  assert.match(report.text, /^rejected, nothing changed: "only the agent can call this\\u001b\[2K"$/);
});

test("giving up never reads as 'nothing happened'", async () => {
  const { sender } = sending({ kind: "unknown", why: "no answer within 30 s", tx_hash: "TX" });
  const report = await pay(input, { guardId, rpc: chain(), sender });
  assert.equal(report.status, "unknown");
  assert.equal(report.tx_hash, "TX");
  assert.match(report.text, /^unknown: no answer within 30 s\. The payment may still go through\. Transaction TX\./);
  assert.match(report.text, /Do not pay again before checking\.$/);
});

test("an error the RPC wrote can't reach the model as it is", async () => {
  const { sender } = sending({ kind: "unknown", why: "bad\x1b[2K", tx_hash: null });
  const report = await pay(input, { guardId, rpc: chain(), sender });
  assert.match(report.text, /^unknown: bad\\u001b\[2K\./);
});

test("unknown works without a transaction hash", async () => {
  const { sender } = sending({ kind: "unknown", why: "x", tx_hash: null });
  const report = await pay(input, { guardId, rpc: chain(), sender });
  assert.doesNotMatch(report.text, /Transaction/);
});

test("an answer this code can't read is unknown: the payment may have gone", async () => {
  for (const value of [{ status: "settled", request_id: "7" }, null, "paid", { status: "paid" }]) {
    const report = await pay(input, { guardId, rpc: chain(), sender: sending(final(value)).sender });
    assert.equal(report.status, "unknown", JSON.stringify(value));
    assert.equal(report.tx_hash, "TX");
    assert.match(report.text, /couldn't be read/);
  }
});

test("a token's symbol can't reach a terminal as it is", async () => {
  const { sender } = sending(paid);
  const rpc = chain(async () => ({ symbol: "T\x1b[2KST", decimals: 6 }));
  const report = await pay(input, { guardId, rpc, sender });
  assert.match(report.text, /^[\x20-\x7e\n]*$/);
});

test("pay's input is read strictly", () => {
  assert.deepEqual(parsePayInput({ token: "native", receiver_id: "a.testnet", amount: "1" }), {
    token: "native",
    receiver_id: "a.testnet",
    amount: "1",
    msg: null,
    expires_in_s: undefined,
  });
  assert.deepEqual(
    parsePayInput({ token: tst, receiver_id: "a.testnet", amount: "1", msg: "go", expires_in_s: 60 }),
    { token: tst, receiver_id: "a.testnet", amount: "1", msg: "go", expires_in_s: 60 },
  );
  const ok = { token: tst, receiver_id: "a.testnet", amount: "1" };
  const bad: [unknown, RegExp][] = [
    [null, /an object/],
    [[], /an object/],
    ["pay", /an object/],
    [{ ...ok, gas: 300 }, /no input "gas"/],
    [{ ...ok, deposit: "1" }, /no input/],
    [{ ...ok, token: "near" }, /token must be/],
    [{ ...ok, token: "tst.testnet" }, /token must be/],
    [{ ...ok, token: null }, /token must be/],
    [{ ...ok, token: { ft: 7 } }, /token must be/],
    [{ ...ok, token: { ft: "t.testnet", native: true } }, /token must be/],
    [{ ...ok, receiver_id: 7 }, /receiver_id/],
    [{ ...ok, amount: 5 }, /amount/],
    [{ ...ok, msg: 7 }, /msg/],
    [{ ...ok, expires_in_s: "60" }, /expires_in_s/],
  ];
  for (const [value, message] of bad) {
    assert.throws(() => parsePayInput(value), message, JSON.stringify(value));
  }
});

const b64 = (value: string) => Buffer.from(value).toString("base64");
const finished = (status: unknown, receipts: [string, unknown][] = [["r1", status]]) => ({
  status,
  transaction_outcome: { outcome: { receipt_ids: receipts.length ? [receipts[0][0]] : [] } },
  receipts_outcome: receipts.map(([id, s]) => ({ id, outcome: { status: s } })),
});
const panic = (text: string) => ({
  Failure: { ActionError: { index: 0, kind: { FunctionCallError: { ExecutionError: text } } } },
});

test("a finished transaction's value is read from its status", () => {
  const value = { status: "paid", request_id: "1", sent: "5" };
  assert.deepEqual(submissionOf(finished({ SuccessValue: b64(JSON.stringify(value)) }), "TX"), {
    kind: "final",
    value,
    tx_hash: "TX",
  });
});

test("a value that isn't JSON leaves the payment unknown", () => {
  const out = submissionOf(finished({ SuccessValue: b64("{") }), "TX");
  assert.equal(out.kind, "unknown");
});

test("a panic in request_payment itself means nothing happened", () => {
  const out = submissionOf(finished(panic("Smart contract panicked: only the agent can call this")), "TX");
  assert.deepEqual(out, {
    kind: "rejected",
    error: "Smart contract panicked: only the agent can call this",
    tx_hash: "TX",
  });
});

test("a failure after the first receipt doesn't say nothing moved", () => {
  const out = submissionOf(
    finished(panic("Exceeded the prepaid gas"), [
      ["r1", { SuccessReceiptId: "r2" }],
      ["r2", { SuccessValue: "" }],
      ["r3", panic("Exceeded the prepaid gas")],
    ]),
    "TX",
  );
  assert.equal(out.kind, "unknown");
  assert.match((out as { why: string }).why, /later step failed/);
});

test("a transaction that never became a receipt did nothing", () => {
  const out = submissionOf(finished({ Failure: { InvalidTxError: "Expired" } }, []), "TX");
  assert.equal(out.kind, "rejected");
  assert.match((out as { error: string }).error, /Expired/);
});

test("a first receipt missing from the answer doesn't prove anything", () => {
  const tx = finished(panic("x"));
  tx.receipts_outcome = [];
  assert.equal(submissionOf(tx, "TX").kind, "unknown");
});

test("no final status is unknown", () => {
  for (const status of ["Started", "NotStarted", { Started: null }, null]) {
    assert.equal(submissionOf(finished(status), "TX").kind, "unknown", JSON.stringify(status));
  }
});

test("a deadline gives up without stopping the work", async () => {
  assert.equal(await withDeadline(new Promise(() => {}), 5), "timeout");
  assert.equal(await withDeadline(Promise.resolve("done"), 1000), "done");
  await assert.rejects(withDeadline(Promise.reject(new Error("no")), 1000), /no/);
});

test("the agent's key has to be the narrow one", async () => {
  const config = async () => ({
    owner_id: "o.testnet",
    agent_id: "agent.testnet",
    approver_id: "h.testnet",
    paused: false,
    version: "1.0.0",
  });
  const withKey = (permission: unknown) => chain(config, async () => ({ permission }));
  const ok = { FunctionCall: { allowance: "1", receiver_id: guardId, method_names: ["request_payment"] } };
  await checkAgent(withKey(ok), guardId, "agent.testnet", "ed25519:k");
  const refused = [
    "FullAccess",
    { FunctionCall: { ...ok.FunctionCall, receiver_id: "other.testnet" } },
    { FunctionCall: { ...ok.FunctionCall, method_names: [] } },
    { FunctionCall: { ...ok.FunctionCall, method_names: ["request_payment", "pause"] } },
    { FunctionCall: { ...ok.FunctionCall, method_names: ["pause"] } },
    undefined,
  ];
  for (const permission of refused) {
    await assert.rejects(
      checkAgent(withKey(permission), guardId, "agent.testnet", "ed25519:k"),
      /refusing to run/,
      JSON.stringify(permission),
    );
  }
  await assert.rejects(
    checkAgent(withKey(ok), guardId, "someone.testnet", "ed25519:k"),
    /agent is agent\.testnet, not someone\.testnet/,
  );
});
