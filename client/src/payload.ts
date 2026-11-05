// The payload a human approves, and its hash, as the guard computes them.
// INTERFACE.md has the layout; the vectors in
// contracts/guard/src/tests/payload_vectors.json bind this file to the Rust
// code. The hash identifies a payload in logs and tools. It is never signed:
// `approve` carries the payload itself.

import { createHash } from "node:crypto";

export const PAYLOAD_DOMAIN = "near-spend-guard/payload/v1";
export const MAX_MSG_BYTES = 1024;

export type Token = "native" | { ft: string };

/** As the guard's JSON has it, with numbers as decimal strings. */
export interface PaymentPayload {
  request_id: string;
  token: Token;
  receiver_id: string;
  amount: string;
  msg: string | null;
  expires_at: string;
}

const ACCOUNT_ID = /^(?:[a-z\d]+[-_])*[a-z\d]+(?:\.(?:[a-z\d]+[-_])*[a-z\d]+)*$/;

function u8(value: number): Buffer {
  return Buffer.from([value]);
}

function u32(value: number): Buffer {
  const out = Buffer.alloc(4);
  out.writeUInt32LE(value);
  return out;
}

/** A little-endian integer from a decimal string. Only the form the guard
 * prints is accepted, so two spellings can't name one payload. */
function uint(value: string, bytes: number, field: string): Buffer {
  if (!/^(?:0|[1-9]\d*)$/.test(value)) {
    throw new Error(`${field} must be a decimal string, got ${JSON.stringify(value)}`);
  }
  let rest = BigInt(value);
  if (rest >> BigInt(bytes * 8) !== 0n) {
    throw new Error(`${field} does not fit in ${bytes} bytes: ${value}`);
  }
  const out = Buffer.alloc(bytes);
  for (let i = 0; i < bytes; i++) {
    out[i] = Number(rest & 0xffn);
    rest >>= 8n;
  }
  return out;
}

function text(value: string): Buffer {
  const bytes = Buffer.from(value, "utf8");
  return Buffer.concat([u32(bytes.length), bytes]);
}

function accountId(value: string, field: string): Buffer {
  if (value.length < 2 || value.length > 64 || !ACCOUNT_ID.test(value)) {
    throw new Error(`${field} is not a NEAR account id: ${JSON.stringify(value)}`);
  }
  return text(value);
}

function token(value: Token): Buffer {
  return value === "native"
    ? u8(0)
    : Buffer.concat([u8(1), accountId(value.ft, "token.ft")]);
}

function msg(value: string | null): Buffer {
  if (value === null) {
    return u8(0);
  }
  // `Buffer.from` would swap a lone surrogate for U+FFFD, hashing a string
  // the guard could never have stored.
  if (!value.isWellFormed()) {
    throw new Error("msg is not well-formed Unicode");
  }
  if (Buffer.byteLength(value, "utf8") > MAX_MSG_BYTES) {
    throw new Error(`msg can be at most ${MAX_MSG_BYTES} bytes`);
  }
  return Buffer.concat([u8(1), text(value)]);
}

/** `borsh((PAYLOAD_DOMAIN, guard account id, payload))`. */
export function payloadBytes(guardId: string, payload: PaymentPayload): Buffer {
  return Buffer.concat([
    text(PAYLOAD_DOMAIN),
    accountId(guardId, "guard"),
    uint(payload.request_id, 8, "request_id"),
    token(payload.token),
    accountId(payload.receiver_id, "receiver_id"),
    uint(payload.amount, 16, "amount"),
    msg(payload.msg),
    uint(payload.expires_at, 8, "expires_at"),
  ]);
}

/** Lowercase hex, as in the guard's events and its `payload_hash` view. */
export function payloadHash(guardId: string, payload: PaymentPayload): string {
  return createHash("sha256").update(payloadBytes(guardId, payload)).digest("hex");
}
