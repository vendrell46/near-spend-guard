#!/usr/bin/env node
// Deploys the demo world to testnet: TESTNET.md steps 2 to 11, in the order of
// the sandbox tests' set-up. Usage (scripts/deploy-testnet.sh runs it):
//
//   node client/deploy/testnet.ts <prefix> [<dir>]
//
// Every step reads the chain first and only acts if its result isn't there
// yet, so running it again finishes a deploy that stopped half-way and sends
// nothing when everything is in place. A key is written to disk before the
// transaction that puts it on chain, so a crash can't leave an account
// nobody holds a key for.
//
// - Full-access keys: `~/.near-credentials/testnet/<account>.json` (mode
//   0600), near CLI's legacy keychain, so the approver can sign with
//   `sign-with-legacy-keychain`. The agent's full-access key goes there too
//   and is never given to the agent.
// - `<dir>` (default `~/.config/near-spend-guard/testnet`, mode 0700): the
//   agent's function-call key in `agent.env` (0600) and `mcp.json`, so
//   scripts/local-agent.sh runs unchanged against testnet; `world.json`; and
//   `config/near-spend-guard/approver.json` for `nsg-approve`.
//
// No secret is printed. Testnet only: it stops unless the RPC says testnet.

import { existsSync, mkdirSync, readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  Account,
  JsonRpcProvider,
  KeyPair,
  type KeyPairString,
  actions,
  nearToYocto,
  teraToGas,
} from "near-api-js";

import { jsonRpc } from "../src/rpc.ts";
import {
  AGENT_KEY_ALLOWANCE_NEAR,
  GUARD_FUNDING,
  TST,
  type World,
  agentEnv,
  allowApiArgs,
  approverJson,
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
  tokenOf,
  worldJson,
  worldOf,
} from "./world.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, "../..");
const NO_CODE = "1".repeat(32);
const ONE_YOCTO = 1n;
const EXPLORER = "https://testnet.nearblocks.io/txns/";

function usage(): never {
  console.error("usage: node client/deploy/testnet.ts [--connection <near CLI connection>] <prefix> [<dir>]");
  process.exit(2);
}

/** One of near CLI's own connections (TESTNET.md step 11: read it, don't
 * assume it). The default is `testnet-fastnear`: on 10 Nov near CLI's
 * `testnet` connection, fastnear's archival RPC, answered 429 to `send_tx`
 * after a dozen transactions while still answering reads. */
function nearCliConnection(name: string): { rpcUrl: string; faucetUrl: string } {
  const path =
    process.platform === "darwin"
      ? join(homedir(), "Library/Application Support/near-cli/config.toml")
      : join(process.env.XDG_CONFIG_HOME || join(homedir(), ".config"), "near-cli/config.toml");
  const text = readFileSync(path, "utf8");
  const header = `[network_connection.${name}]\n`;
  const start = text.indexOf(header);
  const rest = start < 0 ? "" : text.slice(start + header.length);
  const section = rest.split(/\n\[/)[0];
  if (!/^network_name = "testnet"$/m.test(section)) {
    throw new Error(`${path} has no testnet connection named ${name}`);
  }
  const rpcUrl = /^rpc_url = "([^"]+)"$/m.exec(section)?.[1];
  const faucetUrl = /^faucet_url = "([^"]+)"$/m.exec(section)?.[1];
  if (!rpcUrl || !faucetUrl) {
    throw new Error(`${path}: ${name} has no rpc_url or faucet_url`);
  }
  return { rpcUrl, faucetUrl };
}

const args = process.argv.slice(2);
let connection = "testnet-fastnear";
if (args[0] === "--connection") {
  if (args.length < 2 || !/^[a-z0-9-]+$/.test(args[1])) usage();
  connection = args[1];
  args.splice(0, 2);
}
if (args.length < 1 || args.length > 2) usage();
const world: World = worldOf(args[0]);
const dir = resolve(args[1] ?? join(homedir(), ".config/near-spend-guard/testnet"));
const credentials = join(homedir(), ".near-credentials/testnet");
const { rpcUrl, faucetUrl } = nearCliConnection(connection);
const provider = new JsonRpcProvider({ url: rpcUrl });
const rpc = jsonRpc(rpcUrl, { timeoutMs: 30_000 });
const sums = parseSha256Sums(readFileSync(join(REPO, "target/deploy/SHA256SUMS"), "utf8"));

function wasm(name: string): { code: Uint8Array; hash: string } {
  const hex = sums.get(name);
  if (hex === undefined) throw new Error(`target/deploy/SHA256SUMS lists no ${name}`);
  return { code: readFileSync(join(REPO, "target/deploy", name)), hash: codeHashOf(hex) };
}

function log(line: string): void {
  console.log(line);
}

function privateDir(path: string): void {
  mkdirSync(path, { recursive: true, mode: 0o700 });
  if ((statSync(path).mode & 0o077) !== 0) {
    throw new Error(`${path} can be read by others; make it 0700 first`);
  }
}

/** Reads again when the free RPC says it's busy (fastnear answers 429 to a
 * burst of reads, seen on the first run, 10 Nov). Writes are never retried
 * here: a second send could pay twice. */
async function retrying<T>(read: () => Promise<T>): Promise<T> {
  for (let attempt = 1; ; attempt++) {
    try {
      return await read();
    } catch (e) {
      if (attempt >= 6 || !/429|Rate limits/i.test((e as Error).message)) throw e;
      await new Promise((r) => setTimeout(r, 2_000 * attempt));
    }
  }
}

function view(contractId: string, method: string, args: object): Promise<unknown> {
  return retrying(() => rpc.view(contractId, method, args));
}

function accessKey(accountId: string, publicKey: string): Promise<unknown> {
  return retrying(() => rpc.accessKey(accountId, publicKey));
}

async function accountView(accountId: string): Promise<{ amount: bigint; code_hash: string } | null> {
  try {
    const view = await retrying(() => provider.viewAccount({ accountId }));
    return { amount: BigInt(view.amount), code_hash: String(view.code_hash) };
  } catch (e) {
    if (/does not exist|UNKNOWN_ACCOUNT|UnknownAccount/i.test((e as Error).message)) return null;
    throw e;
  }
}

function credentialPath(accountId: string): string {
  return join(credentials, `${accountId}.json`);
}

/** The account's full-access key from the legacy keychain, made first if
 * there is none. */
function keyFor(accountId: string): KeyPair {
  const path = credentialPath(accountId);
  if (existsSync(path)) {
    const saved = JSON.parse(readFileSync(path, "utf8")) as { account_id: string; private_key: string };
    if (saved.account_id !== accountId) throw new Error(`${path} is for ${saved.account_id}`);
    return KeyPair.fromString(saved.private_key as KeyPairString);
  }
  const key = KeyPair.fromRandom("ed25519");
  createFile(path, credentialJson(accountId, key.getPublicKey().toString(), key.toString()), 0o600);
  return key;
}

function accountOf(accountId: string): Account {
  return new Account(accountId, provider, keyFor(accountId).toString());
}

/** The key must be a full-access key of the account: otherwise the name is
 * someone else's (or ours from another machine), and nothing goes on. */
async function checkOwnKey(accountId: string): Promise<void> {
  if (!existsSync(credentialPath(accountId))) {
    throw new Error(`${accountId} exists and ${credentialPath(accountId)} doesn't: pick another prefix`);
  }
  const publicKey = keyFor(accountId).getPublicKey().toString();
  const view = (await accessKey(accountId, publicKey).catch(() => null)) as {
    permission?: unknown;
  } | null;
  if (view?.permission !== "FullAccess") {
    throw new Error(`${accountId} exists but ${credentialPath(accountId)} isn't one of its full-access keys`);
  }
}

async function send(
  what: string,
  signerId: string,
  receiverId: string,
  list: ReturnType<typeof actions.transfer>[],
): Promise<string> {
  const outcome = await accountOf(signerId).signAndSendTransaction({
    receiverId,
    actions: list,
    waitUntil: "FINAL",
    throwOnFailure: true,
  });
  const hash = outcome.transaction_outcome.id;
  log(`  ${what}: ${EXPLORER}${hash}`);
  return hash;
}

async function waitForAccount(accountId: string): Promise<void> {
  for (let i = 0; i < 30; i++) {
    if ((await accountView(accountId)) !== null) return;
    await new Promise((r) => setTimeout(r, 2_000));
  }
  throw new Error(`${accountId} didn't appear within a minute of the faucet's answer`);
}

async function faucet(accountId: string): Promise<void> {
  const existing = await accountView(accountId);
  if (existing !== null) {
    await checkOwnKey(accountId);
    log(`  ${accountId}: exists, ours (${formatNear(existing.amount)} NEAR)`);
    return;
  }
  const publicKey = keyFor(accountId).getPublicKey().toString();
  const response = await fetch(faucetUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ newAccountId: accountId, newAccountPublicKey: publicKey }),
    signal: AbortSignal.timeout(60_000),
  });
  if (!response.ok) {
    throw new Error(`the faucet answered ${response.status} for ${accountId}: ${(await response.text()).slice(0, 200)}`);
  }
  await waitForAccount(accountId);
  const view = await accountView(accountId);
  log(`  ${accountId}: created by the faucet (${formatNear(view?.amount ?? 0n)} NEAR)`);
}

function formatNear(yocto: bigint): string {
  const milli = yocto / 10n ** 21n;
  return `${milli / 1000n}.${(milli % 1000n).toString().padStart(3, "0")}`;
}

async function subAccount(id: string, near: string): Promise<void> {
  if ((await accountView(id)) !== null) {
    await checkOwnKey(id);
    log(`  ${id}: exists, ours`);
    return;
  }
  const publicKey = keyFor(id).getPublicKey();
  await send(`${id} created with ${near} NEAR`, world.demo, id, [
    actions.createAccount(),
    actions.transfer(nearToYocto(near)),
    actions.addFullAccessKey(publicKey),
  ]);
}

/** Deploys and initialises in one transaction, so nobody can call `new`
 * in between. Stops if the account already runs other code. */
async function deploy(accountId: string, file: string, init: object): Promise<void> {
  const { code, hash } = wasm(file);
  const view = await accountView(accountId);
  if (view === null) throw new Error(`${accountId} doesn't exist`);
  if (view.code_hash === hash) {
    log(`  ${accountId}: runs ${file} already (${hash})`);
    return;
  }
  if (view.code_hash !== NO_CODE) {
    throw new Error(`${accountId} runs other code (${view.code_hash}), not ${file} (${hash})`);
  }
  await send(`${file} deployed to ${accountId} with new()`, accountId, accountId, [
    actions.deployContract(code),
    actions.functionCall("new", init, teraToGas(100), 0n),
  ]);
}

async function checkCode(accountId: string, file: string): Promise<string> {
  const { hash } = wasm(file);
  const view = await accountView(accountId);
  if (view?.code_hash !== hash) {
    throw new Error(`${accountId}'s code hash is ${view?.code_hash}, want ${hash} (${file})`);
  }
  return hash;
}

async function main(): Promise<void> {
  const chain = await rpc.chainId();
  if (chain !== "testnet") throw new Error(`${rpcUrl} is ${chain}, not testnet`);
  log(`testnet through near CLI's ${connection} (${rpcUrl}); world ${world.prefix}; files in ${dir}`);
  privateDir(credentials);
  privateDir(dir);

  log("1. faucet accounts");
  for (const id of faucetAccounts(world)) await faucet(id);

  log("2. the demo account's sub-accounts");
  for (const { id, near } of subAccounts(world)) await subAccount(id, near);

  log("3. the test token");
  await deploy(world.token, "test_token.wasm", tokenInitArgs(world));

  log("4. TST storage");
  for (const id of storageAccounts(world)) {
    if ((await view(world.token, "storage_balance_of", { account_id: id })) !== null) {
      log(`  ${id}: registered already`);
      continue;
    }
    await send(`${id} registered`, world.demo, world.token, [
      actions.functionCall("storage_deposit", { account_id: id }, teraToGas(30), nearToYocto("0.01")),
    ]);
  }

  log("5. the guard");
  await deploy(world.guard, "near_spend_guard.wasm", guardInitArgs(world));

  log("6. the guard's TST");
  const funded = join(dir, "funded");
  const balance = BigInt((await view(world.token, "ft_balance_of", { account_id: world.guard })) as string);
  if (existsSync(funded) || balance > 0n) {
    log(`  ${world.guard}: funded already (${balance / TST} TST now)`);
  } else {
    const hash = await send(`${GUARD_FUNDING / TST} TST to ${world.guard}`, world.demo, world.token, [
      actions.functionCall(
        "ft_transfer",
        { receiver_id: world.guard, amount: GUARD_FUNDING.toString() },
        teraToGas(30),
        ONE_YOCTO,
      ),
    ]);
    createFile(funded, `${hash}\n`, 0o644);
  }

  log("7. the policy");
  const token = tokenOf(world);
  if ((await view(world.guard, "get_token", { token })) === null) {
    await send("set_token", world.approver, world.guard, [
      actions.functionCall("set_token", setTokenArgs(world), teraToGas(30), ONE_YOCTO),
    ]);
  } else {
    log("  set_token: done already");
  }
  const before = (await view(world.guard, "get_token", { token })) as {
    receivers?: Array<{ account_id: string }>;
  };
  if (!(before.receivers ?? []).some((r) => r.account_id === world.api)) {
    await send(`allow_receiver ${world.api}`, world.approver, world.guard, [
      actions.functionCall("allow_receiver", allowApiArgs(world), teraToGas(30), ONE_YOCTO),
    ]);
  } else {
    log(`  allow_receiver ${world.api}: done already`);
  }

  log("8. checks");
  const config = (await view(world.guard, "get_config", {})) as Record<string, unknown>;
  for (const [field, want] of Object.entries(guardInitArgs(world))) {
    if (config[field] !== want) throw new Error(`get_config ${field} is ${JSON.stringify(config[field])}, want ${want}`);
  }
  if (config.paused !== false) throw new Error("the guard is paused");
  const policy = await view(world.guard, "get_token", { token });
  const diffs = policyDiff(world, policy);
  if (diffs.length > 0) throw new Error(`the policy differs: ${diffs.join("; ")}`);
  const remaining = (await view(world.guard, "remaining_budget", { token })) as string;
  const spent = (policy as { autonomous_spent: string }).autonomous_spent;
  if (BigInt(remaining) + BigInt(spent) !== 100n * TST) {
    throw new Error(`remaining_budget ${remaining} and autonomous_spent ${spent} don't add up to the daily 100 TST`);
  }
  log(`  get_config and get_token match; remaining_budget ${BigInt(remaining) / TST} TST`);
  log(`  ${world.guard} code hash ${await checkCode(world.guard, "near_spend_guard.wasm")}`);
  log(`  ${world.token} code hash ${await checkCode(world.token, "test_token.wasm")}`);

  log("9. the agent's function-call key");
  const envPath = join(dir, "agent.env");
  if (!existsSync(envPath)) {
    const key = KeyPair.fromRandom("ed25519");
    createFile(envPath, agentEnv(world, rpcUrl, key.toString()), 0o600);
  }
  const envText = readFileSync(envPath, "utf8");
  if (/^NSG_RPC_URL=(.+)$/m.exec(envText)?.[1] !== rpcUrl) {
    throw new Error(`${envPath} names another RPC than ${rpcUrl}`);
  }
  const secret = /^NSG_AGENT_KEY=(.+)$/m.exec(envText)?.[1];
  if (secret === undefined) throw new Error(`${envPath} has no NSG_AGENT_KEY`);
  const agentKey = KeyPair.fromString(secret as KeyPairString).getPublicKey();
  const onChain = (await accessKey(world.agent, agentKey.toString()).catch(() => null)) as {
    permission?: { FunctionCall?: { receiver_id: string; method_names: string[] } };
  } | null;
  if (onChain === null) {
    await send(`function-call key ${agentKey} on ${world.agent}`, world.agent, world.agent, [
      actions.addFunctionCallAccessKey(agentKey, world.guard, ["request_payment"], nearToYocto(AGENT_KEY_ALLOWANCE_NEAR)),
    ]);
  } else {
    const call = onChain.permission?.FunctionCall;
    if (call?.receiver_id !== world.guard || call.method_names.join(",") !== "request_payment") {
      throw new Error(`${world.agent}'s key ${agentKey} can do more than request_payment on ${world.guard}`);
    }
    log(`  ${agentKey}: on chain already, request_payment on ${world.guard} only`);
  }

  log("10. files");
  const files: Array<[string, string, number]> = [
    [join(dir, "mcp.json"), mcpJson(envPath, join(REPO, "client/bin/nsg-pay-mcp.ts")), 0o644],
    [join(dir, "world.json"), worldJson(world, connection, rpcUrl), 0o644],
    [join(dir, "config/near-spend-guard/approver.json"), approverJson(world, connection, rpcUrl), 0o644],
  ];
  for (const [path, contents, mode] of files) {
    mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
    if (existsSync(path)) {
      if (readFileSync(path, "utf8") !== contents) throw new Error(`${path} exists with other contents`);
      log(`  ${path}: as expected`);
    } else {
      createFile(path, contents, mode);
      log(`  ${path}: written`);
    }
  }
  log("done");
}

main().catch((e) => {
  console.error(`deploy-testnet: ${(e as Error).message}`);
  process.exitCode = 1;
});
