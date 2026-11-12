#!/usr/bin/env node
// Before recording, and after rehearsals: removes expired requests from the
// guard, lists what is still pending, and says whether scene 3 can be recorded
// now (recordingProblems in world.ts): the guard not paused, 5 TST of today's
// budget and 65 TST in the guard, NEAR for the receipts, the agent's key, and
// nothing left pending. Exits 1 unless it ends with "ready to record".
// Usage: node client/deploy/tidy.ts <dir>  (scripts/demo-scene.sh tidy)
//
// It signs as the demo account (the token's owner, the parent of `tst.`),
// whose key scripts/deploy-testnet.sh saved, never as the approver: pruning
// needs no authority, and the approver's key is the human's.

import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

import { Account, JsonRpcProvider, type KeyPairString, actions, teraToGas } from "near-api-js";

import { NEAR_DECIMALS, formatAmount } from "../src/amount.ts";
import { jsonRpc } from "../src/rpc.ts";
import { agentKeyAllowance, recordingProblems, spareYocto } from "./world.ts";

if (process.argv.length !== 3) {
  console.error("usage: node client/deploy/tidy.ts <dir>");
  process.exit(2);
}
const dir = resolve(process.argv[2]);
const world = JSON.parse(readFileSync(join(dir, "world.json"), "utf8")) as {
  rpc_url: string;
  guard: string;
  token: string;
  agent: string;
  approver: string;
};
const demo = world.token.split(".").slice(1).join(".");
const rpc = jsonRpc(world.rpc_url, { timeoutMs: 30_000 });

type Pending = { payload: { request_id: string; amount: string; receiver_id: string; expires_at: string } };

async function main(): Promise<void> {
  if ((await rpc.chainId()) !== "testnet") throw new Error(`${world.rpc_url} isn't testnet`);
  const now = BigInt(await rpc.blockTimeNs());
  const before = (await rpc.view(world.guard, "get_pending", {})) as Pending[];
  const expired = before.filter((p) => BigInt(p.payload.expires_at) <= now).length;
  if (expired > 0) {
    const saved = JSON.parse(
      readFileSync(join(homedir(), ".near-credentials/testnet", `${demo}.json`), "utf8"),
    ) as { private_key: string };
    const provider = new JsonRpcProvider({ url: world.rpc_url });
    const account = new Account(demo, provider, saved.private_key as KeyPairString);
    const outcome = await account.signAndSendTransaction({
      receiverId: world.guard,
      actions: [actions.functionCall("prune_expired", {}, teraToGas(50), 0n)],
      waitUntil: "FINAL",
      throwOnFailure: true,
    });
    console.log(`pruned ${expired} expired: https://testnet.nearblocks.io/txns/${outcome.transaction_outcome.id}`);
  } else {
    console.log("nothing expired");
  }
  const after = (await rpc.view(world.guard, "get_pending", {})) as Pending[];
  console.log(after.length === 0 ? "nothing pending" : `${after.length} still pending:`);
  for (const p of after) {
    console.log(`  request ${p.payload.request_id}: ${p.payload.amount} to ${p.payload.receiver_id}, expires ${new Date(Number(BigInt(p.payload.expires_at) / 1_000_000n)).toISOString()}`);
  }

  const reader = new JsonRpcProvider({ url: world.rpc_url });
  const query = (request_type: string, account_id: string) =>
    reader.query({ request_type, account_id, finality: "final" } as never) as Promise<unknown>;
  const token = { ft: world.token };
  const config = (await rpc.view(world.guard, "get_config", {})) as { paused: boolean };
  const state = {
    paused: config.paused === true,
    budget: BigInt((await rpc.view(world.guard, "remaining_budget", { token })) as string),
    guardTst: BigInt((await rpc.view(world.token, "ft_balance_of", { account_id: world.guard })) as string),
    guardSpare: spareYocto(await query("view_account", world.guard)),
    agentAllowance: agentKeyAllowance(await query("view_access_key_list", world.agent), world.guard),
    approverSpare: spareYocto(await query("view_account", world.approver)),
    pending: after.map((p) => p.payload.request_id),
  };
  const tst = (v: bigint) => `${formatAmount(v.toString(), 6)} TST`;
  const near = (v: bigint) => `${formatAmount(((v / 10n ** 21n) * 10n ** 21n).toString(), NEAR_DECIMALS)} NEAR`;
  console.log(`paused          ${state.paused ? "yes" : "no"}`);
  console.log(`budget left     ${tst(state.budget)} today without a human`);
  console.log(`guard holds     ${tst(state.guardTst)}, ${near(state.guardSpare)} above its storage`);
  const allowance = state.agentAllowance;
  console.log(`agent's key     ${allowance === null ? "none for request_payment on this guard" : allowance === "unlimited" ? "no gas allowance limit" : `${near(allowance)} of gas allowance left`}`);
  console.log(`approver        ${near(state.approverSpare)} above its storage`);
  const problems = recordingProblems(state);
  if (problems.length === 0) {
    console.log("ready to record");
  } else {
    for (const problem of problems) console.log(`not ready: ${problem}`);
    process.exitCode = 1;
  }
}

main().catch((e) => {
  console.error(`tidy: ${(e as Error).message}`);
  process.exitCode = 1;
});
