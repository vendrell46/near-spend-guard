#!/usr/bin/env node
// The `pay` MCP server, over stdio. Settings come from the environment, and it
// won't start unless the key can do nothing but `request_payment` on the guard.

import { plain } from "../src/display.ts";
import { serve } from "../src/mcp.ts";
import { nearSender } from "../src/near.ts";
import { checkAgent, parsePayInput, pay } from "../src/pay.ts";
import { jsonRpc, refuseMainnet } from "../src/rpc.ts";

function setting(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} is not set`);
  }
  return value;
}

try {
  const guardId = setting("NSG_GUARD_ID");
  const agentId = setting("NSG_AGENT_ID");
  const rpcUrl = setting("NSG_RPC_URL");
  const secretKey = setting("NSG_AGENT_KEY");
  delete process.env.NSG_AGENT_KEY;

  const rpc = jsonRpc(rpcUrl);
  const { sender, publicKey } = nearSender({ guardId, agentId, rpcUrl, secretKey });
  await refuseMainnet(rpc);
  await checkAgent(rpc, guardId, agentId, await publicKey());
  await serve(process.stdin, process.stdout, (args) =>
    pay(parsePayInput(args), { guardId, rpc, sender }),
  );
} catch (e) {
  // Stdout is the protocol's.
  console.error(`nsg-pay-mcp: ${plain((e as Error).message)}`);
  process.exitCode = 1;
}
