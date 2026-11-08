//! The NEP-141 test token (TST, 6 decimals), for tests and the testnet demo.
//!
//! Built on `near-contract-standards`, with one switch, set by the owner, that
//! makes a transfer fail or return something unusual, so the guard's handling
//! of failed, partly refunded and oddly answered transfers is tested rather
//! than assumed. The same contract, deployed a second time, is the receiver of
//! `ft_transfer_call`: its `ft_on_transfer` does what `msg` says.

use near_contract_standards::fungible_token::FungibleToken;
use near_contract_standards::fungible_token::core::FungibleTokenCore;
use near_contract_standards::fungible_token::events::FtMint;
use near_contract_standards::fungible_token::metadata::{
    FT_METADATA_SPEC, FungibleTokenMetadata, FungibleTokenMetadataProvider,
};
use near_contract_standards::storage_management::{
    StorageBalance, StorageBalanceBounds, StorageManagement,
};
use near_sdk::json_types::U128;
use near_sdk::serde_json::{self, Value};
use near_sdk::{AccountId, NearToken, PanicOnDefault, PromiseOrValue, env, near, require};

pub const ERR_NOT_OWNER: &str = "only the owner can set the switch";
pub const ERR_FAIL_TRANSFER: &str = "test token: transfer failed on purpose";
pub const ERR_FAIL_RESOLVE: &str = "test token: resolve failed on purpose";
pub const ERR_FAIL_ON_TRANSFER: &str = "test receiver: ft_on_transfer failed on purpose";
pub const ERR_UNKNOWN_MSG: &str = "test receiver: msg must be keep, refund:<n> or fail";

/// 70 bytes, above the guard's 64-byte bound. Read in full it would parse as
/// 5, so a guard that read past its bound would count a refund of 5.
pub const LONG_RESULT: &str = concat!(
    "\"",
    "00000000000000000000000000000000000000000000000000000000000000000005",
    "\""
);
/// Valid JSON, but not a `U128`.
pub const GARBAGE_RESULT: &str = "\"not a number\"";

/// What the token does wrong, if anything. One at a time, set by the owner.
#[near(serializers = [borsh, json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Switch {
    /// A correct NEP-141 token.
    #[default]
    Off,
    /// `ft_transfer` and `ft_transfer_call` panic before moving anything.
    FailTransfer,
    /// `ft_resolve_transfer` panics after the tokens moved (decision 5).
    FailResolve,
    /// The transfer completes, then the last receipt returns `LONG_RESULT`:
    /// `ft_transfer` itself, or `ft_resolve_transfer` after its refund
    /// (decision 13).
    LongResult,
    /// `ft_resolve_transfer` refunds as usual, then returns `GARBAGE_RESULT`.
    GarbageResult,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct TestToken {
    owner_id: AccountId,
    switch: Switch,
    token: FungibleToken,
    metadata: FungibleTokenMetadata,
}

#[near]
impl TestToken {
    /// Mints `total_supply` to the owner. The receiver deployment uses 0.
    /// Private, so on testnet nobody else can initialise a freshly deployed
    /// token and mint the supply to themselves.
    #[init]
    #[private]
    pub fn new(owner_id: AccountId, total_supply: U128) -> Self {
        let mut token = FungibleToken::new(b"a".to_vec());
        token.internal_register_account(&owner_id);
        token.internal_deposit(&owner_id, total_supply.0);
        FtMint {
            owner_id: &owner_id,
            amount: total_supply,
            memo: Some("test token"),
        }
        .emit();
        Self {
            owner_id,
            switch: Switch::Off,
            token,
            metadata: FungibleTokenMetadata {
                spec: FT_METADATA_SPEC.to_string(),
                name: "near-spend-guard test token".to_string(),
                symbol: "TST".to_string(),
                icon: None,
                reference: None,
                reference_hash: None,
                decimals: 6,
            },
        }
    }

    pub fn set_switch(&mut self, switch: Switch) {
        require!(
            env::predecessor_account_id() == self.owner_id,
            ERR_NOT_OWNER
        );
        self.switch = switch;
    }

    pub fn get_switch(&self) -> Switch {
        self.switch
    }

    // ---------- NEP-141, with the switch ----------

    #[payable]
    pub fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        require!(self.switch != Switch::FailTransfer, ERR_FAIL_TRANSFER);
        self.token.ft_transfer(receiver_id, amount, memo);
        if self.switch == Switch::LongResult {
            env::value_return(LONG_RESULT.as_bytes());
        }
    }

    #[payable]
    pub fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<U128> {
        require!(self.switch != Switch::FailTransfer, ERR_FAIL_TRANSFER);
        self.token.ft_transfer_call(receiver_id, amount, memo, msg)
    }

    /// The standard resolve step, then the switch. Returns raw JSON so the
    /// switch can answer with something that isn't a `U128`.
    #[private]
    pub fn ft_resolve_transfer(
        &mut self,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: U128,
    ) -> Value {
        require!(self.switch != Switch::FailResolve, ERR_FAIL_RESOLVE);
        let (used, _burned) =
            self.token
                .internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        resolve_result(self.switch, used)
    }

    pub fn ft_total_supply(&self) -> U128 {
        self.token.ft_total_supply()
    }

    pub fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.token.ft_balance_of(account_id)
    }

    // ---------- receiver side ----------

    /// For the deployment that receives `ft_transfer_call`. `msg` says what
    /// to do: `keep` uses everything, `refund:<n>` hands `n` back, `fail`
    /// panics (the token then refunds everything).
    pub fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        let _ = (sender_id, amount);
        PromiseOrValue::Value(U128(on_transfer_unused(&msg)))
    }
}

/// What `ft_resolve_transfer` returns: the used amount as NEP-141 says, or
/// the switch's odd answer.
pub fn resolve_result(switch: Switch, used: u128) -> Value {
    match switch {
        Switch::LongResult => serde_json::from_str(LONG_RESULT).expect("valid JSON"),
        Switch::GarbageResult => serde_json::from_str(GARBAGE_RESULT).expect("valid JSON"),
        _ => Value::String(used.to_string()),
    }
}

/// The unused amount `ft_on_transfer` reports for `msg`.
pub fn on_transfer_unused(msg: &str) -> u128 {
    match msg {
        "keep" => 0,
        "fail" => env::panic_str(ERR_FAIL_ON_TRANSFER),
        _ => msg
            .strip_prefix("refund:")
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| env::panic_str(ERR_UNKNOWN_MSG)),
    }
}

#[near]
impl StorageManagement for TestToken {
    #[payable]
    fn storage_deposit(
        &mut self,
        account_id: Option<AccountId>,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        self.token.storage_deposit(account_id, registration_only)
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.token.storage_withdraw(amount)
    }

    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        self.token.internal_storage_unregister(force).is_some()
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.token.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.token.storage_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenMetadataProvider for TestToken {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        self.metadata.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::testing_env;

    fn acc(id: &str) -> AccountId {
        id.parse().unwrap()
    }

    fn call_as(predecessor: &str, yocto: u128) {
        testing_env!(
            VMContextBuilder::new()
                .current_account_id(acc("tst.test.near"))
                .predecessor_account_id(acc(predecessor))
                .attached_deposit(NearToken::from_yoctonear(yocto))
                .build()
        );
    }

    fn token() -> TestToken {
        call_as("owner.test.near", 0);
        TestToken::new(acc("owner.test.near"), U128(1_000_000_000))
    }

    #[test]
    fn new_mints_to_owner_with_metadata() {
        let t = token();
        assert_eq!(t.ft_balance_of(acc("owner.test.near")).0, 1_000_000_000);
        assert_eq!(t.ft_total_supply().0, 1_000_000_000);
        let m = t.ft_metadata();
        assert_eq!((m.symbol.as_str(), m.decimals), ("TST", 6));
        assert_eq!(t.get_switch(), Switch::Off);
    }

    #[test]
    #[should_panic(expected = "only the owner can set the switch")]
    fn only_owner_sets_switch() {
        let mut t = token();
        call_as("stranger.test.near", 0);
        t.set_switch(Switch::FailTransfer);
    }

    #[test]
    fn owner_sets_and_clears_switch() {
        let mut t = token();
        t.set_switch(Switch::FailResolve);
        assert_eq!(t.get_switch(), Switch::FailResolve);
        t.set_switch(Switch::Off);
        assert_eq!(t.get_switch(), Switch::Off);
    }

    #[test]
    fn switch_json_names() {
        let names: Vec<String> = [
            Switch::Off,
            Switch::FailTransfer,
            Switch::FailResolve,
            Switch::LongResult,
            Switch::GarbageResult,
        ]
        .iter()
        .map(|s| serde_json::to_string(s).unwrap())
        .collect();
        assert_eq!(
            names,
            [
                "\"off\"",
                "\"fail_transfer\"",
                "\"fail_resolve\"",
                "\"long_result\"",
                "\"garbage_result\""
            ]
        );
    }

    #[test]
    fn transfer_moves_tokens_when_off() {
        let mut t = token();
        t.token.internal_register_account(&acc("guard.test.near"));
        call_as("owner.test.near", 1);
        t.ft_transfer(acc("guard.test.near"), U128(25), None);
        assert_eq!(t.ft_balance_of(acc("guard.test.near")).0, 25);
    }

    #[test]
    #[should_panic(expected = "test token: transfer failed on purpose")]
    fn fail_transfer_panics_ft_transfer() {
        let mut t = token();
        t.set_switch(Switch::FailTransfer);
        call_as("owner.test.near", 1);
        t.ft_transfer(acc("owner.test.near"), U128(1), None);
    }

    #[test]
    #[should_panic(expected = "test token: transfer failed on purpose")]
    fn fail_transfer_panics_ft_transfer_call() {
        let mut t = token();
        t.set_switch(Switch::FailTransfer);
        call_as("owner.test.near", 1);
        let _ = t.ft_transfer_call(acc("shop.test.near"), U128(1), None, "keep".into());
    }

    #[test]
    #[should_panic(expected = "test token: resolve failed on purpose")]
    fn fail_resolve_panics() {
        let mut t = token();
        t.set_switch(Switch::FailResolve);
        call_as("tst.test.near", 0);
        let _ = t.ft_resolve_transfer(acc("owner.test.near"), acc("shop.test.near"), U128(1));
    }

    #[test]
    fn long_result_is_over_the_guards_bound_and_would_read_as_5() {
        // The guard's MAX_RESULT_BYTES is 64; written out, not imported.
        assert_eq!(LONG_RESULT.len(), 70);
        let read: U128 = serde_json::from_str(LONG_RESULT).unwrap();
        assert_eq!(read.0, 5);
        assert!(serde_json::from_str::<U128>(GARBAGE_RESULT).is_err());
    }

    #[test]
    fn resolve_result_per_switch() {
        assert_eq!(resolve_result(Switch::Off, 7), Value::String("7".into()));
        assert_eq!(
            resolve_result(Switch::FailTransfer, 7),
            Value::String("7".into())
        );
        assert_eq!(
            serde_json::to_string(&resolve_result(Switch::LongResult, 7)).unwrap(),
            LONG_RESULT
        );
        assert_eq!(
            serde_json::to_string(&resolve_result(Switch::GarbageResult, 7)).unwrap(),
            GARBAGE_RESULT
        );
    }

    #[test]
    fn on_transfer_msgs() {
        assert_eq!(on_transfer_unused("keep"), 0);
        assert_eq!(on_transfer_unused("refund:7"), 7);
        assert_eq!(on_transfer_unused("refund:0"), 0);
    }

    #[test]
    #[should_panic(expected = "test receiver: ft_on_transfer failed on purpose")]
    fn on_transfer_fail() {
        token();
        on_transfer_unused("fail");
    }

    #[test]
    #[should_panic(expected = "test receiver: msg must be keep, refund:<n> or fail")]
    fn on_transfer_unknown_msg() {
        token();
        on_transfer_unused("refund:-1");
    }
}
