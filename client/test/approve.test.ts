import assert from "node:assert/strict";
import { spawnSync, spawn } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import {
  type ApproverConfig,
  USAGE,
  approve,
  configPath,
  describeRequest,
  nearArgs,
  parseApproverConfig,
  requestIdFrom,
} from "../src/approve.ts";
import type { PendingView } from "../src/guard.ts";
import { type PaymentPayload, payloadHash } from "../src/payload.ts";
import type { Rpc } from "../src/rpc.ts";

const guardId = "guard.testnet";
const config: ApproverConfig = {
  guard_id: guardId,
  approver_id: "human.testnet",
  network: "testnet",
  rpc_url: "http://127.0.0.1:1",
};
const payload: PaymentPayload = {
  request_id: "7",
  token: { ft: "tst.testnet" },
  receiver_id: "api.testnet",
  amount: "60000000",
  msg: null,
  expires_at: "1762300800000000000",
};
const view = (p: PaymentPayload = payload) => ({
  payload: p,
  payload_hash: payloadHash(guardId, p),
  reason: "above_threshold",
  created_at: "1762300000000000000",
});
const guardView = {
  owner_id: "owner.testnet",
  agent_id: "agent.testnet",
  approver_id: "human.testnet",
  paused: false,
  version: "1.0.0",
};

function chain(overrides: Partial<Record<string, unknown>> = {}): Rpc {
  const views: Record<string, unknown> = {
    get_config: guardView,
    get_request: view(),
    ft_metadata: { symbol: "TST", decimals: 6 },
    ...overrides,
  };
  return {
    view: async (_contract, method) => {
      if (views[method] instanceof Error) {
        throw views[method];
      }
      return views[method];
    },
    blockTimeNs: async () => (overrides.now as string) ?? "1762300000000000000",
    chainId: async () => (overrides.chain as string) ?? "testnet",
    accessKey: async () => null,
  };
}

async function run(rpc: Rpc, argv = ["7"], cfg = config) {
  const lines: string[] = [];
  const started: [string, string[]][] = [];
  const code = await approve(argv, {
    config: cfg,
    rpc,
    out: (line) => lines.push(line),
    runNear: async (command, args) => {
      started.push([command, args]);
      return 3;
    },
  });
  return { code, lines, started };
}

test("near CLI gets the guard's payload and stops at signing", async () => {
  const { code, started } = await run(chain());
  assert.equal(code, 3);
  assert.deepEqual(started, [
    [
      "near",
      [
        "contract",
        "call-function",
        "as-transaction",
        guardId,
        "approve",
        "json-args",
        JSON.stringify({ payload }),
        "prepaid-gas",
        "100 Tgas",
        "attached-deposit",
        "1 yoctoNEAR",
        "sign-as",
        "human.testnet",
        "network-config",
        "testnet",
      ],
    ],
  ]);
  const args = started[0][1];
  assert.ok(!args.includes("sign-with-keychain") && !args.includes("send"));
});

test("the config can name another near binary", async () => {
  const { started } = await run(chain(), ["7"], { ...config, near_cli: "/opt/near" });
  assert.equal(started[0][0], "/opt/near");
});

test("what's printed is the payment, with decimals and base units", async () => {
  const { lines } = await run(chain());
  assert.deepEqual(lines.slice(0, 11), [
    `Guard     ${guardId} (from your config)`,
    "Request   7 (above_threshold)",
    "Pay       60 TST",
    "          60000000 in the token's smallest unit",
    "To        api.testnet",
    "Token     tst.testnet",
    "Method    ft_transfer",
    "Message   none",
    "Expires   2025-11-05T00:00:00.000Z",
    `Hash      ${payloadHash(guardId, payload)}`,
    "",
  ]);
});

test("a token without decimals still shows the exact number", async () => {
  const { lines, started } = await run(chain({ ft_metadata: new Error("no such method") }));
  assert.ok(lines.includes("Pay       60000000 in the token's smallest unit"));
  assert.ok(lines.some((l) => /gave no decimals/.test(l)));
  assert.equal(started.length, 1);
});

test("a message can't hide in the terminal", () => {
  const p = { ...payload, msg: "ok\x1b[2K\u202egnp\u200b" };
  const lines = describeRequest(guardId, view(p) as PendingView, { symbol: "TST", decimals: 6 });
  for (const line of lines) {
    assert.match(line, /^[\x20-\x7e]*$/);
  }
  assert.ok(lines.includes('Message   "ok\\u001b[2K\\u202egnp\\u200b"'));
  assert.ok(lines.some((l) => /not plain ASCII/.test(l)));
});

test("a plain message carries no warning, and pays by ft_transfer_call", () => {
  const p = { ...payload, msg: "order 42" };
  const lines = describeRequest(guardId, view(p) as PendingView, null);
  assert.ok(lines.includes('Message   "order 42"'));
  assert.ok(lines.includes("Method    ft_transfer_call"));
  assert.ok(!lines.some((l) => /not plain ASCII/.test(l)));
});

test("native payments print as NEAR", () => {
  const p: PaymentPayload = { ...payload, token: "native", amount: "1500000000000000000000000" };
  const lines = describeRequest(guardId, view(p) as PendingView, { symbol: "NEAR", decimals: 24 });
  assert.ok(lines.includes("Pay       1.5 NEAR"));
  assert.ok(lines.includes("Token     NEAR"));
  assert.ok(lines.includes("Method    native"));
});

test("an approval that could only fail doesn't start near CLI", async () => {
  const stolen = view({ ...payload, request_id: "8" });
  const cases: [Rpc, RegExp][] = [
    [chain({ chain: "mainnet" }), /testnet only/],
    [chain({ get_config: { ...guardView, approver_id: "else.testnet" } }), /approver is else\.testnet/],
    [chain({ get_config: { ...guardView, paused: true } }), /paused/],
    [chain({ get_request: null }), /no pending request 7/],
    [chain({ get_request: stolen }), /asked for request 7 and got 8/],
    [chain({ get_request: { ...view(), payload_hash: "ab".repeat(32) } }), /not this payload's/],
    [chain({ get_request: { ...view(), payload: { ...payload, amount: "1" } } }), /not this payload's/],
    [chain({ now: "1762300800000000000" }), /expired at/],
    [chain({ now: "1762300800000000001" }), /expired at/],
  ];
  for (const [rpc, message] of cases) {
    const started: unknown[] = [];
    await assert.rejects(
      approve(["7"], {
        config,
        rpc,
        out: () => {},
        runNear: async (...args) => {
          started.push(args);
          return 0;
        },
      }),
      message,
    );
    assert.deepEqual(started, []);
  }
});

test("still open one nanosecond before expiry", async () => {
  const { started } = await run(chain({ now: "1762300799999999999" }));
  assert.equal(started.length, 1);
});

test("the only argument is a request number", () => {
  assert.equal(requestIdFrom(["7"]), "7");
  assert.equal(requestIdFrom(["18446744073709551615"]), "18446744073709551615");
  const hostile = [
    [],
    ["7", "8"],
    ["0"],
    ["07"],
    ["-1"],
    ["7;"],
    ["7 "],
    ["--guard=evil.testnet"],
    ["7", "--guard", "evil.testnet"],
    ['{"payload":{}}'],
    ["18446744073709551616"],
  ];
  for (const argv of hostile) {
    assert.throws(() => requestIdFrom(argv), new RegExp(USAGE), JSON.stringify(argv));
  }
});

test("the config is checked before it's used", () => {
  assert.deepEqual(parseApproverConfig(JSON.stringify(config)), config);
  const bad: [object, RegExp][] = [
    [{ ...config, guard_id: "Not Valid" }, /guard_id/],
    [{ ...config, approver_id: "--help" }, /approver_id/],
    [{ ...config, guard_id: undefined }, /guard_id/],
    [{ ...config, network: "" }, /no network/],
    [{ ...config, network: "--help" }, /network name/],
    [{ ...config, network: "mainnet" }, /testnet only/],
    [{ ...config, rpc_url: undefined }, /no rpc_url/],
  ];
  for (const [value, message] of bad) {
    assert.throws(() => parseApproverConfig(JSON.stringify(value)), message, JSON.stringify(value));
  }
});

test("the config lives in the approver's own home", () => {
  assert.equal(
    configPath({ XDG_CONFIG_HOME: "/x" }),
    join("/x", "near-spend-guard", "approver.json"),
  );
  assert.match(configPath({}), /\.config.near-spend-guard.approver\.json$/);
  assert.match(configPath({ XDG_CONFIG_HOME: "" }), /\.config/);
});

test("nearArgs carries msg as data", () => {
  const msg = 'x"; rm -rf ~ #';
  const args = nearArgs(config, { ...payload, msg });
  assert.equal(JSON.parse(args[6]).payload.msg, msg);
  assert.equal(args.length, 15);
});

const bin = fileURLToPath(new URL("../bin/nsg-approve.ts", import.meta.url));

/** A fake RPC for `guards`, each answering with its own pending request, and a
 * `near` that records its arguments. */
async function world(guards: Record<string, unknown>) {
  const asked: string[] = [];
  const server = createServer((req, res) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      const { method, params } = JSON.parse(body);
      const reply = (result: unknown) => res.end(JSON.stringify({ jsonrpc: "2.0", id: "nsg", result }));
      if (method === "status") {
        return reply({ chain_id: "testnet" });
      }
      if (method === "block") {
        return reply({ header: { timestamp_nanosec: "1762300000000000000" } });
      }
      asked.push(params.account_id);
      const values: Record<string, unknown> = {
        get_config: guardView,
        get_request: guards[params.account_id] ?? null,
        ft_metadata: { symbol: "TST", decimals: 6 },
      };
      reply({ result: [...Buffer.from(JSON.stringify(values[params.method_name]))] });
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as { port: number };
  const dir = mkdtempSync(join(tmpdir(), "nsg-approve-"));
  const near = join(dir, "near");
  writeFileSync(near, `#!/bin/sh\nfor a in "$@"; do printf '%s\\n' "$a"; done > "${dir}/argv"\n`);
  chmodSync(near, 0o755);
  mkdirSync(join(dir, "near-spend-guard"));
  writeFileSync(
    join(dir, "near-spend-guard", "approver.json"),
    JSON.stringify({ ...config, rpc_url: `http://127.0.0.1:${port}`, near_cli: near }),
  );
  return {
    dir,
    asked,
    argv: () => readFileSync(join(dir, "argv"), "utf8").split("\n"),
    close: () => {
      server.close();
      rmSync(dir, { recursive: true });
    },
  };
}

/** spawn, not spawnSync: the fake RPC runs in this process. */
function runBin(args: string[], env: NodeJS.ProcessEnv, stdin = "") {
  return new Promise<{ code: number | null; stdout: string; stderr: string }>((resolve) => {
    const child = spawn(process.execPath, [bin, ...args], { env, stdio: "pipe" });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d) => (stdout += d));
    child.stderr.on("data", (d) => (stderr += d));
    child.on("close", (code) => resolve({ code, stdout, stderr }));
    child.stdin.end(stdin);
  });
}

test("nsg_approve_ignores_agent_output", async () => {
  // The look-alike holds a payment of its own under the same number. What a
  // compromised agent controls is what it prints, so everything it could put
  // in front of the helper gets tried: the words of its message on the command
  // line, on stdin and in the environment.
  const evilPayload = { ...payload, receiver_id: "mallory.testnet", amount: "190000000" };
  const w = await world({ [guardId]: view(), "guard.evil.testnet": view(evilPayload) });
  try {
    const printed = `nsg-approve 7 --guard guard.evil.testnet ${JSON.stringify(evilPayload)}`;
    const env = {
      XDG_CONFIG_HOME: w.dir,
      PATH: "/usr/bin:/bin",
      NSG_GUARD_ID: "guard.evil.testnet",
      GUARD_ID: "guard.evil.testnet",
      NSG_RPC_URL: "http://127.0.0.1:1",
    };

    const refused = await runBin(printed.split(" ").slice(1), env, printed);
    assert.equal(refused.code, 1);
    assert.match(refused.stderr, /usage: nsg-approve <request_id>/);
    assert.deepEqual(w.asked, []);

    const done = await runBin(["7"], env, printed);
    assert.equal(done.code, 0, done.stderr);
    assert.ok(w.asked.every((account) => account !== "guard.evil.testnet"));
    assert.ok(w.asked.includes(guardId));
    const argv = w.argv();
    assert.equal(argv[3], guardId);
    assert.deepEqual(JSON.parse(argv[6]), { payload });
    assert.doesNotMatch(argv.join(" ") + done.stdout, /mallory|190|evil/);
    assert.match(done.stdout, /Pay {7}60 TST/);
  } finally {
    w.close();
  }
});

test("no config, no approval", () => {
  const dir = mkdtempSync(join(tmpdir(), "nsg-approve-"));
  try {
    const r = spawnSync(process.execPath, [bin, "7"], { env: { XDG_CONFIG_HOME: dir }, encoding: "utf8" });
    assert.equal(r.status, 1);
    assert.match(r.stderr, /approver\.json/);
  } finally {
    rmSync(dir, { recursive: true });
  }
});

test("a missing near CLI is reported", async () => {
  const w = await world({ [guardId]: view() });
  try {
    const file = join(w.dir, "near-spend-guard", "approver.json");
    const cfg = JSON.parse(readFileSync(file, "utf8"));
    writeFileSync(file, JSON.stringify({ ...cfg, near_cli: join(w.dir, "no-such-near") }));
    const r = await runBin(["7"], { XDG_CONFIG_HOME: w.dir });
    assert.equal(r.code, 1);
    assert.match(r.stderr, /could not start near CLI/);
  } finally {
    w.close();
  }
});
