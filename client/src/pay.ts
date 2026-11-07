// The agent's one move: ask the guard for a payment and say what came of it.
// Only `request_payment` is reachable from here (`Sender` has no other method),
// and the guard decides; this file only asks and reports.

import { formatAmount, parseAmount } from "./amount.ts";
import { plain, printable } from "./display.ts";
import {
  type RequestOutcome,
  type TokenInfo,
  agentGas,
  guardConfig,
  isoTime,
  methodOf,
  parseOutcome,
  tokenInfo,
} from "./guard.ts";
import { type PaymentPayload, type Token, payloadBytes, payloadHash } from "./payload.ts";
import type { Rpc } from "./rpc.ts";

export const DEFAULT_EXPIRES_IN_S = 600;
export const MAX_EXPIRES_IN_S = 7 * 24 * 60 * 60;

export interface PayInput {
  token: Token;
  receiver_id: string;
  /** In whole tokens, as the invoice writes it. */
  amount: string;
  msg?: string | null;
  expires_in_s?: number;
}

/** The arguments of `request_payment`, as the guard's JSON has them. */
export type RequestArgs = Omit<PaymentPayload, "request_id">;

/** What became of a transaction. Only `rejected` says nothing changed. Anything
 * short of a final answer is `unknown`: giving up on a transaction doesn't
 * cancel it, so "it failed" would invite the agent to pay twice. */
export type Submission =
  | { kind: "final"; value: unknown; tx_hash: string }
  | { kind: "rejected"; error: string; tx_hash: string | null }
  | { kind: "unknown"; why: string; tx_hash: string | null };

export interface Sender {
  requestPayment(args: RequestArgs, tgas: number): Promise<Submission>;
}

export type PayStatus = RequestOutcome["status"] | "rejected" | "unknown";

export interface PayReport {
  status: PayStatus;
  text: string;
  request_id: string | null;
  tx_hash: string | null;
}

const INPUT_FIELDS = ["token", "receiver_id", "amount", "msg", "expires_in_s"];

/** `pay`'s input as it arrives from a model: anything unexpected is refused. */
export function parsePayInput(value: unknown): PayInput {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("pay takes an object");
  }
  const input = value as Record<string, unknown>;
  for (const key of Object.keys(input)) {
    if (!INPUT_FIELDS.includes(key)) {
      throw new Error(`pay has no input ${printable(key)}`);
    }
  }
  const { token, receiver_id, amount, msg, expires_in_s } = input;
  const ft = typeof token === "object" && token !== null ? (token as { ft?: unknown }).ft : null;
  if (token !== "native" && (typeof ft !== "string" || Object.keys(token as object).length !== 1)) {
    throw new Error('token must be "native" or {"ft": "<token account>"}');
  }
  if (typeof receiver_id !== "string") {
    throw new Error("receiver_id must be a string");
  }
  if (typeof amount !== "string") {
    throw new Error('amount must be a string such as "5.25", in whole tokens');
  }
  if (msg !== undefined && msg !== null && typeof msg !== "string") {
    throw new Error("msg must be a string");
  }
  if (expires_in_s !== undefined && typeof expires_in_s !== "number") {
    throw new Error("expires_in_s must be a number of seconds");
  }
  return {
    token: token === "native" ? "native" : { ft: ft as string },
    receiver_id,
    amount,
    msg: msg ?? null,
    expires_in_s,
  };
}

function shown(base: string, info: TokenInfo): string {
  return `${formatAmount(base, info.decimals)} ${plain(info.symbol)}`;
}

function report(
  outcome: RequestOutcome,
  payload: Omit<PaymentPayload, "request_id">,
  guardId: string,
  info: TokenInfo,
  txHash: string,
): PayReport {
  const { request_id } = outcome;
  const amount = shown(payload.amount, info);
  const to = payload.receiver_id;
  const lines = ((): string[] => {
    switch (outcome.status) {
      case "paid":
        return outcome.sent === payload.amount
          ? [`paid: ${amount} to ${to} (request ${request_id})`]
          : [`paid: ${shown(outcome.sent, info)} of ${amount} to ${to}; the receiver sent the rest back (request ${request_id})`];
      case "refused":
        return [`refused (${outcome.reason}): nothing was sent or reserved (request ${request_id})`];
      case "failed":
        return [`failed: nothing moved, ${shown(outcome.released, info)} of budget came back (request ${request_id})`];
      case "uncertain":
        return [
          `uncertain: the transfer of ${amount} to ${to} reported a failure after the tokens may have moved (request ${request_id})`,
          "Do not pay again before checking the receiver's balance.",
        ];
      case "pending": {
        const full: PaymentPayload = { request_id, ...payload };
        const ours = payloadHash(guardId, full);
        return [
          `pending (${outcome.reason}): request ${request_id} waits for a human`,
          `amount   ${amount} (${payload.amount} base units)`,
          `receiver ${to}`,
          `expires  ${isoTime(payload.expires_at)}`,
          `hash     ${outcome.payload_hash}`,
          `payload  ${JSON.stringify(full)}`,
          ...(ours === outcome.payload_hash
            ? []
            : [`WARNING: the guard's hash is not this payload's (${ours}). Do not approve.`]),
          `The approver reads it from the guard with: nsg-approve ${request_id}`,
        ];
      }
      default:
        return [`${outcome.status} (request ${request_id})`];
    }
  })();
  lines.push(`transaction ${txHash}`);
  return { status: outcome.status, text: lines.join("\n"), request_id, tx_hash: txHash };
}

interface FinishedTx {
  status: unknown;
  transaction_outcome: { outcome: { receipt_ids: string[] } };
  receipts_outcome: { id: string; outcome: { status: unknown } }[];
}

function failure(status: unknown): unknown {
  return typeof status === "object" && status !== null
    ? (status as { Failure?: unknown }).Failure
    : undefined;
}

function panicText(error: unknown): string {
  const kind = (error as { ActionError?: { kind?: { FunctionCallError?: { ExecutionError?: unknown } } } })
    .ActionError?.kind?.FunctionCallError?.ExecutionError;
  return typeof kind === "string" ? kind : JSON.stringify(error);
}

/** What a transaction that ran to the end says. The first receipt is
 * `request_payment` itself: if that failed, the guard panicked and nothing
 * happened. A failure further along can be the callback, after a transfer went
 * out, so it can't be read as "nothing moved". */
export function submissionOf(tx: FinishedTx, txHash: string): Submission {
  const result = (tx.status as { SuccessValue?: unknown } | null)?.SuccessValue;
  if (typeof result === "string") {
    try {
      const value: unknown = JSON.parse(Buffer.from(result, "base64").toString("utf8"));
      return { kind: "final", value, tx_hash: txHash };
    } catch {
      return { kind: "unknown", why: "the guard's answer isn't JSON", tx_hash: txHash };
    }
  }
  const error = failure(tx.status);
  if (error === undefined) {
    return { kind: "unknown", why: "the transaction has no final status", tx_hash: txHash };
  }
  const first = tx.transaction_outcome.outcome.receipt_ids[0];
  const own = tx.receipts_outcome.find((r) => r.id === first);
  if (first === undefined || failure(own?.outcome.status) !== undefined) {
    return { kind: "rejected", error: panicText(error), tx_hash: txHash };
  }
  return {
    kind: "unknown",
    why: `a later step failed after the guard took the request: ${panicText(error)}`,
    tx_hash: txHash,
  };
}

/** `work`'s result, or "timeout" once `ms` have passed. `work` isn't stopped:
 * nothing can recall a transaction that has been broadcast. */
export async function withDeadline<T>(work: Promise<T>, ms: number): Promise<T | "timeout"> {
  let timer: NodeJS.Timeout | undefined;
  const deadline = new Promise<"timeout">((resolve) => {
    timer = setTimeout(() => resolve("timeout"), ms);
  });
  try {
    return await Promise.race([work, deadline]);
  } finally {
    clearTimeout(timer);
  }
}

/** Asks the guard for one payment. Throws, before anything is sent, on input
 * the guard would refuse to read; everything after sending comes back as a
 * report, because by then the payment may have happened. */
export async function pay(
  input: PayInput,
  deps: { guardId: string; rpc: Rpc; sender: Sender },
): Promise<PayReport> {
  const { guardId, rpc, sender } = deps;
  const msg = input.msg ?? null;
  const expiresIn = input.expires_in_s ?? DEFAULT_EXPIRES_IN_S;
  if (!Number.isInteger(expiresIn) || expiresIn < 1 || expiresIn > MAX_EXPIRES_IN_S) {
    throw new Error(`expires_in_s must be a whole number from 1 to ${MAX_EXPIRES_IN_S}`);
  }
  if (input.token === "native" && msg !== null) {
    throw new Error("msg needs an FT token");
  }
  const info = await tokenInfo(rpc, input.token);
  const amount = parseAmount(input.amount, info.decimals);
  // Chain time, not this machine's: expiry is checked against block_timestamp.
  const now = BigInt(await rpc.blockTimeNs());
  const args: RequestArgs = {
    token: input.token,
    receiver_id: input.receiver_id,
    amount,
    msg,
    expires_at: (now + BigInt(expiresIn) * 1_000_000_000n).toString(),
  };
  // Every field in the form the guard reads, or an error before sending.
  payloadBytes(guardId, { request_id: "0", ...args });

  const sent = await sender.requestPayment(args, agentGas(methodOf(args.token, msg)));
  switch (sent.kind) {
    case "rejected":
      return {
        status: "rejected",
        text: `rejected, nothing changed: ${printable(sent.error)}`,
        request_id: null,
        tx_hash: sent.tx_hash,
      };
    case "unknown":
      return unknown(sent.why, sent.tx_hash);
    case "final":
      try {
        return report(parseOutcome(sent.value), args, guardId, info, sent.tx_hash);
      } catch (e) {
        return unknown(`the guard's answer couldn't be read (${(e as Error).message})`, sent.tx_hash);
      }
  }
}

function unknown(why: string, txHash: string | null): PayReport {
  const tx = txHash === null ? "" : ` Transaction ${txHash}.`;
  return {
    status: "unknown",
    text: `unknown: ${plain(why)}. The payment may still go through.${tx}\nDo not pay again before checking.`,
    request_id: null,
    tx_hash: txHash,
  };
}

/** Refuses a set-up where the guard wouldn't take this agent's requests, or
 * where the key could do more than ask: the whole point is an agent that holds
 * nothing else. */
export async function checkAgent(
  rpc: Rpc,
  guardId: string,
  agentId: string,
  publicKey: string,
): Promise<void> {
  const config = await guardConfig(rpc, guardId);
  if (config.agent_id !== agentId) {
    throw new Error(`${guardId}'s agent is ${config.agent_id}, not ${agentId}`);
  }
  const { permission } = (await rpc.accessKey(agentId, publicKey)) as {
    permission?: { FunctionCall?: { receiver_id?: string; method_names?: string[] } } | string;
  };
  const limited = typeof permission === "object" ? permission.FunctionCall : undefined;
  if (
    limited?.receiver_id !== guardId ||
    limited.method_names?.length !== 1 ||
    limited.method_names[0] !== "request_payment"
  ) {
    throw new Error(
      `refusing to run: this key must be a function-call key for request_payment on ${guardId}, ` +
        `and it is ${printable(JSON.stringify(permission) ?? "missing")}`,
    );
  }
}
