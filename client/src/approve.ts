// `nsg-approve <request_id>`: what the approver runs, on their own machine. The
// request number is the only thing it takes from the agent's side. The guard's
// id comes from the approver's config and the payment from that guard, so a
// compromised agent can't print a command for a look-alike contract and have it
// run. near CLI then shows the transaction and asks how to sign; this helper
// holds no key and signs nothing.

import { homedir } from "node:os";
import { join } from "node:path";

import { formatAmount } from "./amount.ts";
import { isPlainAscii, plain, printable } from "./display.ts";
import {
  APPROVE_TGAS,
  type PendingView,
  type TokenInfo,
  guardConfig,
  isoTime,
  methodOf,
  pendingRequest,
  tokenInfo,
} from "./guard.ts";
import { type PaymentPayload, checkAccountId } from "./payload.ts";
import { type Rpc, refuseMainnet } from "./rpc.ts";

export interface ApproverConfig {
  guard_id: string;
  approver_id: string;
  /** near CLI's name for the network. */
  network: string;
  rpc_url: string;
  /** The near CLI binary, if `near` on `PATH` isn't it. */
  near_cli?: string;
}

export const USAGE = "usage: nsg-approve <request_id>";

export function configPath(env: Record<string, string | undefined>): string {
  return join(
    env.XDG_CONFIG_HOME || join(homedir(), ".config"),
    "near-spend-guard",
    "approver.json",
  );
}

export function parseApproverConfig(text: string): ApproverConfig {
  const config = JSON.parse(text) as ApproverConfig;
  for (const field of ["guard_id", "approver_id", "network", "rpc_url"] as const) {
    if (typeof config[field] !== "string" || config[field] === "") {
      throw new Error(`the config has no ${field}`);
    }
  }
  checkAccountId(config.guard_id, "guard_id");
  checkAccountId(config.approver_id, "approver_id");
  if (!/^[a-z0-9][a-z0-9-]*$/.test(config.network)) {
    throw new Error(`the config's network isn't a near CLI network name: ${printable(config.network)}`);
  }
  // near CLI names mainnet connections `mainnet`, `mainnet-fastnear` and
  // `mainnet-lava` (0.31.1's default config), so the name is checked, not
  // matched: near CLI signs on its own connection, whatever rpc_url says.
  if (config.network.includes("mainnet")) {
    throw new Error("near-spend-guard's client runs on testnet only");
  }
  return config;
}

/** Exactly one argument, and it has to be a request number. There is no way
 * to pass a guard, an amount or a payload. */
export function requestIdFrom(argv: string[]): string {
  const id = argv[0] ?? "";
  if (argv.length !== 1 || !/^[1-9]\d*$/.test(id) || BigInt(id) >= 1n << 64n) {
    throw new Error(USAGE);
  }
  return id;
}

/** near CLI, stopping where it asks how to sign: no signing option is given. */
export function nearArgs(config: ApproverConfig, payload: PaymentPayload): string[] {
  return [
    "contract",
    "call-function",
    "as-transaction",
    config.guard_id,
    "approve",
    "json-args",
    JSON.stringify({ payload }),
    "prepaid-gas",
    `${APPROVE_TGAS} Tgas`,
    "attached-deposit",
    "1 yoctoNEAR",
    "sign-as",
    config.approver_id,
    "network-config",
    config.network,
  ];
}

function amountLines(payload: PaymentPayload, info: TokenInfo | null): string[] {
  const base = `${payload.amount} in the token's smallest unit`;
  if (info === null) {
    return [`Pay       ${base}`, "          (the token gave no decimals, so check this number)"];
  }
  const amount = `${formatAmount(payload.amount, info.decimals)} ${plain(info.symbol)}`;
  return [`Pay       ${amount}`, `          ${base}`];
}

export function describeRequest(
  guardId: string,
  view: PendingView,
  info: TokenInfo | null,
): string[] {
  const { payload } = view;
  return [
    `Guard     ${guardId} (from your config)`,
    `Request   ${payload.request_id} (${view.reason})`,
    ...amountLines(payload, info),
    `To        ${payload.receiver_id}`,
    `Token     ${payload.token === "native" ? "NEAR" : payload.token.ft}`,
    `Method    ${methodOf(payload.token, payload.msg)}`,
    `Message   ${payload.msg === null ? "none" : printable(payload.msg)}`,
    ...(payload.msg !== null && !isPlainAscii(payload.msg)
      ? ["          (not plain ASCII: near CLI may show this differently, so go by the line above)"]
      : []),
    `Expires   ${isoTime(payload.expires_at)}`,
    `Hash      ${view.payload_hash}`,
  ];
}

export interface ApproveDeps {
  config: ApproverConfig;
  rpc: Rpc;
  out: (line: string) => void;
  runNear: (command: string, args: string[]) => Promise<number>;
}

/** Reads the request from the guard, prints it and starts near CLI. Returns
 * near CLI's exit code. Throws, before starting it, if the approval could only
 * fail. */
export async function approve(argv: string[], deps: ApproveDeps): Promise<number> {
  const { config, rpc, out, runNear } = deps;
  const id = requestIdFrom(argv);
  await refuseMainnet(rpc);
  const guard = await guardConfig(rpc, config.guard_id);
  if (guard.approver_id !== config.approver_id) {
    throw new Error(`${config.guard_id}'s approver is ${guard.approver_id}, not ${config.approver_id}`);
  }
  if (guard.paused) {
    throw new Error(`${config.guard_id} is paused, so an approval would fail`);
  }
  const view = await pendingRequest(rpc, config.guard_id, id);
  if (view === null) {
    throw new Error(`${config.guard_id} holds no pending request ${id}`);
  }
  if (view.payload.request_id !== id) {
    throw new Error(`asked for request ${id} and got ${view.payload.request_id}`);
  }
  if (BigInt(await rpc.blockTimeNs()) >= BigInt(view.payload.expires_at)) {
    throw new Error(`request ${id} expired at ${isoTime(view.payload.expires_at)}`);
  }
  const info = await tokenInfo(rpc, view.payload.token).catch(() => null);
  for (const line of describeRequest(config.guard_id, view, info)) {
    out(line);
  }
  out("");
  out("near CLI shows the same payment next and asks how to sign. Nothing is signed before that.");
  return runNear(config.near_cli ?? "near", nearArgs(config, view.payload));
}
