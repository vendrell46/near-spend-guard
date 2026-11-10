#!/usr/bin/env node
// DEMO.md scene 3 without a model: speaks MCP to `nsg-pay-mcp` as a host does
// and makes the scene's three calls against the world scripts/deploy-testnet.sh
// built (or scripts/local-node.sh's, same files). Usage:
//
//   node client/deploy/scene3.ts <dir>
//
// 1. 5 TST to the API provider: paid.
// 2. 500 TST to mallory: refused, receiver_not_allowed.
// 3. 60 TST to the API provider: pending, above_threshold, expiring in 15
//    minutes so a rehearsal leaves nothing behind.
//
// Each answer is printed as the host would show it, and the run fails unless
// all three come back as above. The agent's key stays in <dir>/agent.env,
// which only the server reads (`node --env-file`), as in mcp.json.

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVER = resolve(HERE, "../bin/nsg-pay-mcp.ts");

if (process.argv.length !== 3) {
  console.error("usage: node client/deploy/scene3.ts <dir>");
  process.exit(2);
}
const dir = resolve(process.argv[2]);
const world = JSON.parse(readFileSync(join(dir, "world.json"), "utf8")) as {
  token: string;
  api: string;
  mallory: string;
  approver: string;
};

const CALLS = [
  { amount: "5", receiver: world.api, want: /^paid: 5 TST to /, say: "paid" },
  { amount: "500", receiver: world.mallory, want: /^refused \(receiver_not_allowed\)/, say: "refused" },
  { amount: "60", receiver: world.api, want: /^pending \(above_threshold\): request \d+ /, say: "pending" },
];

const server = spawn(process.execPath, [`--env-file=${join(dir, "agent.env")}`, SERVER], {
  stdio: ["pipe", "pipe", "inherit"],
});
const answers = new Map<number, (message: Record<string, unknown>) => void>();
createInterface({ input: server.stdout }).on("line", (line) => {
  const message = JSON.parse(line) as { id?: number };
  if (typeof message.id === "number") answers.get(message.id)?.(message);
});
// A server that stops (a key that can do more, a wrong guard) answers nothing
// more, so every question still open fails instead of waiting forever.
let exited: string | null = null;
server.on("exit", (code, signal) => {
  exited = `the pay server stopped (${signal ?? `exit ${code}`})`;
  for (const done of answers.values()) done({ error: exited });
  answers.clear();
});
let next = 1;
function request(method: string, params: object): Promise<Record<string, unknown>> {
  const id = next++;
  if (exited !== null) return Promise.resolve({ error: exited });
  return new Promise((done) => {
    answers.set(id, (message) => {
      answers.delete(id);
      done(message);
    });
    server.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
  });
}

async function main(): Promise<number> {
  const hello = await request("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "nsg-scene3", version: "0.1.0" },
  });
  if (hello.error !== undefined) throw new Error(`initialize: ${JSON.stringify(hello.error)}`);
  server.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" })}\n`);

  let failures = 0;
  let pending: string | null = null;
  for (const call of CALLS) {
    const args: Record<string, unknown> = {
      token: { ft: world.token },
      receiver_id: call.receiver,
      amount: call.amount,
    };
    if (call.say === "pending") args.expires_in_s = 900;
    console.log(`\n> pay ${call.amount} TST to ${call.receiver}`);
    const answer = await request("tools/call", { name: "pay", arguments: args });
    if (answer.error !== undefined) throw new Error(String(answer.error));
    const result = answer.result as { content?: Array<{ text?: string }> } | undefined;
    const text = result?.content?.[0]?.text ?? JSON.stringify(answer);
    console.log(text);
    if (!call.want.test(text)) {
      console.log(`  NOT ${call.say}`);
      failures++;
    }
    const id = /^pending \(above_threshold\): request (\d+) /.exec(text)?.[1];
    if (id !== undefined) pending = id;
  }
  if (pending !== null) {
    console.log(`\nThe approver, in their own terminal:`);
    console.log(`  XDG_CONFIG_HOME=${join(dir, "config")} node client/bin/nsg-approve.ts ${pending}`);
  }
  console.log(failures === 0 ? "\nscene 3: paid, refused, pending, as DEMO.md has it" : `\nscene 3: ${failures} of 3 not as expected`);
  return failures === 0 ? 0 : 1;
}

main()
  .then((code) => {
    process.exitCode = code;
  })
  .catch((e) => {
    console.error(`scene3: ${(e as Error).message}`);
    process.exitCode = 1;
  })
  .finally(() => server.stdin.end());
