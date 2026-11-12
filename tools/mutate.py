"""Weaken one check at a time in the guard and confirm the unit tests catch it.

Usage: python3 -I tools/mutate.py <repo> <clean wrapper> <backup dir> [name filter]
Each mutant is (name, old, new) for lib.rs, or (name, old, new, file, nth) to
pick a file under contracts/guard/src and the nth occurrence (from 0) of a
pattern that appears more than once.
"""
import pathlib, subprocess, sys

repo = pathlib.Path(sys.argv[1]); clean = str(pathlib.Path(sys.argv[2]).resolve())
backups = pathlib.Path(sys.argv[3]); backups.mkdir(parents=True, exist_ok=True)
only = sys.argv[4] if len(sys.argv) > 4 else ""
src = repo / "contracts/guard/src"
LIB, TYPES = "lib.rs", "types.rs"

MUTANTS = [
    # Day 1 (1 Nov)
    ("agent as receiver", "require!(receiver_id != self.agent_id, ERR_AGENT_IS_RECEIVER);", ""),
    ("1 yocto on owner calls", "        assert_one_yocto();\n", "", LIB, 1),
    ("agent already a receiver", "state.receiver(&agent_id).is_none()", "true"),
    ("approver = guard in set_approver", "approver_id != env::current_account_id(),", "true,"),
    ("approver = agent in set_approver", "require!(approver_id != self.agent_id, ERR_AGENT_IS_APPROVER);", ""),
    ("ninth token", "self.tokens.len() < MAX_TOKENS", "self.tokens.len() <= MAX_TOKENS"),
    ("33rd receiver", "state.receivers.len() < MAX_RECEIVERS", "state.receivers.len() <= MAX_RECEIVERS"),
    ("agent may pause", "caller == self.owner_id || caller == self.approver_id,", "caller == self.owner_id || caller == self.approver_id || caller == self.agent_id,", LIB, 0),
    ("owner = guard in new", "require!(owner_id != guard_id, ERR_OWNER_IS_GUARD);", ""),
    ("token = guard", "contract_id != &env::current_account_id(),", "true,"),
    ("fresh generation", "self.next_generation += 1;", ""),
    ("slot lower bound", "slot_ns.0 >= MIN_SLOT_NS", "slot_ns.0 > 0"),
    ("receiver = guard in allow_receiver", "receiver_id != env::current_account_id(),", "true,", LIB, 0),
    ("ft_call on native", "require!(!ft_call || token != Token::Native, ERR_FT_CALL_NATIVE);", ""),
    ("disallow unknown", "require!(state.receivers.len() < before, ERR_RECEIVER_NOT_ALLOWED);", ""),
    ("threshold above max", "approval_above.0 <= per_payment_max.0,", "true,"),
    # Day 2 (2 Nov): request_payment
    ("caller is the agent", "env::predecessor_account_id() == self.agent_id,", "true,"),
    ("zero amount", "require!(amount.0 > 0, ERR_ZERO_AMOUNT);", "", LIB, 1),
    ("receiver = guard in request", "receiver_id != env::current_account_id(),", "true,", LIB, 2),
    ("expiry at now", "require!(expires_at.0 > now, ERR_EXPIRED);", "require!(expires_at.0 >= now, ERR_EXPIRED);"),
    ("expiry 7 days inclusive", "expires_at.0 - now <= MAX_EXPIRY_NS", "expires_at.0 - now < MAX_EXPIRY_NS"),
    ("msg with native", "(Token::Native, Some(_)) => env::panic_str(ERR_MSG_NATIVE),", "(Token::Native, Some(_)) => Method::Native,"),
    ("msg length bound", "m.len() <= MAX_MSG_BYTES", "m.len() <= MAX_MSG_BYTES + 1"),
    ("msg length in chars", "m.len() <= MAX_MSG_BYTES", "m.chars().count() <= MAX_MSG_BYTES"),
    ("gas check", "env::prepaid_gas().saturating_sub(env::used_gas()) >= needed,", "true,"),
    ("gas check counts the callback", "        .saturating_add(GAS_CALLBACK)\n", ""),
    ("request ids advance", "self.next_request_id = checked(request_id.checked_add(1));", ""),
    ("paused refuses", "if self.paused {", "if false {", LIB, 1),
    ("receiver allowlist", "state.receiver(&payload.receiver_id)", "state.receivers.first()"),
    ("ft_call flag", "payload.msg.is_some() && !rule.ft_call", "false"),
    ("per-payment max", "payload.amount.0 > state.policy.per_payment_max", "payload.amount.0 >= state.policy.per_payment_max"),
    ("approval threshold", "amount.0 > policy.approval_above", "amount.0 >= policy.approval_above"),
    ("daily cap inclusive", "total > policy.daily_max", "total >= policy.daily_max"),
    ("overflow is over the cap", "after.is_none_or(", "after.is_some_and("),
    ("pending limit", "self.pending.len() >= MAX_PENDING", "self.pending.len() > MAX_PENDING", LIB, 1),
    ("reserve before dispatch", "slot.autonomous = checked(slot.autonomous.checked_add(amount.0));", ""),
    # Day 2: dispatch
    ("memo carries the id", 'format!("nsg:{}", request_id.0)', '"nsg".to_string()'),
    ("1 yocto on ft calls", "NearToken::from_yoctonear(1),", "NearToken::from_yoctonear(0),", LIB, 1),
    ("dispatch gas per method", "                dispatch_gas(method),\n", "                GAS_FT_TRANSFER,\n"),
    ("callback gas", "            .with_static_gas(GAS_CALLBACK)\n", ""),
    ("callback gets the epoch", "                U64(epoch),\n", "                U64(epoch + 1),\n"),
    ("callback gets the generation", "                U64(generation),\n", "                U64(generation + 1),\n"),
    # Day 2: settle and release
    ("single-receipt failure releases", "            release: amount,\n", "            release: 0,\n"),
    ("ft_transfer_call failure is uncertain", "(Method::FtTransferCall, Err(PromiseError::Failed)) =>", "(Method::FtTransferCall, Err(PromiseError::Failed)) if false =>"),
    ("used capped at amount", "used.0.min(amount)", "used.0"),
    ("unparseable counts as used", "map_or(amount, ", "map_or(0, "),
    ("too long is paid", "(_, Err(_)) => paid(amount),", "(_, Err(_)) => Settled { outcome: Settlement::Failed, sent: Some(0), release: amount },"),
    ("result length bound", "pub const MAX_RESULT_BYTES: usize = 64;", "pub const MAX_RESULT_BYTES: usize = 65;"),
    ("release by mode", "Mode::Autonomous => &mut slot.autonomous,", "Mode::Autonomous => &mut slot.approved,"),
    ("release checks generation", ".is_none_or(|state| state.generation != generation)", ".is_none()"),
    ("release saturates", "let released = amount.min(*spent);", "let released = amount;"),
    # Approvals (built 2 Nov, planned for days 5-6)
    ("1 yocto on approve", "        assert_one_yocto();\n", "", LIB, 0),
    ("approve needs the approver", "require!(approver == self.approver_id, ERR_NOT_APPROVER);", ""),
    ("approve compares the payload", "if entry.payload != payload {", "if false {"),
    ("approve expiry inclusive", "env::block_timestamp() >= payload.expires_at.0", "env::block_timestamp() > payload.expires_at.0"),
    ("approve when paused", "require!(!self.paused, ERR_PAUSED);", ""),
    ("approve rechecks policy", "self.policy_refusal(&payload)", "None::<RefusalReason>", LIB, 1),
    ("approve is single use", "        self.pending.remove(&request_id.0);\n", "", LIB, 2),
    ("approved column", "slot.approved = checked(slot.approved.checked_add(payload.amount.0));", ""),
    ("approve dispatches as approved", "            Mode::Approved,\n", "            Mode::Autonomous,\n"),
    ("agent may reject", "caller == self.owner_id || caller == self.approver_id,", "caller == self.owner_id || caller == self.approver_id || caller == self.agent_id,", LIB, 1),
    ("reject unknown panics", "if self.pending.remove(&request_id.0).is_none() {", "if false {"),
    ("prune expiry inclusive", "now >= entry.payload.expires_at.0", "now > entry.payload.expires_at.0"),
    ("prune limit", "expired.truncate(limit as usize);", ""),
    ("prune oldest first", "        expired.sort_unstable();\n", ""),
    # 11 Nov (decision 51): a full list makes room from its expired entries
    ("full list sweeps", "            self.remove_expired(now, MAX_PENDING);\n", ""),
    ("sweep only when full", "self.pending.len() >= MAX_PENDING", "true", LIB, 0),
    ("sweep at exactly full", "self.pending.len() >= MAX_PENDING", "self.pending.len() > MAX_PENDING", LIB, 0),
    ("sweep uses the block's time", "self.remove_expired(now, MAX_PENDING);", "self.remove_expired(now - 1, MAX_PENDING);"),
    ("sweep removes them all", "self.remove_expired(now, MAX_PENDING);", "self.remove_expired(now, 1);"),
    ("set_agent cancels pending", "        self.pending.clear();\n", ""),
    ("get_pending in id order", "entries.sort_unstable_by_key(|entry| entry.payload.request_id.0);", ""),
    # withdraw (2 Nov)
    ("withdraw zero amount", "require!(amount.0 > 0, ERR_ZERO_AMOUNT);", "", LIB, 0),
    ("withdraw to the guard", "receiver_id != env::current_account_id(),", "true,", LIB, 1),
    ("withdraw memo", '"memo": "nsg:withdraw",', '"memo": "nsg",'),
    ("withdraw 1 yocto", "NearToken::from_yoctonear(1),", "NearToken::from_yoctonear(0),", LIB, 0),
    # Day 2: the window (types.rs)
    ("window is 25 slots", "epoch - slot.epoch < WINDOW_SLOTS as u64", "epoch - slot.epoch <= WINDOW_SLOTS as u64", TYPES, 0),
    ("stale slot emptied", "        if slot.epoch != epoch {\n", "        if false {\n", TYPES, 0),
    ("release only in its own hour", "(slot.epoch == epoch).then_some(slot)", "Some(slot)", TYPES, 0),
    # 5 Nov: what the guard can afford (decision 26)
    ("unaffordable dispatch", "if free_balance() < transfer_deposit(&payload.token, amount.0) {", "if false {"),
    ("affordable dispatch off by one", "if free_balance() < transfer_deposit(&payload.token, amount.0) {", "if free_balance() <= transfer_deposit(&payload.token, amount.0) {"),
    ("unaffordable approve", "free_balance() >= transfer_deposit(&payload.token, payload.amount.0),", "true,"),
    ("affordable approve off by one", "free_balance() >= transfer_deposit(&payload.token, payload.amount.0),", "free_balance() > transfer_deposit(&payload.token, payload.amount.0),"),
    ("unaffordable withdraw", "free_balance() >= transfer_deposit(&token, amount.0),", "true,"),
    ("affordable withdraw off by one", "free_balance() >= transfer_deposit(&token, amount.0),", "free_balance() > transfer_deposit(&token, amount.0),"),
    ("held entry without room", "if free_balance() < pending_entry_cost() {", "if false {"),
    ("held entry off by one", "if free_balance() < pending_entry_cost() {", "if free_balance() <= pending_entry_cost() {"),
    ("FT dispatch costs nothing", "Token::Ft(_) => 1,", "Token::Ft(_) => 0,"),
    ("storage cost ignored", ".saturating_sub(storage_cost)", ""),
    # 5 Nov: the hash, against the vectors the TypeScript client shares
    ("hash without its domain", "&(PAYLOAD_DOMAIN, env::current_account_id(), payload)", '&("", env::current_account_id(), payload)'),
    ("hash without the guard", "&(PAYLOAD_DOMAIN, env::current_account_id(), payload)", '&(PAYLOAD_DOMAIN, "", payload)'),
]


def apply(text, old, new, nth, name):
    count = text.count(old)
    if nth is None:
        assert count == 1, f"{name}: pattern found {count} times"
        return text.replace(old, new)
    assert count > nth, f"{name}: pattern found {count} times, wanted occurrence {nth}"
    at = -1
    for _ in range(nth + 1):
        at = text.index(old, at + 1)
    return text[:at] + new + text[at + len(old):]


originals = {f: (src / f).read_text() for f in (LIB, TYPES)}
for f, text in originals.items():
    (backups / f).write_text(text)
survived = []
# A test that already fails would make every mutant look caught.
r = subprocess.run([clean, "cargo", "test", "-p", "near-spend-guard", "--offline", "--locked", "-q"],
                   cwd=repo, capture_output=True, text=True)
assert r.returncode == 0, f"the unit tests fail before any mutant:\n{r.stdout[-2000:]}"
try:
    for m in MUTANTS:
        name, old, new = m[:3]
        file, nth = (m[3], m[4]) if len(m) > 3 else (LIB, None)
        if only and only not in name:
            continue
        (src / file).write_text(apply(originals[file], old, new, nth, name))
        r = subprocess.run([clean, "cargo", "test", "-p", "near-spend-guard", "--offline", "--locked", "-q"],
                           cwd=repo, capture_output=True, text=True)
        (src / file).write_text(originals[file])
        failed = [l.split()[1] for l in r.stdout.splitlines() if l.startswith("test ") and l.endswith("FAILED")]
        if "could not compile" in r.stderr:
            # A mutant that doesn't build proves nothing about the tests.
            print(f"NO BUILD {name}", flush=True)
            survived.append(f"{name} (no build)")
            continue
        caught = r.returncode != 0
        detail = ", ".join(failed[:3]) + (f" (+{len(failed) - 3})" if len(failed) > 3 else "")
        if not failed:
            detail = (r.stderr.strip().splitlines() or ["?"])[-1]
        print(f"{'caught  ' if caught else 'SURVIVED'} {name}: {detail}", flush=True)
        if not caught:
            survived.append(name)
finally:
    for f, text in originals.items():
        (src / f).write_text(text)
print(f"{len(MUTANTS)} mutants; survived:", survived or "none")
