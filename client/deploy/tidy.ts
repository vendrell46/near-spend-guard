#!/usr/bin/env node
// Before recording, and after rehearsals: removes expired requests from the
// guard and lists what is still pending. An expired request keeps its place
// among the 16 pending ones until someone calls `prune_expired` (anyone may),
// so rehearsals that leave requests to expire would end in `too_many_pending`.
// Usage: node client/deploy/tidy.ts <dir>  (scripts/demo-scene.sh tidy)
//
// It signs as the demo account (the token's owner, the parent of `tst.`),
// whose key scripts/deploy-testnet.sh saved, never as the approver: pruning
// needs no authority, and the approver's key is the human's.

import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

import { Account, JsonRpcProvider, type KeyPairString, actions, teraToGas } from "near-api-js";

import { jsonRpc } from "../src/rpc.ts";

if (process.argv.length !== 3) {
  console.error("usage: node client/deploy/tidy.ts <dir>");
  process.exit(2);
}
const dir = resolve(process.argv[2]);
const world = JSON.parse(readFileSync(join(dir, "world.json"), "utf8")) as {
  rpc_url: string;
  guard: string;
  token: string;
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
}

main().catch((e) => {
  console.error(`tidy: ${(e as Error).message}`);
  process.exitCode = 1;
});
