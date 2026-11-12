// The testnet demo world, as data: account names, the policy, the arguments
// of every set-up call and the files the deploy writes. Nothing here touches
// the network; client/deploy/testnet.ts does, step by step, in the order of
// the sandbox tests' own set-up (sandbox-tests/tests/common/mod.rs). Like
// client/src apart from near.ts, it loads nothing from npm, so its tests run
// with a bare `npm test`.

import { closeSync, constants, openSync, writeSync } from "node:fs";

/** 1 TST in base units: the test token has 6 decimals. */
export const TST = 1_000_000n;
export const TOTAL_SUPPLY = 1_000_000n * TST;
export const GUARD_FUNDING = 1_000n * TST;
export const SLOT_NS = "3600000000000";
/** DEMO.md: per payment 200, approval above 40, daily 100 (TST). */
export const POLICY = {
  per_payment_max: (200n * TST).toString(),
  daily_max: (100n * TST).toString(),
  approval_above: (40n * TST).toString(),
};
/** The agent's function-call key may spend this much NEAR on gas, no more. */
export const AGENT_KEY_ALLOWANCE_NEAR = "1";

export interface World {
  prefix: string;
  guard: string;
  agent: string;
  approver: string;
  demo: string;
  token: string;
  api: string;
  mallory: string;
}

/** Top-level testnet names come from the faucet as `<name>.testnet`; the
 * prefix keeps the four together and leaves room for `-approver`. */
export function checkPrefix(prefix: string): string {
  if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(prefix) || prefix.length > 40) {
    throw new Error(`not a usable prefix: ${JSON.stringify(prefix)} (lowercase letters, digits and single hyphens, up to 40)`);
  }
  return prefix;
}

export function worldOf(prefix: string): World {
  checkPrefix(prefix);
  const demo = `${prefix}-demo.testnet`;
  return {
    prefix,
    guard: `${prefix}-guard.testnet`,
    agent: `${prefix}-agent.testnet`,
    approver: `${prefix}-approver.testnet`,
    demo,
    token: `tst.${demo}`,
    api: `api.${demo}`,
    mallory: `mallory.${demo}`,
  };
}

/** The four faucet accounts, in the order they're created. */
export function faucetAccounts(world: World): string[] {
  return [world.guard, world.agent, world.approver, world.demo];
}

/** The demo account's sub-accounts and the NEAR each starts with
 * (TESTNET.md step 3: the token's 180 KB of code needs about 1.8). */
export function subAccounts(world: World): Array<{ id: string; near: string }> {
  return [
    { id: world.token, near: "3" },
    { id: world.api, near: "0.2" },
    { id: world.mallory, near: "0.2" },
  ];
}

export function tokenOf(world: World): { ft: string } {
  return { ft: world.token };
}

export function tokenInitArgs(world: World): object {
  return { owner_id: world.demo, total_supply: TOTAL_SUPPLY.toString() };
}

export function guardInitArgs(world: World): object {
  return {
    owner_id: world.approver,
    agent_id: world.agent,
    approver_id: world.approver,
    slot_ns: SLOT_NS,
  };
}

export function setTokenArgs(world: World): object {
  return { token: tokenOf(world), ...POLICY };
}

export function allowApiArgs(world: World): object {
  return { token: tokenOf(world), receiver_id: world.api, ft_call: false };
}

/** Who has to hold TST storage before money moves: the guard and both
 * receivers (mallory too, so a refusal is the guard's and not the token's). */
export function storageAccounts(world: World): string[] {
  return [world.guard, world.api, world.mallory];
}

/** `sha256sum`'s lines, by file name. */
export function parseSha256Sums(text: string): Map<string, string> {
  const sums = new Map<string, string>();
  for (const line of text.split("\n")) {
    if (line.trim() === "") continue;
    const match = /^([0-9a-f]{64}) [ *]\.?\/?(\S+)$/.exec(line);
    if (match === null) {
      throw new Error(`not a SHA256SUMS line: ${JSON.stringify(line)}`);
    }
    sums.set(match[2], match[1]);
  }
  return sums;
}

const BASE58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/** Base58 as NEAR (and Bitcoin) spell it: leading zero bytes become `1`s. */
export function base58(bytes: Uint8Array): string {
  let n = 0n;
  for (const byte of bytes) n = (n << 8n) | BigInt(byte);
  let out = "";
  while (n > 0n) {
    out = BASE58[Number(n % 58n)] + out;
    n /= 58n;
  }
  for (const byte of bytes) {
    if (byte !== 0) break;
    out = `1${out}`;
  }
  return out;
}

/** A code hash as NEAR's RPC reports it: base58 of the SHA-256. */
export function codeHashOf(hex: string): string {
  if (!/^[0-9a-f]{64}$/.test(hex)) {
    throw new Error(`not a SHA-256: ${JSON.stringify(hex)}`);
  }
  return base58(Buffer.from(hex, "hex"));
}

/** The policy the guard must hold once the deploy is done, compared field by
 * field with `get_token`'s answer. Returns what differs, empty if nothing. */
export function policyDiff(world: World, view: unknown): string[] {
  if (typeof view !== "object" || view === null) {
    return ["no policy for the token"];
  }
  const got = view as {
    policy?: Record<string, unknown>;
    receivers?: Array<{ account_id?: unknown; ft_call?: unknown }>;
  };
  const diffs: string[] = [];
  for (const [field, want] of Object.entries(POLICY)) {
    if (got.policy?.[field] !== want) {
      diffs.push(`${field}: ${JSON.stringify(got.policy?.[field])}, want ${want}`);
    }
  }
  const receivers = got.receivers ?? [];
  if (receivers.length !== 1 || receivers[0].account_id !== world.api || receivers[0].ft_call !== false) {
    diffs.push(`receivers: ${JSON.stringify(receivers)}, want only ${world.api} without ft_call`);
  }
  return diffs;
}

/** What scene 3 spends on testnet: 5 TST paid by the agent alone, then 60
 * TST held for a human and approved. */
export const SCENE3_AUTONOMOUS = 5n * TST;
export const SCENE3_APPROVED = 60n * TST;
/** NEAR an account needs above its storage cost for the scene's receipts: a
 * pending entry costs the guard about 0.015, `approve` the approver about
 * 0.01 in gas, a request the agent's key a few thousandths. */
export const MIN_SPARE_YOCTO = 10n ** 23n;

/** The testnet guard as `scripts/demo-scene.sh tidy` reads it, after pruning. */
export interface DemoState {
  paused: boolean;
  /** `remaining_budget` for TST: what the agent can still move alone today. */
  budget: bigint;
  /** The guard's TST balance. */
  guardTst: bigint;
  /** The guard's NEAR above what its storage costs. */
  guardSpare: bigint;
  /** What's left of the gas allowance of the agent's `request_payment` key on
   * this guard, "unlimited" for a key without one, null if there's no such key. */
  agentAllowance: bigint | "unlimited" | null;
  /** The approver's NEAR above what its storage costs. */
  approverSpare: bigint;
  /** Requests still pending. */
  pending: string[];
}

/** NEAR's storage price: 10^19 yoctoNEAR a byte (nearcore's
 * `storage_amount_per_byte`, unchanged since genesis on testnet). */
export const YOCTO_PER_BYTE = 10n ** 19n;

/** An account's NEAR above what its storage costs, from `view_account`. */
export function spareYocto(view: unknown): bigint {
  const v = view as { amount?: unknown; storage_usage?: unknown };
  if (typeof v?.amount !== "string" || !/^\d+$/.test(v.amount) || typeof v.storage_usage !== "number") {
    throw new Error("the RPC sent no account");
  }
  const spare = BigInt(v.amount) - BigInt(v.storage_usage) * YOCTO_PER_BYTE;
  return spare > 0n ? spare : 0n;
}

/** The allowance left on the agent's key that may call `request_payment` on
 * `guard` and nothing else, from `view_access_key_list`: "unlimited" for a key
 * without an allowance, null if there's no such key. A key for other methods
 * or another contract doesn't count. */
export function agentKeyAllowance(list: unknown, guard: string): bigint | "unlimited" | null {
  const keys = (list as { keys?: unknown })?.keys;
  if (!Array.isArray(keys)) {
    throw new Error("the RPC sent no key list");
  }
  for (const key of keys) {
    const call = (key as { access_key?: { permission?: { FunctionCall?: Record<string, unknown> } } })
      ?.access_key?.permission?.FunctionCall;
    if (call === undefined || call.receiver_id !== guard) continue;
    const methods = call.method_names;
    if (!Array.isArray(methods) || methods.length !== 1 || methods[0] !== "request_payment") continue;
    if (call.allowance === null || call.allowance === undefined) return "unlimited";
    if (typeof call.allowance !== "string" || !/^\d+$/.test(call.allowance)) {
      throw new Error("the RPC sent an allowance that isn't a number");
    }
    return BigInt(call.allowance);
  }
  return null;
}

/** Why scene 3 can't be recorded on this guard now; empty if it can. */
export function recordingProblems(s: DemoState): string[] {
  const problems: string[] = [];
  if (s.paused) problems.push("the guard is paused");
  if (s.budget < SCENE3_AUTONOMOUS) {
    problems.push("less than 5 TST of today's budget left, so the 5 TST payment would wait for a human");
  }
  if (s.guardTst < SCENE3_AUTONOMOUS + SCENE3_APPROVED) {
    problems.push("the guard holds less than 65 TST, the 5 paid plus the 60 approved");
  }
  if (s.guardSpare < MIN_SPARE_YOCTO) {
    problems.push("the guard has less than 0.1 NEAR above its storage, so a held request would be refused (insufficient_balance)");
  }
  if (s.agentAllowance === null) {
    problems.push("the agent has no request_payment key on this guard");
  } else if (s.agentAllowance !== "unlimited" && s.agentAllowance < MIN_SPARE_YOCTO) {
    problems.push("the agent's key has less than 0.1 NEAR of gas allowance left");
  }
  if (s.approverSpare < MIN_SPARE_YOCTO) {
    problems.push("the approver has less than 0.1 NEAR for the approval's gas");
  }
  if (s.pending.length > 0) {
    problems.push(`request${s.pending.length === 1 ? "" : "s"} ${s.pending.join(", ")} still pending: approve or reject, or wait for expiry and tidy again`);
  }
  return problems;
}

/** Creates a file that must not exist yet, with `mode` from the start, so a
 * secret is never readable by others, not even for a moment, and nothing
 * already there is replaced. */
export function createFile(path: string, contents: string, mode: number): void {
  const fd = openSync(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL, mode);
  try {
    writeSync(fd, contents);
  } finally {
    closeSync(fd);
  }
}

/** near CLI's legacy keychain: `~/.near-credentials/testnet/<account>.json`,
 * which `sign-with-legacy-keychain` reads. */
export function credentialJson(accountId: string, publicKey: string, secretKey: string): string {
  return `${JSON.stringify({ account_id: accountId, public_key: publicKey, private_key: secretKey })}\n`;
}

/** The `pay` server's settings, as scripts/local-agent.sh expects them. */
export function agentEnv(world: World, rpcUrl: string, secretKey: string): string {
  return `NSG_GUARD_ID=${world.guard}\nNSG_AGENT_ID=${world.agent}\nNSG_RPC_URL=${rpcUrl}\nNSG_AGENT_KEY=${secretKey}\n`;
}

export function mcpJson(agentEnvPath: string, payServer: string): string {
  return `${JSON.stringify(
    {
      mcpServers: {
        "near-spend-guard": {
          type: "stdio",
          command: "node",
          args: [`--env-file=${agentEnvPath}`, payServer],
        },
      },
    },
    null,
    2,
  )}\n`;
}

/** `nsg-approve`'s config: real near CLI, so no `near_cli` override, on the
 * same near CLI connection the deploy used. */
export function approverJson(world: World, connection: string, rpcUrl: string): string {
  return `${JSON.stringify({
    guard_id: world.guard,
    approver_id: world.approver,
    network: connection,
    rpc_url: rpcUrl,
  })}\n`;
}

export function worldJson(world: World, connection: string, rpcUrl: string): string {
  return `${JSON.stringify(
    {
      network: "testnet",
      near_cli_connection: connection,
      rpc_url: rpcUrl,
      guard: world.guard,
      token: world.token,
      agent: world.agent,
      approver: world.approver,
      owner: world.approver,
      allowed: [world.api],
      not_allowed: [world.mallory],
      api: world.api,
      mallory: world.mallory,
    },
    null,
    2,
  )}\n`;
}
