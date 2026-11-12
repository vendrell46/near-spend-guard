"""Weaken one line of the TypeScript client and confirm its tests catch it.

Usage: python3 -I tools/mutate_client.py <repo> <clean wrapper> <backup dir> [name filter]
Each mutant is (name, old, new) under a file in client/src (or ../deploy/). `near.ts` runs
test-near/ too, which needs `npm ci --ignore-scripts` first. This runs nothing
but our own code, near-api-js and Node's test runner.
"""
import os, pathlib, shutil, subprocess, sys

repo = pathlib.Path(sys.argv[1]); clean = str(pathlib.Path(sys.argv[2]).resolve())
backups = pathlib.Path(sys.argv[3]); backups.mkdir(parents=True, exist_ok=True)
only = sys.argv[4] if len(sys.argv) > 4 else ""
node = shutil.which("node")
assert node, "needs node on PATH (22.22.2 or later)"

MUTANTS = {
    "payload.ts": [
        ("lengths big-endian", "out.writeUInt32LE(value);", "out.writeUInt32BE(value);"),
        ("integers big-endian", "out[i] = Number(rest & 0xffn);", "out[bytes - 1 - i] = Number(rest & 0xffn);"),
        ("any spelling of a number", '!/^(?:0|[1-9]\\d*)$/.test(value)', "false"),
        ("no width check", "if (rest >> BigInt(bytes * 8) !== 0n) {", "if (false) {"),
        ("width off by one bit", "rest >> BigInt(bytes * 8)", "rest >> BigInt(bytes * 8 + 1)"),
        ("domain dropped", "    text(PAYLOAD_DOMAIN),\n", ""),
        ("guard dropped", '    accountId(guardId, "guard"),\n', ""),
        ("token dropped", "    token(payload.token),\n", ""),
        ("receiver dropped", '    accountId(payload.receiver_id, "receiver_id"),\n', ""),
        ("expiry dropped", '    uint(payload.expires_at, 8, "expires_at"),\n', ""),
        ("token tags swapped", '? u8(0)\n    : Buffer.concat([u8(1),', '? u8(1)\n    : Buffer.concat([u8(0),'),
        ("no msg and empty msg alike", "if (value === null) {", 'if (value === null || value === "") {'),
        ("msg limit in characters", 'Buffer.byteLength(value, "utf8") > MAX_MSG_BYTES', "value.length > MAX_MSG_BYTES"),
        ("msg limit off by one", 'Buffer.byteLength(value, "utf8") > MAX_MSG_BYTES', 'Buffer.byteLength(value, "utf8") >= MAX_MSG_BYTES'),
        ("lone surrogates pass", "if (!value.isWellFormed()) {", "if (false) {"),
        ("id length", "value.length > 64", "value.length > 65"),
        ("id minimum", "value.length < 2", "value.length < 1"),
        ("id rules", "!ACCOUNT_ID.test(value)", "false"),
        ("wrong hash", 'createHash("sha256")', 'createHash("sha512")'),
    ],
    "display.ts": [
        ("non-ASCII passes", "/[^\\x20-\\x7e]/g,", "/[^\\x20-\\uffff]/g,"),
        ("DEL passes", "/[^\\x20-\\x7e]/g,", "/[^\\x20-\\x7f]/g,"),
        ("short escape", ".padStart(4, \"0\")", ""),
        ("ascii test takes all", "/^[\\x20-\\x7e]*$/.test(value)", "true"),
        ("ascii test takes DEL", "/^[\\x20-\\x7e]*$/", "/^[\\x20-\\x7f]*$/"),
        ("plain keeps quotes", ".slice(1, -1)", ""),
    ],
    "amount.ts": [
        ("no decimals check", "if (!Number.isInteger(decimals) || decimals < 0 || decimals > 255) {", "if (false) {"),
        ("rounds silently", "if (fraction.length > decimals) {", "if (false) {"),
        ("trailing zeros count", '.replace(/0+$/, "");\n  if (fraction.length', ';\n  if (fraction.length'),
        ("zero is a payment", "if (base === 0n) {", "if (false) {"),
        ("no u128 check", "if (base > U128_MAX) {", "if (false) {"),
        ("u128 off by one", "(1n << 128n) - 1n", "(1n << 128n)"),
        ("any amount spelling", "/^(0|[1-9]\\d*)(?:\\.(\\d+))?$/", "/^(\\d*)(?:\\.(\\d*))?/"),
        ("format any base", 'if (!/^(?:0|[1-9]\\d*)$/.test(base)) {', "if (false) {"),
        ("format keeps zeros", 'digits.slice(point).replace(/0+$/, "")', "digits.slice(point)"),
    ],
    "rpc.ts": [
        ("ignores HTTP status", "if (!response.ok) {", "if (false) {"),
        ("ignores RPC errors", "if (body.error !== undefined) {", "if (false) {"),
        ("ignores view errors", 'if (typeof result.error === "string") {', "if (false) {"),
        ("no view result check", "if (!Array.isArray(result.result)) {", "if (false) {"),
        ("no timeout", "      signal: AbortSignal.timeout(timeoutMs),\n", ""),
        ("final views", '{ finality: "optimistic", ...params }', '{ finality: "final", ...params }'),
        ("optimistic block time", 'call("block", { finality: "final" })', 'call("block", { finality: "optimistic" })'),
        ("no block time check", 'if (typeof time !== "string" || !/^\\d+$/.test(time)) {', "if (false) {"),
        ("mainnet passes", '=== "mainnet"', '=== "nope"'),
        ("no chain id check", 'if (typeof status.chain_id !== "string") {', "if (false) {"),
        ("error shows no text", "`the RPC refused: ${printable(JSON.stringify(body.error)).slice(0, 300)}`", "`the RPC refused`"),
    ],
    "guard.ts": [
        ("reasons unchecked", "if (!allowed.includes(value as T)) {", "if (false) {"),
        ("loose decimal", 'if (typeof value !== "string" || !/^(?:0|[1-9]\\d*)$/.test(value)) {', 'if (typeof value !== "string") {'),
        ("loose hash", '!/^[0-9a-f]{64}$/.test(value)', 'false'),
        ("unknown status passes", "throw new Error(`unknown status: ${printable(String(o.status))}`);", 'return { status: "uncertain", request_id };'),
        ("hash not recomputed", "if (payloadHash(guardId, payload) !== payload_hash) {", "if (false) {"),
        ("msg type unchecked", 'if (p.msg !== null && typeof p.msg !== "string") {', "if (false) {"),
        ("receiver type unchecked", 'if (typeof p.receiver_id !== "string") {', "if (false) {"),
        ("empty msg is ft_transfer", 'return msg === null ? "ft_transfer" : "ft_transfer_call";', 'return !msg ? "ft_transfer" : "ft_transfer_call";'),
        ("no ft_metadata check", 'if (typeof m.symbol !== "string" || !Number.isInteger(m.decimals)) {', "if (false) {"),
        ("symbol uncut", "m.symbol.slice(0, MAX_SYMBOL_CHARS)", "m.symbol"),
        ("wrong native decimals", "decimals: NEAR_DECIMALS", "decimals: 18"),
        ("paused unchecked", 'if (typeof c.paused !== "boolean") {', "if (false) {"),
        ("config fields unchecked", 'if (typeof c[field] !== "string") {', "if (false) {"),
        ("config ids unchecked", "    checkAccountId(c[field], field);\n", ""),
        ("version unchecked", 'if (typeof c.version !== "string") {', "if (false) {"),
        ("null becomes a request", "return value === null ? null : parsePending(guardId, value);", "return parsePending(guardId, value);"),
    ],
    "gas.json": [
        ("native gas", '"native": 30', '"native": 31'),
        ("ft_transfer gas", '"ft_transfer": 40', '"ft_transfer": 100'),
        ("ft_transfer_call gas", '"ft_transfer_call": 100', '"ft_transfer_call": 40'),
        ("approve gas", '"approve": 100', '"approve": 30'),
    ],
    "pay.ts": [
        ("default expiry", "DEFAULT_EXPIRES_IN_S = 600", "DEFAULT_EXPIRES_IN_S = 601"),
        ("expiry limit", "7 * 24 * 60 * 60", "8 * 24 * 60 * 60"),
        ("extra inputs pass", "if (!INPUT_FIELDS.includes(key)) {", "if (false) {"),
        ("string token passes", """if (token !== "native" && (typeof ft !== "string" || Object.keys(token as object).length !== 1)) {""", "if (false) {"),
        ("token with extras passes", " || Object.keys(token as object).length !== 1", ""),
        ("numeric amount passes", 'if (typeof amount !== "string") {', "if (false) {"),
        ("expiry range open at 0", "expiresIn < 1", "expiresIn < 0"),
        ("expiry range open at top", "expiresIn > MAX_EXPIRES_IN_S", "expiresIn > MAX_EXPIRES_IN_S + 1"),
        ("fractional expiry", "!Number.isInteger(expiresIn) || ", ""),
        ("msg with native", 'if (input.token === "native" && msg !== null) {', "if (false) {"),
        ("machine time", "BigInt(await rpc.blockTimeNs())", "BigInt(Date.now()) * 1_000_000n"),
        ("flat gas", "agentGas(methodOf(args.token, msg))", "100"),
        ("no validation before send", '  payloadBytes(guardId, { request_id: "0", ...args });\n', ""),
        ("hash mismatch hidden", "...(ours === outcome.payload_hash\n            ? []", "...(true\n            ? []"),
        ("partial refund reads as full", "return outcome.sent === payload.amount\n", "return true\n"),
        ("unreadable answer rethrown", "return unknown(`the guard's answer couldn't be read ((e as Error).message})`, sent.tx_hash);".replace("((e", "(${(e"), "throw e;"),
        ("unknown drops the warning", "\\nDo not pay again before checking.`,", "`,"),
        ("why raw", "`unknown: ${plain(why)}.", "`unknown: ${why}."),
        ("symbol raw", "${plain(info.symbol)}`;\n}\n\nfunction report", "${info.symbol}`;\n}\n\nfunction report"),
        ("panic text raw", "`rejected, nothing changed: ${printable(sent.error)}`", "`rejected, nothing changed: ${sent.error}`"),
        ("later failure reads as rejected", "if (first === undefined || failure(own?.outcome.status) !== undefined) {", "if (true) {"),
        ("no-receipt failure unknown", "first === undefined || ", ""),
        ("no-JSON value thrown", 'return { kind: "unknown", why: "the guard\'s answer isn\'t JSON", tx_hash: txHash };', 'return { kind: "rejected", error: "x", tx_hash: txHash };'),
        ("no status reads as rejected", 'return { kind: "unknown", why: "the transaction has no final status", tx_hash: txHash };', 'return { kind: "rejected", error: "x", tx_hash: txHash };'),
        ("deadline never fires", 'timer = setTimeout(() => resolve("timeout"), ms);', ""),
        ("any agent", "if (config.agent_id !== agentId) {", "if (false) {"),
        ("any receiver on the key", "limited?.receiver_id !== guardId ||\n", "!limited ||\n"),
        ("many methods on the key", "limited.method_names?.length !== 1 ||\n", ""),
        ("any method on the key", 'limited.method_names[0] !== "request_payment"', "false"),
    ],
    "mcp.ts": [
        ("any tool", "if (params.name !== PAY_TOOL.name) {", "if (false) {"),
        ("errors not flagged", "const isError = !OK_STATUSES.includes(report.status);", "const isError = false;"),
        ("refused is an error", 'const OK_STATUSES = ["paid", "pending", "refused"];', 'const OK_STATUSES = ["paid", "pending"];'),
        ("thrown input crashes", 'const text = `not sent: ${plain((e as Error).message)}`;', "throw e;"),
        ("error text raw", '`not sent: ${plain((e as Error).message)}`', '`not sent: ${(e as Error).message}`'),
        ("any version accepted", "PROTOCOL_VERSIONS.includes(asked as string)\n        ? asked", "true\n        ? asked"),
        ("notifications pay", "if (message.id === undefined) {", "if (false) {"),
        ("bad line stops the server", 'send(null, { error: { code: -32700, message: "not JSON" } });\n      continue;', "throw new Error();"),
        ("end doesn't wait", "  await Promise.all(running);\n", ""),
        ("calls in line", "const call = answer(method, params, run).then((response) => {", "const call = await answer(method, params, run).then((response) => {"),
        ("no ping", '    case "ping":\n      return { result: {} };\n', ""),
    ],
    "approve.ts": [
        ("guard from anywhere", '"guard_id", "approver_id", "network", "rpc_url"] as const', '"approver_id", "network", "rpc_url"] as const'),
        ("guard id unchecked", '  checkAccountId(config.guard_id, "guard_id");\n', ""),
        ("approver id unchecked", '  checkAccountId(config.approver_id, "approver_id");\n', ""),
        ("network unchecked", "if (!/^[a-z0-9][a-z0-9-]*$/.test(config.network)) {", "if (false) {"),
        ("mainnet config", 'if (config.network.includes("mainnet")) {', "if (false) {"),
        ("only plain mainnet", 'config.network.includes("mainnet")', 'config.network === "mainnet"'),
        ("extra arguments pass", "argv.length !== 1 || ", ""),
        ("loose id", '!/^[1-9]\\d*$/.test(id) || ', ""),
        ("id over u64", " || BigInt(id) >= 1n << 64n", ""),
        ("signs at once", '    config.network,\n  ];', '    config.network,\n    "sign-with-keychain",\n    "send",\n  ];'),
        ("attaches more", '"1 yoctoNEAR"', '"2 yoctoNEAR"'),
        ("no chain check", "  await refuseMainnet(rpc);\n", ""),
        ("any approver", "if (guard.approver_id !== config.approver_id) {", "if (false) {"),
        ("paused goes on", "if (guard.paused) {", "if (false) {"),
        ("wrong id goes on", "if (view.payload.request_id !== id) {", "if (false) {"),
        ("expiry goes on", "if (BigInt(await rpc.blockTimeNs()) >= BigInt(view.payload.expires_at)) {", "if (false) {"),
        ("expiry off by one", ">= BigInt(view.payload.expires_at)", "> BigInt(view.payload.expires_at)"),
        ("metadata stops it", ".catch(() => null)", ""),
        ("msg raw", 'payload.msg === null ? "none" : printable(payload.msg)', 'payload.msg === null ? "none" : payload.msg'),
        ("no non-ASCII note", "payload.msg !== null && !isPlainAscii(payload.msg)", "false"),
        ("config's binary ignored", 'config.near_cli ?? "near"', '"near"'),
        ("no fallback config dir", 'env.XDG_CONFIG_HOME || join(homedir(), ".config")', 'env.XDG_CONFIG_HOME ?? join(homedir(), ".config")'),
    ],
    "../deploy/world.ts": [
        ("any prefix", "!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(prefix) || ", ""),
        ("prefix length off by one", "prefix.length > 40", "prefix.length > 41"),
        ("policy field unchecked", "if (got.policy?.[field] !== want) {", "if (false) {"),
        ("extra receivers pass", "receivers.length !== 1 || ", ""),
        ("other receiver passes", "receivers[0].account_id !== world.api || ", ""),
        ("ft_call receiver passes", " || receivers[0].ft_call !== false", ""),
        ("secret file replaced", "constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL", "constants.O_WRONLY | constants.O_CREAT | constants.O_TRUNC"),
        ("secret file readable", "constants.O_EXCL, mode);", "constants.O_EXCL, 0o644);"),
        ("hash in any case", "/^[0-9a-f]{64}$/.test(hex)", "/^[0-9a-fA-F]{64}$/.test(hex)"),
        ("any sums line", "if (match === null) {", "if (false) {"),
        ("leading zeros dropped", "    out = `1${out}`;", "    out = `${out}`;"),
        ("base58 alphabet", '"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"', '"123456789ABCDEFGHIJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxy"'),
        ("agent owns the guard", "    owner_id: world.approver,\n    agent_id", "    owner_id: world.agent,\n    agent_id"),
        ("approver on fixed network", "    network: connection,", '    network: "testnet",'),
        ("daily cap doubled", "daily_max: (100n * TST)", "daily_max: (200n * TST)"),
        # 12 Nov: tidy's verdict before recording
        ("paused ignored", "if (s.paused) problems.push", "if (false) problems.push"),
        ("budget boundary", "if (s.budget < SCENE3_AUTONOMOUS) {", "if (s.budget <= SCENE3_AUTONOMOUS) {"),
        ("budget unchecked", "if (s.budget < SCENE3_AUTONOMOUS) {", "if (false) {"),
        ("guard TST needs only the 60", "s.guardTst < SCENE3_AUTONOMOUS + SCENE3_APPROVED", "s.guardTst < SCENE3_APPROVED"),
        ("guard NEAR boundary", "if (s.guardSpare < MIN_SPARE_YOCTO) {", "if (s.guardSpare <= MIN_SPARE_YOCTO) {"),
        ("missing agent key passes", "if (s.agentAllowance === null) {", "if (false) {"),
        ("allowance unchecked", 's.agentAllowance !== "unlimited" && s.agentAllowance < MIN_SPARE_YOCTO', "false"),
        ("approver NEAR unchecked", "if (s.approverSpare < MIN_SPARE_YOCTO) {", "if (false) {"),
        ("pending ignored", "if (s.pending.length > 0) {", "if (false) {"),
        ("spare margin lowered", "export const MIN_SPARE_YOCTO = 10n ** 23n;", "export const MIN_SPARE_YOCTO = 10n ** 22n;"),
        ("spare below zero", "return spare > 0n ? spare : 0n;", "return spare;"),
        ("storage price off", "export const YOCTO_PER_BYTE = 10n ** 19n;", "export const YOCTO_PER_BYTE = 10n ** 18n;"),
        ("account amount any text", '!/^\\d+$/.test(v.amount) || ', ''),
        ("key for another contract counts", " || call.receiver_id !== guard", ""),
        ("wider key counts", "methods.length !== 1 || ", ""),
        ("other method counts", ' || methods[0] !== "request_payment"', ""),
        ("no allowance reads as no key", 'call.allowance === undefined) return "unlimited";', 'call.allowance === undefined) return null;'),
        ("allowance any text", ' || !/^\\d+$/.test(call.allowance)', ''),
    ],
    "near.ts": [
        ("a deposit", "teraToGas(tgas), 0n)", "teraToGas(tgas), 1n)"),
        ("gas ignored", "teraToGas(tgas)", "teraToGas(300)"),
        ("another method", 'actions.functionCall("request_payment"', 'actions.functionCall("pause"'),
        ("another receiver", "receiverId: guardId", "receiverId: agentId"),
        ("waits for inclusion only", '"EXECUTED_OPTIMISTIC"', '"INCLUDED"'),
        ("waits forever", 'provider.sendTransactionUntil(signed, "EXECUTED_OPTIMISTIC"),\n        timeoutMs,', 'provider.sendTransactionUntil(signed, "EXECUTED_OPTIMISTIC"),\n        3_600_000,'),
        ("signing waits forever", "        }),\n        timeoutMs,", "        }),\n        3_600_000,"),
        ("no queue", "const turn = queue.then(() => send(args, tgas));", "const turn = send(args, tgas);"),
        ("nonce not read again", "        account = fresh();\n", ""),
        ("timeout reads as rejected", '? { kind: "unknown", why: `no answer within ${timeoutMs / 1000} s`, tx_hash: txHash }', '? { kind: "rejected", error: "x", tx_hash: txHash }'),
        ("send error reads as rejected", 'return { kind: "unknown", why: (e as Error).message, tx_hash: txHash };', 'return { kind: "rejected", error: "x", tx_hash: txHash };'),
        ("hash of the signed bytes", "signed.transaction.encode()", "signed.encode()"),
        ("not-sent reads as unknown", 'return { kind: "rejected", error: `not sent: ${(e as Error).message}`, tx_hash: null };', 'return { kind: "unknown", why: "x", tx_hash: null };'),
    ],
}

src = repo / "client/src"
tests = [clean, node, "--test"]
fast = tests + sorted(f"test/{p.name}" for p in (repo / "client/test").glob("*.test.ts"))
slow = fast + sorted(f"test-near/{p.name}" for p in (repo / "client/test-near").glob("*.test.ts"))
# A test that already fails would make every mutant look caught.
r = subprocess.run(slow, cwd=repo / "client", capture_output=True, text=True)
assert r.returncode == 0, f"the client tests fail before any mutant:\n{r.stdout[-2000:]}"
survived, count = [], 0
for name, mutants in MUTANTS.items():
    file = src / name
    original = file.read_text()
    # Flattened, so a file outside client/src (../deploy/world.ts) is backed
    # up inside the backup directory and not next to the original.
    (backups / name.replace("../", "").replace("/", "__")).write_text(original)
    try:
        for label, old, new in mutants:
            if only and only not in label:
                continue
            count += 1
            assert original.count(old) >= 1, f"{label}: pattern not found"
            assert original.count(old) == 1 or label == "gas ignored", f"{label}: pattern found {original.count(old)} times"
            file.write_text(original.replace(old, new, 1))
            run = slow if name == "near.ts" else fast
            try:
                r = subprocess.run(run, cwd=repo / "client", capture_output=True, text=True, timeout=120)
                caught = r.returncode != 0
            except subprocess.TimeoutExpired:
                caught = True
            print(f"{'caught  ' if caught else 'SURVIVED'} {name}: {label}", flush=True)
            if not caught:
                survived.append(f"{name}: {label}")
    finally:
        file.write_text(original)
print(f"{count} mutants; survived:", survived or "none")
