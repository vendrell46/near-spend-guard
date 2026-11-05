//! Unit tests with a mocked context.
//!
//! near-sdk generates the `#[init]` re-init check, `#[private]` and the
//! non-payable deposit check into the wasm export only, so calling the Rust
//! methods here never runs them. Those are sandbox tests (day 4). Every check
//! written in a method body is tested here.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};

use near_sdk::json_types::{U64, U128};
use near_sdk::serde_json::{self, Value, json};
use near_sdk::test_utils::{VMContextBuilder, get_logs};
use near_sdk::{AccountId, NearToken, VMContext, test_vm_config, testing_env};

use crate::types::*;
use crate::*;

mod payments;

const HOUR_NS: u64 = 3_600_000_000_000;
/// An hour boundary (1 Nov 2025, 13:00 UTC), so timestamps look real.
const START_NS: u64 = 1_762_002_000 * 1_000_000_000;
/// The mocked account's storage, a contract's worth.
const STORAGE_BYTES: u64 = 300 * 1024;

thread_local! {
    /// The mocked `block_timestamp`, applied by the next `call_as`.
    static NOW: Cell<u64> = const { Cell::new(START_NS) };
    /// NEAR above the storage cost, applied by the next `call_as`; `None`
    /// leaves plenty.
    static FREE: Cell<Option<u128>> = const { Cell::new(None) };
    static VM_CONFIG: RefCell<Option<Box<dyn Any>>> = const { RefCell::new(None) };
}

/// `test_vm_config()` rebuilds nearcore's whole runtime config store on every
/// call (about 10 ms), and the property tests make thousands of contexts, so
/// it is built once per thread. Generic because near-sdk doesn't re-export
/// the config's type.
fn vm_config<T: Clone + 'static>(make: fn() -> T) -> T {
    VM_CONFIG.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| Box::new(make()))
            .downcast_ref::<T>()
            .expect("one config type")
            .clone()
    })
}

fn now() -> u64 {
    NOW.with(Cell::get)
}

fn set_now(ns: u64) {
    NOW.with(|n| n.set(ns));
}

fn set_free(yocto: u128) {
    FREE.with(|f| f.set(Some(yocto)));
}
const ERR_ONE_YOCTO: &str = "Requires attached deposit of exactly 1 yoctoNEAR";

fn acc(id: &str) -> AccountId {
    id.parse().unwrap()
}
fn guard_id() -> AccountId {
    acc("guard.testnet")
}
fn owner() -> AccountId {
    acc("owner.testnet")
}
fn agent() -> AccountId {
    acc("agent.testnet")
}
fn approver() -> AccountId {
    acc("approver.testnet")
}
fn stranger() -> AccountId {
    acc("stranger.testnet")
}
fn alice() -> AccountId {
    acc("alice.testnet")
}
fn tst() -> Token {
    Token::Ft(acc("tst.nsg-token.testnet"))
}

/// The next calls come from `predecessor` with `yocto` attached. Starts a
/// fresh mocked blockchain, so the logs start empty; storage carries over.
fn call_as(predecessor: &AccountId, yocto: u128) {
    testing_env!(context(predecessor, yocto), vm_config(test_vm_config));
}

fn context(predecessor: &AccountId, yocto: u128) -> VMContext {
    let storage_cost = u128::from(STORAGE_BYTES) * env::storage_byte_cost().as_yoctonear();
    let free = FREE.with(Cell::get).unwrap_or(100 * 10u128.pow(24));
    VMContextBuilder::new()
        .current_account_id(guard_id())
        .predecessor_account_id(predecessor.clone())
        .attached_deposit(NearToken::from_yoctonear(yocto))
        .account_balance(NearToken::from_yoctonear(storage_cost + free))
        .storage_usage(STORAGE_BYTES)
        .block_timestamp(now())
        .build()
}

/// A guard on empty storage. Collections flush to storage when dropped, so a
/// guard from an earlier loop iteration would otherwise leak into this one.
fn init() -> Guard {
    near_sdk::mock::with_mocked_blockchain(|b| b.take_storage());
    FREE.with(|f| f.set(None));
    call_as(&guard_id(), 0);
    Guard::new(owner(), agent(), approver(), U64(HOUR_NS))
}

/// TST with a 100 per-payment and daily cap, approval above 50, and alice
/// allowlisted. The context is left as the owner with 1 yocto.
fn setup() -> Guard {
    let mut g = init();
    call_as(&owner(), 1);
    g.set_token(tst(), U128(100), U128(100), U128(50));
    g.allow_receiver(tst(), alice(), false);
    g
}

/// The panic message of `f`, or `None` if it returned.
fn panic_message(f: impl FnOnce()) -> Option<String> {
    catch_unwind(AssertUnwindSafe(f)).err().map(|e| {
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default()
    })
}

fn assert_panics(expected: &str, f: impl FnOnce()) {
    match panic_message(f) {
        Some(msg) => assert!(
            msg.contains(expected),
            "panicked with {msg:?}, expected {expected:?}"
        ),
        None => panic!("did not panic, expected {expected:?}"),
    }
}

fn events() -> Vec<Value> {
    get_logs()
        .iter()
        .map(|line| {
            let json = line
                .strip_prefix("EVENT_JSON:")
                .expect("log is not an event");
            serde_json::from_str(json).unwrap()
        })
        .collect()
}

fn event(name: &str, data: Value) -> Value {
    json!({ "standard": "near-spend-guard", "version": "1.0.0", "event": name, "data": data })
}

type OwnerCall = (&'static str, fn(&mut Guard));
/// A call, its caller and the one event it must emit.
type EventStep = (fn(&mut Guard), AccountId, Value);

/// Every owner method, with arguments that succeed on `setup()`.
const OWNER_CALLS: [OwnerCall; 8] = [
    ("set_token", |g| {
        g.set_token(tst(), U128(10), U128(10), U128(5))
    }),
    ("remove_token", |g| g.remove_token(tst())),
    ("allow_receiver", |g| {
        g.allow_receiver(tst(), acc("bob.testnet"), true)
    }),
    ("disallow_receiver", |g| g.disallow_receiver(tst(), alice())),
    ("set_agent", |g| g.set_agent(acc("agent2.testnet"))),
    ("set_approver", |g| g.set_approver(acc("approver2.testnet"))),
    ("unpause", |g| g.unpause()),
    ("withdraw", |g| {
        let _ = g.withdraw(tst(), U128(1), owner());
    }),
];

fn snapshot(g: &Guard) -> (ConfigView, Vec<TokenView>) {
    (g.get_config(), g.get_tokens())
}

// ---------- new ----------

#[test]
fn new_sets_config() {
    let g = init();
    let config = g.get_config();
    assert_eq!(config.owner_id, owner());
    assert_eq!(config.agent_id, agent());
    assert_eq!(config.approver_id, approver());
    assert!(!config.paused);
    assert_eq!(config.slot_ns, U64(HOUR_NS));
    assert_eq!(config.version, "1.0.0");
    assert_eq!(g.next_request_id, 1);
    assert!(g.get_tokens().is_empty());
    assert!(g.pending.is_empty());
}

#[test]
#[should_panic(expected = "the agent cannot be the owner")]
fn new_refuses_agent_as_owner() {
    call_as(&guard_id(), 0);
    Guard::new(agent(), agent(), approver(), U64(HOUR_NS));
}

#[test]
#[should_panic(expected = "the agent cannot be the approver")]
fn new_refuses_agent_as_approver() {
    call_as(&guard_id(), 0);
    Guard::new(owner(), agent(), agent(), U64(HOUR_NS));
}

#[test]
#[should_panic(expected = "the agent cannot be the guard")]
fn new_refuses_agent_as_guard() {
    call_as(&guard_id(), 0);
    Guard::new(owner(), guard_id(), approver(), U64(HOUR_NS));
}

#[test]
fn new_refuses_guard_as_owner_or_approver() {
    call_as(&guard_id(), 0);
    assert_panics(ERR_OWNER_IS_GUARD, || {
        Guard::new(guard_id(), agent(), approver(), U64(HOUR_NS));
    });
    assert_panics(ERR_APPROVER_IS_GUARD, || {
        Guard::new(owner(), agent(), guard_id(), U64(HOUR_NS));
    });
}

#[test]
#[should_panic(expected = "slot_ns must be at least 1 second")]
fn new_refuses_zero_slot() {
    call_as(&guard_id(), 0);
    Guard::new(owner(), agent(), approver(), U64(0));
}

#[test]
fn new_refuses_slot_below_one_second() {
    call_as(&guard_id(), 0);
    // An hour written in seconds or in milliseconds instead of nanoseconds.
    for slot_ns in [3_600, 3_600_000, MIN_SLOT_NS - 1] {
        assert_panics(ERR_SLOT_TOO_SHORT, || {
            Guard::new(owner(), agent(), approver(), U64(slot_ns));
        });
    }
    let g = Guard::new(owner(), agent(), approver(), U64(MIN_SLOT_NS));
    assert_eq!(g.get_config().slot_ns, U64(MIN_SLOT_NS));
}

#[test]
fn new_allows_owner_as_approver() {
    call_as(&guard_id(), 0);
    let g = Guard::new(owner(), agent(), owner(), U64(HOUR_NS));
    assert_eq!(g.get_config().approver_id, owner());
}

// ---------- every owner method ----------

#[test]
fn config_needs_owner() {
    for (name, call) in OWNER_CALLS {
        for caller in [stranger(), agent(), approver(), guard_id()] {
            let mut g = setup();
            let before = snapshot(&g);
            call_as(&caller, 1);
            let msg = panic_message(|| call(&mut g));
            assert!(
                msg.as_deref().is_some_and(|m| m.contains(ERR_NOT_OWNER)),
                "{name} from {caller}: {msg:?}"
            );
            assert_eq!(snapshot(&g), before, "{name} from {caller} changed state");
        }
        // The same call from the owner succeeds, so the refusal above was the
        // caller check and not the arguments.
        let mut g = setup();
        call_as(&owner(), 1);
        assert_eq!(
            panic_message(|| call(&mut g)),
            None,
            "{name} from the owner"
        );
    }
}

#[test]
fn config_needs_one_yocto() {
    for (name, call) in OWNER_CALLS {
        for yocto in [0, 2, NearToken::from_near(1).as_yoctonear()] {
            let mut g = setup();
            let before = snapshot(&g);
            call_as(&owner(), yocto);
            let msg = panic_message(|| call(&mut g));
            assert!(
                msg.as_deref().is_some_and(|m| m.contains(ERR_ONE_YOCTO)),
                "{name} with {yocto} yocto: {msg:?}"
            );
            assert_eq!(
                snapshot(&g),
                before,
                "{name} with {yocto} yocto changed state"
            );
        }
    }
}

// ---------- set_token ----------

#[test]
#[should_panic(expected = "approval_above cannot exceed per_payment_max")]
fn set_token_refuses_threshold_above_max() {
    let mut g = init();
    call_as(&owner(), 1);
    g.set_token(tst(), U128(100), U128(1000), U128(101));
}

#[test]
fn set_token_allows_threshold_equal_to_max() {
    let mut g = init();
    call_as(&owner(), 1);
    g.set_token(tst(), U128(100), U128(1000), U128(100));
    let view = g.get_token(tst()).unwrap();
    assert_eq!(view.policy.approval_above, U128(100));
    assert_eq!(view.policy.per_payment_max, U128(100));
}

#[test]
#[should_panic(expected = "the token contract cannot be the guard")]
fn set_token_refuses_guard_as_token() {
    let mut g = init();
    call_as(&owner(), 1);
    g.set_token(Token::Ft(guard_id()), U128(100), U128(100), U128(50));
}

#[test]
fn ninth_token_refused() {
    let mut g = init();
    call_as(&owner(), 1);
    g.set_token(Token::Native, U128(1), U128(1), U128(1));
    for i in 1..MAX_TOKENS {
        g.set_token(
            Token::Ft(acc(&format!("t{i}.testnet"))),
            U128(1),
            U128(1),
            U128(1),
        );
    }
    assert_eq!(g.get_tokens().len(), MAX_TOKENS as usize);
    assert_panics(ERR_TOO_MANY_TOKENS, || {
        g.set_token(Token::Ft(acc("t9.testnet")), U128(1), U128(1), U128(1));
    });
    // Updating one of the eight still works when the list is full.
    g.set_token(Token::Native, U128(5), U128(5), U128(5));
    assert_eq!(
        g.get_token(Token::Native).unwrap().policy.per_payment_max,
        U128(5)
    );
    assert_eq!(g.get_tokens().len(), MAX_TOKENS as usize);
}

#[test]
fn set_token_update_keeps_receivers_and_window() {
    let mut g = setup();
    let spent = Slot {
        epoch: 7,
        autonomous: 40,
        approved: 25,
    };
    g.tokens.get_mut(&tst()).unwrap().slots[7] = spent;
    g.set_token(tst(), U128(200), U128(300), U128(150));
    let state = g.tokens.get(&tst()).unwrap();
    assert_eq!(
        state.policy,
        Policy {
            per_payment_max: 200,
            daily_max: 300,
            approval_above: 150
        }
    );
    assert_eq!(
        state.receivers,
        vec![ReceiverRule {
            account_id: alice(),
            ft_call: false
        }]
    );
    assert_eq!(state.slots[7], spent);
    assert_eq!(state.generation, 1);
}

#[test]
fn readded_token_gets_a_new_generation() {
    let mut g = setup();
    g.set_token(Token::Native, U128(10), U128(10), U128(5));
    assert_eq!(g.tokens.get(&tst()).unwrap().generation, 1);
    assert_eq!(g.tokens.get(&Token::Native).unwrap().generation, 2);
    g.remove_token(tst());
    g.set_token(tst(), U128(100), U128(100), U128(50));
    assert_eq!(g.tokens.get(&tst()).unwrap().generation, 3);
    assert_eq!(g.tokens.get(&Token::Native).unwrap().generation, 2);
}

// ---------- remove_token ----------

#[test]
fn remove_token_deletes_policy_and_receivers() {
    let mut g = setup();
    g.tokens.get_mut(&tst()).unwrap().slots[3] = Slot {
        epoch: 3,
        autonomous: 90,
        approved: 0,
    };
    g.remove_token(tst());
    assert_eq!(g.get_token(tst()), None);
    assert!(g.get_tokens().is_empty());
    // Re-adding starts from nothing: no receivers and an empty window.
    g.set_token(tst(), U128(100), U128(100), U128(50));
    let state = g.tokens.get(&tst()).unwrap();
    assert!(state.receivers.is_empty());
    assert!(state.slots.iter().all(|s| *s == Slot::default()));
}

#[test]
#[should_panic(expected = "token not configured")]
fn remove_unknown_token_panics() {
    let mut g = setup();
    g.remove_token(Token::Native);
}

// ---------- allow_receiver and disallow_receiver ----------

#[test]
#[should_panic(expected = "the receiver cannot be the guard")]
fn allow_receiver_refuses_guard() {
    let mut g = setup();
    g.allow_receiver(tst(), guard_id(), false);
}

#[test]
#[should_panic(expected = "the agent cannot be a receiver")]
fn agent_cannot_be_receiver() {
    let mut g = setup();
    g.allow_receiver(tst(), agent(), false);
}

#[test]
fn thirty_third_receiver_refused() {
    let mut g = setup();
    for i in 1..MAX_RECEIVERS {
        g.allow_receiver(tst(), acc(&format!("r{i}.testnet")), false);
    }
    assert_eq!(g.get_token(tst()).unwrap().receivers.len(), MAX_RECEIVERS);
    assert_panics(ERR_TOO_MANY_RECEIVERS, || {
        g.allow_receiver(tst(), acc("r33.testnet"), false);
    });
    // Changing the flag of a receiver already listed still works when full.
    g.allow_receiver(tst(), alice(), true);
    assert_eq!(g.get_token(tst()).unwrap().receivers.len(), MAX_RECEIVERS);
}

#[test]
#[should_panic(expected = "token not configured")]
fn allow_receiver_unknown_token_panics() {
    let mut g = setup();
    g.allow_receiver(Token::Native, alice(), false);
}

#[test]
#[should_panic(expected = "ft_call needs an FT token")]
fn allow_receiver_refuses_ft_call_for_native() {
    let mut g = setup();
    g.set_token(Token::Native, U128(10), U128(10), U128(5));
    g.allow_receiver(Token::Native, alice(), true);
}

#[test]
fn allow_receiver_twice_updates_the_flag() {
    let mut g = setup();
    g.allow_receiver(tst(), alice(), true);
    assert_eq!(
        g.get_token(tst()).unwrap().receivers,
        vec![ReceiverRule {
            account_id: alice(),
            ft_call: true
        }]
    );
}

#[test]
fn disallow_receiver_removes_only_that_receiver() {
    let mut g = setup();
    g.allow_receiver(tst(), acc("bob.testnet"), true);
    g.disallow_receiver(tst(), alice());
    assert_eq!(
        g.get_token(tst()).unwrap().receivers,
        vec![ReceiverRule {
            account_id: acc("bob.testnet"),
            ft_call: true
        }]
    );
}

#[test]
#[should_panic(expected = "receiver not allowed")]
fn disallow_unknown_receiver_panics() {
    let mut g = setup();
    g.disallow_receiver(tst(), acc("bob.testnet"));
}

// ---------- set_agent and set_approver ----------

#[test]
fn set_agent_refuses_receiver_of_any_token() {
    let mut g = setup();
    g.set_token(Token::Native, U128(10), U128(10), U128(5));
    g.allow_receiver(Token::Native, acc("bob.testnet"), false);
    assert_panics(ERR_AGENT_IS_RECEIVER, || g.set_agent(alice()));
    assert_panics(ERR_AGENT_IS_RECEIVER, || g.set_agent(acc("bob.testnet")));
    assert_eq!(g.get_config().agent_id, agent());
}

#[test]
fn set_agent_refuses_owner_or_approver() {
    let mut g = setup();
    assert_panics(ERR_AGENT_IS_OWNER, || g.set_agent(owner()));
    assert_panics(ERR_AGENT_IS_APPROVER, || g.set_agent(approver()));
    assert_panics(ERR_AGENT_IS_GUARD, || g.set_agent(guard_id()));
    assert_eq!(g.get_config().agent_id, agent());
}

#[test]
fn set_agent_changes_the_agent() {
    let mut g = setup();
    g.set_agent(acc("agent2.testnet"));
    assert_eq!(g.get_config().agent_id, acc("agent2.testnet"));
    // The old agent is an ordinary account again and may be allowlisted.
    g.allow_receiver(tst(), agent(), false);
}

#[test]
#[should_panic(expected = "the agent cannot be the approver")]
fn set_approver_refuses_agent() {
    let mut g = setup();
    g.set_approver(agent());
}

#[test]
#[should_panic(expected = "the approver cannot be the guard")]
fn set_approver_refuses_guard() {
    let mut g = setup();
    g.set_approver(guard_id());
}

#[test]
fn set_approver_to_owner_allowed() {
    let mut g = setup();
    g.set_approver(owner());
    assert_eq!(g.get_config().approver_id, owner());
}

// ---------- pause and unpause ----------

#[test]
fn pause_by_approver_without_deposit() {
    let mut g = setup();
    call_as(&approver(), 0);
    g.pause();
    assert!(g.get_config().paused);
    assert_eq!(
        events(),
        vec![event("paused", json!({ "by": "approver.testnet" }))]
    );
}

#[test]
fn pause_by_owner_without_deposit() {
    let mut g = setup();
    call_as(&owner(), 0);
    g.pause();
    assert!(g.get_config().paused);
}

#[test]
fn pause_by_stranger_panics() {
    for caller in [stranger(), agent(), guard_id()] {
        let mut g = setup();
        call_as(&caller, 0);
        assert_panics(ERR_NOT_OWNER_OR_APPROVER, || g.pause());
        assert!(!g.get_config().paused);
    }
}

#[test]
fn pause_twice_emits_once() {
    let mut g = setup();
    call_as(&approver(), 0);
    g.pause();
    call_as(&owner(), 0);
    g.pause();
    assert!(g.get_config().paused);
    assert!(events().is_empty());
}

#[test]
fn unpause_requires_owner_yocto() {
    let mut g = setup();
    call_as(&approver(), 0);
    g.pause();
    call_as(&approver(), 1);
    assert_panics(ERR_NOT_OWNER, || g.unpause());
    call_as(&owner(), 0);
    assert_panics(ERR_ONE_YOCTO, || g.unpause());
    assert!(g.get_config().paused);
    call_as(&owner(), 1);
    g.unpause();
    assert!(!g.get_config().paused);
    assert_eq!(
        events(),
        vec![event("unpaused", json!({ "by": "owner.testnet" }))]
    );
    // Unpausing a running guard succeeds and emits nothing.
    call_as(&owner(), 1);
    g.unpause();
    assert!(events().is_empty());
}

// ---------- views ----------

#[test]
fn get_config_reports_the_limits() {
    let g = init();
    assert_eq!(
        serde_json::to_value(g.get_config().limits).unwrap(),
        json!({
            "window_slots": 25,
            "max_pending": 16,
            "max_receivers": 32,
            "max_tokens": 8,
            "max_msg_bytes": 1024,
            "max_expiry_ns": "604800000000000",
            "min_slot_ns": "1000000000",
        })
    );
}

#[test]
fn get_token_json() {
    let g = setup();
    assert_eq!(g.get_token(Token::Native), None);
    assert_eq!(
        serde_json::to_value(g.get_token(tst())).unwrap(),
        json!({
            "token": { "ft": "tst.nsg-token.testnet" },
            "policy": { "per_payment_max": "100", "daily_max": "100", "approval_above": "50" },
            "receivers": [{ "account_id": "alice.testnet", "ft_call": false }],
            "autonomous_spent": "0",
            "approved_spent": "0",
            "remaining": "100",
            "next_release_at": null,
        })
    );
}

// ---------- events ----------

#[test]
fn config_events_match_spec() {
    let mut g = init();
    let steps: [EventStep; 9] = [
        (
            |g| g.set_token(tst(), U128(100), U128(100), U128(50)),
            owner(),
            event(
                "config_changed",
                json!({ "change": "token", "token": { "ft": "tst.nsg-token.testnet" }, "account_id": null }),
            ),
        ),
        (
            |g| g.set_token(Token::Native, U128(10), U128(10), U128(5)),
            owner(),
            event(
                "config_changed",
                json!({ "change": "token", "token": "native", "account_id": null }),
            ),
        ),
        (
            |g| g.allow_receiver(tst(), alice(), true),
            owner(),
            event(
                "config_changed",
                json!({ "change": "receiver_allowed", "token": { "ft": "tst.nsg-token.testnet" }, "account_id": "alice.testnet" }),
            ),
        ),
        (
            |g| g.disallow_receiver(tst(), alice()),
            owner(),
            event(
                "config_changed",
                json!({ "change": "receiver_removed", "token": { "ft": "tst.nsg-token.testnet" }, "account_id": "alice.testnet" }),
            ),
        ),
        (
            |g| g.set_agent(acc("agent2.testnet")),
            owner(),
            event(
                "config_changed",
                json!({ "change": "agent", "token": null, "account_id": "agent2.testnet" }),
            ),
        ),
        (
            |g| g.set_approver(acc("approver2.testnet")),
            owner(),
            event(
                "config_changed",
                json!({ "change": "approver", "token": null, "account_id": "approver2.testnet" }),
            ),
        ),
        (
            |g| g.remove_token(tst()),
            owner(),
            event(
                "config_changed",
                json!({ "change": "token_removed", "token": { "ft": "tst.nsg-token.testnet" }, "account_id": null }),
            ),
        ),
        (
            |g| g.pause(),
            acc("approver2.testnet"),
            event("paused", json!({ "by": "approver2.testnet" })),
        ),
        (
            |g| g.unpause(),
            owner(),
            event("unpaused", json!({ "by": "owner.testnet" })),
        ),
    ];
    for (i, (call, caller, expected)) in steps.into_iter().enumerate() {
        let yocto = if caller == owner() { 1 } else { 0 };
        call_as(&caller, yocto);
        call(&mut g);
        assert_eq!(events(), vec![expected], "step {i}");
    }
}

#[test]
fn refused_config_call_emits_nothing_it_keeps() {
    // A panicking call's logs never reach a successful receipt; here we only
    // check that every check runs before the event is emitted.
    let mut g = setup();
    call_as(&owner(), 1);
    assert_panics(ERR_AGENT_IS_RECEIVER, || {
        g.allow_receiver(tst(), agent(), false)
    });
    assert!(get_logs().is_empty());
    assert_panics(ERR_TOO_MANY_TOKENS, || {
        for i in 0..=MAX_TOKENS {
            g.set_token(
                Token::Ft(acc(&format!("x{i}.testnet"))),
                U128(1),
                U128(1),
                U128(1),
            );
        }
    });
    assert_eq!(
        get_logs().len(),
        (MAX_TOKENS - 1) as usize,
        "one event per token added, none for the refused one"
    );
}
