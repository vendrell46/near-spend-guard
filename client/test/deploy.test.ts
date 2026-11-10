import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import {
  POLICY,
  agentEnv,
  allowApiArgs,
  approverJson,
  checkPrefix,
  codeHashOf,
  createFile,
  credentialJson,
  faucetAccounts,
  guardInitArgs,
  mcpJson,
  parseSha256Sums,
  policyDiff,
  setTokenArgs,
  storageAccounts,
  subAccounts,
  tokenInitArgs,
  worldOf,
} from "../deploy/world.ts";
import { parseApproverConfig } from "../src/approve.ts";

const world = worldOf("nsg-t");

test("the demo world's names, as TESTNET.md lists them", () => {
  assert.deepEqual(faucetAccounts(world), [
    "nsg-t-guard.testnet",
    "nsg-t-agent.testnet",
    "nsg-t-approver.testnet",
    "nsg-t-demo.testnet",
  ]);
  assert.deepEqual(
    subAccounts(world).map((s) => `${s.id} ${s.near}`),
    ["tst.nsg-t-demo.testnet 3", "api.nsg-t-demo.testnet 0.2", "mallory.nsg-t-demo.testnet 0.2"],
  );
  assert.deepEqual(storageAccounts(world), [
    "nsg-t-guard.testnet",
    "api.nsg-t-demo.testnet",
    "mallory.nsg-t-demo.testnet",
  ]);
});

test("a prefix has to make valid testnet names", () => {
  for (const ok of ["nsg", "nsg-1110", "a1-b2"]) assert.equal(checkPrefix(ok), ok);
  for (const bad of ["", "NSG", "nsg.", "nsg_x", "-nsg", "nsg-", "nsg--x", "a.b", "x".repeat(41)]) {
    assert.throws(() => checkPrefix(bad), /not a usable prefix/, bad);
  }
});

test("the set-up calls' arguments, as the sandbox's and TESTNET.md's", () => {
  assert.deepEqual(tokenInitArgs(world), { owner_id: "nsg-t-demo.testnet", total_supply: "1000000000000" });
  assert.deepEqual(guardInitArgs(world), {
    owner_id: "nsg-t-approver.testnet",
    agent_id: "nsg-t-agent.testnet",
    approver_id: "nsg-t-approver.testnet",
    slot_ns: "3600000000000",
  });
  assert.deepEqual(setTokenArgs(world), {
    token: { ft: "tst.nsg-t-demo.testnet" },
    per_payment_max: "200000000",
    daily_max: "100000000",
    approval_above: "40000000",
  });
  assert.deepEqual(allowApiArgs(world), {
    token: { ft: "tst.nsg-t-demo.testnet" },
    receiver_id: "api.nsg-t-demo.testnet",
    ft_call: false,
  });
  // The agent can be neither owner nor approver (INTERFACE.md).
  const init = guardInitArgs(world) as Record<string, string>;
  assert.notEqual(init.agent_id, init.owner_id);
  assert.notEqual(init.agent_id, init.approver_id);
});

test("code hashes in base58, as cargo-near printed them on 10 Nov", () => {
  const sums = parseSha256Sums(
    "cb5cc36f0da454eca0b3e477f8424ec2fd077c4679dcb6a7981acbf2c0bccfc5  ./naive_guard.wasm\n" +
      "23221f8a2835c3794041ad13955ae05a525b1e54cb50b8e9dd7d980f0c9a452b  ./near_spend_guard.wasm\n" +
      "da27d2dc1f0ca8ee370533f187530feda067c1dd1ba911525869100d5297550e  ./test_token.wasm\n",
  );
  assert.equal(codeHashOf(sums.get("near_spend_guard.wasm")!), "3N9SzbHvuQGebXhuyb5bVwJW1fPbJSjvyQWdcQ97bmza");
  assert.equal(codeHashOf(sums.get("test_token.wasm")!), "Fgb6huvCL7itGnABU89XMnN4YniTfNgtQbiwVrTNjLJh");
  assert.equal(codeHashOf(sums.get("naive_guard.wasm")!), "EgqoaZFczWwwS7d8UgWw6ooBTGfvHmy46otmfvucAH1n");
  assert.equal(sums.size, 3);
  // An account with no code: NEAR's RPC reports 32 zero bytes as 32 ones.
  assert.equal(codeHashOf("0".repeat(64)), "1".repeat(32));
  assert.equal(codeHashOf(`00${"0".repeat(60)}ff`).slice(0, 30), "1".repeat(30));
  assert.throws(() => parseSha256Sums("23221f8a  ./x.wasm\n"), /not a SHA256SUMS line/);
  assert.throws(() => codeHashOf("23221F8A2835C3794041AD13955AE05A525B1E54CB50B8E9DD7D980F0C9A452B"), /not a SHA-256/);
});

const good = {
  token: { ft: world.token },
  policy: { ...POLICY },
  receivers: [{ account_id: world.api, ft_call: false }],
  autonomous_spent: "0",
};

test("the policy check passes only the demo's policy and allowlist", () => {
  assert.deepEqual(policyDiff(world, good), []);
  assert.deepEqual(policyDiff(world, null), ["no policy for the token"]);
  for (const field of Object.keys(POLICY)) {
    const view = { ...good, policy: { ...POLICY, [field]: "1" } };
    assert.equal(policyDiff(world, view).length, 1, field);
  }
  const extra = { ...good, receivers: [...good.receivers, { account_id: world.mallory, ft_call: false }] };
  assert.equal(policyDiff(world, extra).length, 1);
  const ftCall = { ...good, receivers: [{ account_id: world.api, ft_call: true }] };
  assert.equal(policyDiff(world, ftCall).length, 1);
  const other = { ...good, receivers: [{ account_id: world.mallory, ft_call: false }] };
  assert.equal(policyDiff(world, other).length, 1);
  assert.equal(policyDiff(world, { ...good, receivers: [] }).length, 1);
});

test("a secret file is created 0600 and never replaces anything", () => {
  const dir = mkdtempSync(join(tmpdir(), "nsg-deploy-"));
  try {
    const path = join(dir, "agent.env");
    createFile(path, "NSG_AGENT_KEY=ed25519:x\n", 0o600);
    assert.equal(statSync(path).mode & 0o777, 0o600);
    assert.throws(() => createFile(path, "other\n", 0o600), /EEXIST/);
    assert.equal(readFileSync(path, "utf8"), "NSG_AGENT_KEY=ed25519:x\n");
    const taken = join(dir, "taken");
    writeFileSync(taken, "mine\n");
    assert.throws(() => createFile(taken, "theirs\n", 0o644), /EEXIST/);
    assert.equal(readFileSync(taken, "utf8"), "mine\n");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the files the deploy writes", () => {
  assert.deepEqual(JSON.parse(credentialJson("a.testnet", "ed25519:P", "ed25519:S")), {
    account_id: "a.testnet",
    public_key: "ed25519:P",
    private_key: "ed25519:S",
  });
  assert.equal(
    agentEnv(world, "https://rpc.example/", "ed25519:S"),
    "NSG_GUARD_ID=nsg-t-guard.testnet\nNSG_AGENT_ID=nsg-t-agent.testnet\n" +
      "NSG_RPC_URL=https://rpc.example/\nNSG_AGENT_KEY=ed25519:S\n",
  );
  const mcp = JSON.parse(mcpJson("/d/agent.env", "/r/client/bin/nsg-pay-mcp.ts"));
  assert.deepEqual(mcp.mcpServers["near-spend-guard"].args, ["--env-file=/d/agent.env", "/r/client/bin/nsg-pay-mcp.ts"]);
  assert.equal(JSON.stringify(mcp).includes("ed25519"), false);
  // nsg-approve takes the config as written: testnet, the real near CLI.
  const config = parseApproverConfig(approverJson(world, "testnet-fastnear", "https://rpc.example/"));
  assert.equal(config.network, "testnet-fastnear");
  assert.equal(config.rpc_url, "https://rpc.example/");
  assert.equal(config.guard_id, world.guard);
  assert.equal(config.approver_id, world.approver);
  assert.equal(config.near_cli, undefined);
});
