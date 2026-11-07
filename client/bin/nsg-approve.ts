#!/usr/bin/env node
// The approver's helper. It reads ~/.config/near-spend-guard/approver.json, and
// nothing the agent prints can change the guard it talks to.

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";

import { approve, configPath, parseApproverConfig } from "../src/approve.ts";
import { plain } from "../src/display.ts";
import { jsonRpc } from "../src/rpc.ts";

function runNear(command: string, args: string[]): Promise<number> {
  return new Promise((resolve, reject) => {
    // No shell: `msg` can hold anything, and it's in these arguments.
    spawn(command, args, { stdio: "inherit" })
      .on("error", (e) => reject(new Error(`could not start near CLI: ${e.message}`)))
      .on("close", (code) => resolve(code ?? 1));
  });
}

try {
  const config = parseApproverConfig(readFileSync(configPath(process.env), "utf8"));
  process.exitCode = await approve(process.argv.slice(2), {
    config,
    rpc: jsonRpc(config.rpc_url),
    out: (line) => console.log(line),
    runNear,
  });
} catch (e) {
  console.error(`nsg-approve: ${plain((e as Error).message)}`);
  process.exitCode = 1;
}
