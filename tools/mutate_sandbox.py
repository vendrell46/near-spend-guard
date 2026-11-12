"""Weaken one check at a time and confirm the sandbox tests catch it.

For what unit tests can't see: near-sdk's export wrapper (`#[init]`,
`#[private]`, the deposit check) and settlement against the real test token.
Each mutant rebuilds the wasm and runs one sandbox test file.

Usage: python3 -I tools/mutate_sandbox.py <repo> <clean wrapper> <backup dir> [name filter]
Each mutant is (name, file under the repo, old, new, test file).
"""
import pathlib, subprocess, sys

repo = pathlib.Path(sys.argv[1]); clean = str(pathlib.Path(sys.argv[2]).resolve())
backups = pathlib.Path(sys.argv[3]); backups.mkdir(parents=True, exist_ok=True)
only = sys.argv[4] if len(sys.argv) > 4 else ""
GUARD, TOKEN = "contracts/guard/src/lib.rs", "contracts/test-token/src/lib.rs"
NAIVE = "contracts/naive-guard/src/lib.rs"

MUTANTS = [
    # 3 Nov: the export wrapper
    ("new is public", GUARD, "    #[init]\n    #[private]\n", "    #[init]\n", "wrapper"),
    ("new may re-init", GUARD, "    #[init]\n    #[private]\n", "    #[init(ignore_state)]\n    #[private]\n", "wrapper"),
    ("callback is public", GUARD, "    #[private]\n    #[allow(clippy::too_many_arguments)]\n",
     "    #[allow(clippy::too_many_arguments)]\n", "wrapper"),
    ("request_payment payable", GUARD, "    pub fn request_payment(", "    #[payable]\n    pub fn request_payment(", "wrapper"),
    ("pause needs 1 yocto", GUARD, "    pub fn pause(&mut self) {\n",
     "    #[payable]\n    pub fn pause(&mut self) {\n        assert_one_yocto();\n", "wrapper"),
    # 3 Nov: settlement against the test token
    ("transfer_call failure releases", GUARD,
     "            outcome: Settlement::Uncertain,\n            sent: None,\n            release: 0,",
     "            outcome: Settlement::Uncertain,\n            sent: None,\n            release: amount,", "settlement"),
    ("too long releases", GUARD, "        (_, Err(_)) => paid(amount),", "        (_, Err(_)) => paid(0),", "settlement"),
    ("unparseable is unused", GUARD, "map_or(amount, |used|", "map_or(0, |used|", "settlement"),
    ("refund ignored", GUARD, "|used| used.0.min(amount)", "|_used| amount", "settlement"),
    ("result read unbounded", GUARD, "env::promise_result_checked(0, MAX_RESULT_BYTES)",
     "env::promise_result_checked(0, 1024)", "settlement"),
    ("single-receipt failure kept", GUARD,
     "            outcome: Settlement::Failed,\n            sent: Some(0),\n            release: amount,",
     "            outcome: Settlement::Failed,\n            sent: Some(0),\n            release: 0,", "settlement"),
    # 3 Nov: the parallel burst
    ("no reservation at dispatch", GUARD, "        slot.autonomous = checked(slot.autonomous.checked_add(amount.0));\n",
     "", "burst"),
    # 3 Nov: gas headroom and the window on a real clock
    ("callback gas cut to 3", GUARD, "pub const GAS_CALLBACK: Gas = Gas::from_tgas(10);",
     "pub const GAS_CALLBACK: Gas = Gas::from_tgas(3);", "gas"),
    ("ft_transfer gas cut to 3", GUARD, "pub const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);",
     "pub const GAS_FT_TRANSFER: Gas = Gas::from_tgas(3);", "gas"),
    ("release promised a slot early", GUARD, "(oldest + WINDOW_SLOTS as u64).saturating_mul",
     "(oldest + WINDOW_SLOTS as u64 - 1).saturating_mul", "window"),
    ("release promised a slot late", GUARD, "(oldest + WINDOW_SLOTS as u64).saturating_mul",
     "(oldest + WINDOW_SLOTS as u64 + 1).saturating_mul", "window"),
    # 4 Nov (day 7): the demo tests. Each scene's claim must fail when our
    # guard loses the property it shows, and the race must notice a naive
    # guard that isn't naive.
    ("demo race: no reservation at dispatch", GUARD,
     "        slot.autonomous = checked(slot.autonomous.checked_add(amount.0));\n", "", "demo"),
    ("demo race: naive guard counts at dispatch", NAIVE,
     "        PromiseOrValue::Promise(dispatch(payload, true))\n",
     "        let epoch = now / self.slot_ns;\n"
     "        let state = self.tokens.get_mut(&payload.token).unwrap();\n"
     "        let slot = &mut state.slots[(epoch % WINDOW_SLOTS) as usize];\n"
     "        if slot.0 != epoch {\n            *slot = (epoch, 0);\n        }\n"
     "        slot.1 += amount.0;\n"
     "        PromiseOrValue::Promise(dispatch(payload, false))\n", "demo"),
    ("demo price_swap: approve compares only the id", GUARD, "        if entry.payload != payload {\n",
     "        if entry.payload.request_id != payload.request_id {\n", "demo"),
    ("demo approval_flow: approved sent as autonomous", GUARD, "            Mode::Approved,\n",
     "            Mode::Autonomous,\n", "demo"),
    # 5 Nov: decision 26 against nearcore's own check, and the limits
    ("unaffordable dispatch reaches the runtime", GUARD, "if free_balance() < transfer_deposit(&payload.token, amount.0) {", "if false {", "settlement"),
    ("unaffordable approve reaches the runtime", GUARD, "free_balance() >= transfer_deposit(&payload.token, payload.amount.0),", "true,", "settlement"),
    ("unaffordable withdraw reaches the runtime", GUARD, "free_balance() >= transfer_deposit(&token, amount.0),", "true,", "settlement"),
    ("held entry reaches the runtime", GUARD, "if free_balance() < pending_entry_cost() {", "if false {", "settlement"),
    ("pending entry bound too low", GUARD, "pub const MAX_PENDING_ENTRY_BYTES: u64 = 1_500;", "pub const MAX_PENDING_ENTRY_BYTES: u64 = 1_300;", "limits"),
    ("callback reserve fits one receiver only", GUARD, "pub const GAS_CALLBACK: Gas = Gas::from_tgas(10);", "pub const GAS_CALLBACK: Gas = Gas::from_tgas(4);", "limits"),
    ("set_agent keeps pending", GUARD, "        self.pending.clear();\n", "", "limits"),
    # 11 Nov (decision 51): the sweep, on a full guard, inside the gas the client attaches
    ("full list keeps its expired entries", GUARD, "            self.remove_expired(now, MAX_PENDING);\n", "", "limits"),
    # The token's switches must be what the tests exercise.
    ("fail_resolve does nothing", TOKEN, "require!(self.switch != Switch::FailResolve, ERR_FAIL_RESOLVE);", "", "settlement"),
    ("long_result does nothing", TOKEN, "        Switch::LongResult => serde_json::from_str(LONG_RESULT).expect(\"valid JSON\"),\n",
     "", "settlement"),
    # 8 Nov: the testnet token's init
    ("token new is public", TOKEN, "    #[init]\n    #[private]\n", "    #[init]\n", "wrapper"),
]

files = sorted({m[1] for m in MUTANTS})
originals = {f: (repo / f).read_text() for f in files}
for f, text in originals.items():
    (backups / f.replace("/", "_")).write_text(text)
survived = []
# A test that already fails would make every mutant look caught.
for test in sorted({m[4] for m in MUTANTS if not only or only in m[0]}):
    r = subprocess.run([clean, "scripts/test-sandbox.sh", "--offline", "--locked", "--test", test],
                       cwd=repo, capture_output=True, text=True)
    assert r.returncode == 0, f"{test} fails before any mutant:\n{r.stdout[-2000:]}"
try:
    for name, file, old, new, test in MUTANTS:
        if only and only not in name:
            continue
        count = originals[file].count(old)
        assert count == 1, f"{name}: pattern found {count} times"
        (repo / file).write_text(originals[file].replace(old, new))
        r = subprocess.run([clean, "scripts/test-sandbox.sh", "--offline", "--locked", "--test", test],
                           cwd=repo, capture_output=True, text=True)
        (repo / file).write_text(originals[file])
        out = r.stdout + r.stderr
        if "could not compile" in out:
            print(f"NO BUILD {name}", flush=True)
            survived.append(f"{name} (no build)")
            continue
        failed = [l.split()[1] for l in r.stdout.splitlines() if l.startswith("test ") and l.endswith("FAILED")]
        caught = r.returncode != 0 and bool(failed)
        detail = ", ".join(failed) if failed else (out.strip().splitlines() or ["?"])[-1]
        print(f"{'caught  ' if caught else 'SURVIVED'} {name}: {detail}", flush=True)
        if not caught:
            survived.append(name)
finally:
    for f, text in originals.items():
        (repo / f).write_text(text)
print(f"{len(MUTANTS)} mutants; survived:", survived or "none")
