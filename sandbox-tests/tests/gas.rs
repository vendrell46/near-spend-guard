//! The static gas the guard reserves, against what the receipts burn. A
//! callback that runs out of gas leaves its reservation in place (fails
//! closed: lost budget, never lost funds), but the guard should never get
//! there. Measured on the wasm `scripts/build-wasm.sh` builds, before
//! wasm-opt, so the deployable build burns the same or less.

mod common;

use common::*;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::Gas;
use serde_json::{Value, json};

/// The guard's own constants, imported rather than written out: the test is
/// that what the guard reserves keeps its headroom, so lowering a constant
/// must fail it.
const GAS_FT_TRANSFER: Gas = Gas::from_gas(near_spend_guard::GAS_FT_TRANSFER.as_gas());
const GAS_FT_TRANSFER_CALL: Gas = Gas::from_gas(near_spend_guard::GAS_FT_TRANSFER_CALL.as_gas());
const GAS_CALLBACK: Gas = Gas::from_gas(near_spend_guard::GAS_CALLBACK.as_gas());

/// Gas burnt by the receipts `executor` ran in this transaction, those whose
/// logs contain `marker` if one is given.
fn burnt(result: &ExecutionFinalResult, executor: &str, marker: Option<&str>) -> Gas {
    let total: u64 = result
        .receipt_outcomes()
        .iter()
        .filter(|o| o.executor_id.as_str() == executor)
        .filter(|o| marker.is_none_or(|m| o.logs.iter().any(|l| l.contains(m))))
        .map(|o| o.gas_burnt.as_gas())
        .sum();
    Gas::from_gas(total)
}

/// Asserts at least twice the measured gas is reserved, and prints both.
fn headroom(what: &str, measured: Gas, reserved: Gas) {
    eprintln!(
        "{what}: {:.2} TGas burnt, {} TGas reserved",
        measured.as_gas() as f64 / 1e12,
        reserved.as_tgas()
    );
    assert!(measured.as_gas() > 0, "{what}: no receipt measured");
    assert!(
        measured.as_gas() * 2 <= reserved.as_gas(),
        "{what}: {measured} burnt, under 2x headroom in {reserved}"
    );
}

#[tokio::test]
async fn callback_gas_headroom() -> anyhow::Result<()> {
    let env = setup().await?;
    allow(&env, env.shop.id(), true).await?;
    let tst = env.tst.id().as_str();
    let guard = env.guard.id().as_str();

    // ft_transfer, paid.
    let paid = request(&env, env.alice.id(), 5 * TST, None).await?;
    headroom(
        "ft_transfer",
        burnt(&paid, tst, Some("ft_transfer")),
        GAS_FT_TRANSFER,
    );
    headroom(
        "callback after ft_transfer",
        burnt(&paid, guard, Some("payment_settled")),
        GAS_CALLBACK,
    );
    // Not reserved by the guard: what the agent's own call burns before the
    // dispatch, for choosing the gas the agent attaches.
    eprintln!(
        "request_payment itself: {:.2} TGas burnt",
        burnt(&paid, guard, Some("payment_dispatched")).as_gas() as f64 / 1e12
    );
    assert_eq!(paid.json::<Value>()?["status"], "paid");

    // ft_transfer_call with a partial refund: the token's call, the shop's
    // ft_on_transfer and the token's resolve step all come out of the 60.
    let call = request(&env, env.shop.id(), 5 * TST, Some("refund:1000000")).await?;
    let token_side = Gas::from_gas(
        burnt(&call, tst, None).as_gas() + burnt(&call, env.shop.id().as_str(), None).as_gas(),
    );
    headroom(
        "ft_transfer_call, all three receipts",
        token_side,
        GAS_FT_TRANSFER_CALL,
    );
    headroom(
        "callback after ft_transfer_call",
        burnt(&call, guard, Some("payment_settled")),
        GAS_CALLBACK,
    );
    assert_eq!(call.json::<Value>()?["sent"], (4 * TST).to_string());

    // The callback's heaviest path: a failure that releases budget, which
    // rewrites the token's window.
    let root = env.worker.root_account()?;
    let bob = subaccount(&root, "bob", 5).await?;
    allow(&env, bob.id(), false).await?;
    let failed = request(&env, bob.id(), 5 * TST, None).await?;
    headroom(
        "callback releasing after a failed ft_transfer",
        burnt(&failed, guard, Some("payment_settled")),
        GAS_CALLBACK,
    );
    assert_eq!(failed.json::<Value>()?["status"], "failed");

    // Native NEAR.
    owner_call(
        &env,
        "set_token",
        json!({
            "token": "native",
            "per_payment_max": "1000000000000000000000000",
            "daily_max": "2000000000000000000000000",
            "approval_above": "1000000000000000000000000",
        }),
    )
    .await?
    .into_result()?;
    owner_call(
        &env,
        "allow_receiver",
        json!({ "token": "native", "receiver_id": env.alice.id(), "ft_call": false }),
    )
    .await?
    .into_result()?;
    let native = env
        .agent
        .call(env.guard.id(), "request_payment")
        .args_json(json!({
            "token": "native",
            "receiver_id": env.alice.id(),
            "amount": "100000000000000000000000",
            "msg": null,
            "expires_at": expiry(&env).await?,
        }))
        .gas(AGENT_GAS)
        .transact()
        .await?;
    headroom(
        "callback after a native transfer",
        burnt(&native, guard, Some("payment_settled")),
        GAS_CALLBACK,
    );
    assert_eq!(native.json::<Value>()?["status"], "paid");
    Ok(())
}
