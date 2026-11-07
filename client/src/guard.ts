// What the guard says, read strictly: an answer this code doesn't recognise is
// an error, never a guess. INTERFACE.md has the types.

import gas from "./gas.json" with { type: "json" };
import { NEAR_DECIMALS } from "./amount.ts";
import { printable } from "./display.ts";
import { type PaymentPayload, type Token, checkAccountId, payloadHash } from "./payload.ts";
import type { Rpc } from "./rpc.ts";

export const REFUSAL_REASONS = [
  "paused",
  "token_not_configured",
  "receiver_not_allowed",
  "transfer_call_not_allowed",
  "above_per_payment_max",
  "too_many_pending",
  "insufficient_balance",
] as const;
export const PENDING_REASONS = ["above_threshold", "daily_cap"] as const;

export type RefusalReason = (typeof REFUSAL_REASONS)[number];
export type PendingReason = (typeof PENDING_REASONS)[number];
export type Method = "native" | "ft_transfer" | "ft_transfer_call";

export type RequestOutcome =
  | { status: "refused"; request_id: string; reason: RefusalReason }
  | { status: "pending"; request_id: string; reason: PendingReason; payload_hash: string }
  | { status: "paid"; request_id: string; sent: string }
  | { status: "failed"; request_id: string; released: string }
  | { status: "uncertain"; request_id: string }
  | { status: "expired"; request_id: string };

export interface PendingView {
  payload: PaymentPayload;
  payload_hash: string;
  reason: PendingReason;
  created_at: string;
}

export interface GuardConfig {
  owner_id: string;
  agent_id: string;
  approver_id: string;
  paused: boolean;
  version: string;
}

export interface TokenInfo {
  symbol: string;
  decimals: number;
}

/** A token's metadata is the token's to write, and it reaches the approver's
 * screen. */
const MAX_SYMBOL_CHARS = 32;

function object(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${what} is not an object: ${printable(JSON.stringify(value) ?? "undefined")}`);
  }
  return value as Record<string, unknown>;
}

function decimal(value: unknown, field: string): string {
  if (typeof value !== "string" || !/^(?:0|[1-9]\d*)$/.test(value)) {
    throw new Error(`${field} is not a decimal string`);
  }
  return value;
}

function oneOf<T extends string>(value: unknown, allowed: readonly T[], field: string): T {
  if (!allowed.includes(value as T)) {
    throw new Error(`unknown ${field}: ${printable(String(value))}`);
  }
  return value as T;
}

function hash(value: unknown): string {
  if (typeof value !== "string" || !/^[0-9a-f]{64}$/.test(value)) {
    throw new Error("payload_hash is not 64 hex characters");
  }
  return value;
}

/** A `RequestOutcome` from the guard's JSON. */
export function parseOutcome(value: unknown): RequestOutcome {
  const o = object(value, "the outcome");
  const request_id = decimal(o.request_id, "request_id");
  switch (o.status) {
    case "refused":
      return { status: "refused", request_id, reason: oneOf(o.reason, REFUSAL_REASONS, "reason") };
    case "pending":
      return {
        status: "pending",
        request_id,
        reason: oneOf(o.reason, PENDING_REASONS, "reason"),
        payload_hash: hash(o.payload_hash),
      };
    case "paid":
      return { status: "paid", request_id, sent: decimal(o.sent, "sent") };
    case "failed":
      return { status: "failed", request_id, released: decimal(o.released, "released") };
    case "uncertain":
    case "expired":
      return { status: o.status, request_id };
    default:
      throw new Error(`unknown status: ${printable(String(o.status))}`);
  }
}

function parseToken(value: unknown): Token {
  if (value === "native") {
    return "native";
  }
  const { ft } = object(value, "the token");
  if (typeof ft !== "string") {
    throw new Error("token.ft is not a string");
  }
  return { ft };
}

/** A `PendingView`, whose hash must be the one this code computes for
 * `guardId`, so a client and a guard that disagree stop here. */
export function parsePending(guardId: string, value: unknown): PendingView {
  const v = object(value, "the request");
  const p = object(v.payload, "the payload");
  if (typeof p.receiver_id !== "string") {
    throw new Error("receiver_id is not a string");
  }
  if (p.msg !== null && typeof p.msg !== "string") {
    throw new Error("msg is neither a string nor null");
  }
  const payload: PaymentPayload = {
    request_id: decimal(p.request_id, "request_id"),
    token: parseToken(p.token),
    receiver_id: p.receiver_id,
    amount: decimal(p.amount, "amount"),
    msg: p.msg,
    expires_at: decimal(p.expires_at, "expires_at"),
  };
  const payload_hash = hash(v.payload_hash);
  if (payloadHash(guardId, payload) !== payload_hash) {
    throw new Error(`the guard's hash for request ${payload.request_id} is not this payload's`);
  }
  return {
    payload,
    payload_hash,
    reason: oneOf(v.reason, PENDING_REASONS, "reason"),
    created_at: decimal(v.created_at, "created_at"),
  };
}

export function parseConfig(value: unknown): GuardConfig {
  const c = object(value, "the guard's config");
  for (const field of ["owner_id", "agent_id", "approver_id"]) {
    if (typeof c[field] !== "string") {
      throw new Error(`the guard's config has no ${field}`);
    }
    // These reach the screen in error messages, and an RPC can say anything.
    checkAccountId(c[field], field);
  }
  if (typeof c.version !== "string") {
    throw new Error("the guard's config has no version");
  }
  if (typeof c.paused !== "boolean") {
    throw new Error("the guard's config has no paused flag");
  }
  return c as unknown as GuardConfig;
}

export function methodOf(token: Token, msg: string | null): Method {
  if (token === "native") {
    return "native";
  }
  return msg === null ? "ft_transfer" : "ft_transfer_call";
}

/** What the agent attaches, by method (decision 24). */
export function agentGas(method: Method): number {
  return gas[method];
}

export const APPROVE_TGAS: number = gas.approve;

/** How a token writes its amounts. Native needs no call; for an FT the
 * token's own metadata says, and the base units are always shown beside it. */
export async function tokenInfo(rpc: Rpc, token: Token): Promise<TokenInfo> {
  if (token === "native") {
    return { symbol: "NEAR", decimals: NEAR_DECIMALS };
  }
  const m = object(await rpc.view(token.ft, "ft_metadata", {}), "ft_metadata");
  if (typeof m.symbol !== "string" || !Number.isInteger(m.decimals)) {
    throw new Error(`${printable(token.ft)} has no usable ft_metadata`);
  }
  return { symbol: m.symbol.slice(0, MAX_SYMBOL_CHARS), decimals: m.decimals as number };
}

export async function guardConfig(rpc: Rpc, guardId: string): Promise<GuardConfig> {
  return parseConfig(await rpc.view(guardId, "get_config", {}));
}

/** The pending request, or null if the guard holds none with this id. */
export async function pendingRequest(
  rpc: Rpc,
  guardId: string,
  requestId: string,
): Promise<PendingView | null> {
  const value = await rpc.view(guardId, "get_request", { request_id: requestId });
  return value === null ? null : parsePending(guardId, value);
}

/** "2026-11-14T12:00:00.000Z" from the guard's nanoseconds. */
export function isoTime(ns: string): string {
  return new Date(Number(BigInt(ns) / 1_000_000n)).toISOString();
}
