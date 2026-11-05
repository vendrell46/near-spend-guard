import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { MAX_MSG_BYTES, type PaymentPayload, payloadBytes, payloadHash } from "../src/payload.ts";

// The same file `payload_hash_matches_vectors` checks on the Rust side.
const vectors: {
  name: string;
  guard_id: string;
  payload: PaymentPayload;
  borsh: string;
  hash: string;
}[] = JSON.parse(
  readFileSync(
    new URL("../../contracts/guard/src/tests/payload_vectors.json", import.meta.url),
    "utf8",
  ),
);

const guard = "guard.testnet";
const base: PaymentPayload = vectors[0].payload;

for (const v of vectors) {
  test(`vector: ${v.name}`, () => {
    assert.equal(payloadBytes(v.guard_id, v.payload).toString("hex"), v.borsh);
    assert.equal(payloadHash(v.guard_id, v.payload), v.hash);
  });
}

test("the hash covers every field", () => {
  const changed: PaymentPayload[] = [
    { ...base, request_id: "8" },
    { ...base, token: "native" },
    { ...base, token: { ft: "other.testnet" } },
    { ...base, receiver_id: "mallory.testnet" },
    { ...base, amount: "50000001" },
    { ...base, msg: "" },
    { ...base, expires_at: "1762300800000000001" },
  ];
  const hashes = new Set([base, ...changed].map((p) => payloadHash(guard, p)));
  assert.equal(hashes.size, changed.length + 1);
});

test("only the guard's own spelling of a number is accepted", () => {
  for (const amount of ["007", "+7", "7.0", "", " 7", "0x7", "-1", "1e3"]) {
    assert.throws(() => payloadHash(guard, { ...base, amount }), /decimal string/, amount);
  }
});

test("numbers are bounded by their width", () => {
  const u64 = (1n << 64n).toString();
  const u128 = (1n << 128n).toString();
  assert.throws(() => payloadHash(guard, { ...base, request_id: u64 }), /8 bytes/);
  assert.throws(() => payloadHash(guard, { ...base, expires_at: u64 }), /8 bytes/);
  assert.throws(() => payloadHash(guard, { ...base, amount: u128 }), /16 bytes/);
});

test("msg length is counted in bytes", () => {
  payloadHash(guard, { ...base, msg: "m".repeat(MAX_MSG_BYTES) });
  assert.throws(() => payloadHash(guard, { ...base, msg: "m".repeat(MAX_MSG_BYTES + 1) }), /at most/);
  assert.throws(() => payloadHash(guard, { ...base, msg: "é".repeat(513) }), /at most/);
});

test("a msg the guard could never store is refused", () => {
  assert.throws(() => payloadHash(guard, { ...base, msg: "\ud800" }), /well-formed/);
});

test("account ids follow NEAR's rules", () => {
  for (const id of ["", "a", "Alice.testnet", ".alice", "alice.", "a..b", "a b", "a".repeat(65)]) {
    assert.throws(() => payloadHash(guard, { ...base, receiver_id: id }), /account id/, id);
  }
  assert.throws(() => payloadHash(guard, { ...base, token: { ft: "Bad" } }), /account id/);
  assert.throws(() => payloadHash("Bad", base), /account id/);
  payloadHash(guard, { ...base, receiver_id: "a-b_c.d" });
});
