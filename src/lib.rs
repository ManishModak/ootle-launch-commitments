//! Launch Commitments: a token factory on Tari Ootle that charges a flat fee per launch.
//!
//! `LaunchRecord::launch` does everything in one transaction:
//! 1. takes exactly `LAUNCH_FEE` native TARI and deposits it into the fee account;
//! 2. creates a fixed-supply public token that **nobody can ever change**: no owner, every rule
//!    and updater locked, only withdraw and deposit open, no transfer hook;
//! 3. locks the creator's chosen allocations in Lockbox (a separate, ownerless template);
//! 4. creates an ownerless, read-only `LaunchRecord` that the token's `launched_by` metadata
//!    points to, and emits `LaunchCreated`;
//! 5. returns the rest of the supply for the caller to deposit in the same transaction.
//!
//! Any failure rolls the whole launch back, fee included.
//!
//! Two addresses are fixed at build time, so nobody can deploy a fee-free or Lockbox-free copy
//! under this template's address:
//! - `LOCKBOX_TEMPLATE`: the Lockbox template address (64 hex);
//! - `FEE_ACCOUNT`: the account that receives fees (`component_<64 hex>`).
//!
//! Build with both set, e.g. `LOCKBOX_TEMPLATE=… FEE_ACCOUNT=… cargo build --release --target wasm32-unknown-unknown`.

use tari_template_lib::prelude::*;

/// Flat fee per launch: 2,500 TARI (native TARI has 6 decimals).
pub const LAUNCH_FEE: Amount = Amount::new(2_500 * 1_000_000);
/// Every launched token has 18 decimals; amounts are in atomic units.
const DIVISIBILITY: u8 = 18;
const MAX_LOCKS: usize = 8;
const MAX_SYMBOL_BYTES: usize = 12;
const MAX_NAME_BYTES: usize = 64;
const MAX_IMAGE_URL_BYTES: usize = 256;
/// Locks may open at most this many epochs ahead: ~20 years at mainnet's ~20-minute epochs.
const MAX_LOCK_EPOCHS: u64 = 530_000;

fn lockbox_template() -> TemplateAddress {
    let hex = option_env!("LOCKBOX_TEMPLATE").expect("built without LOCKBOX_TEMPLATE");
    TemplateAddress::from_hex(hex).expect("LOCKBOX_TEMPLATE is not 64 hex characters")
}

fn fee_account() -> ComponentAddress {
    let text = option_env!("FEE_ACCOUNT").expect("built without FEE_ACCOUNT");
    text.parse().expect("FEE_ACCOUNT is not a component address")
}

#[template]
mod launch_commitments {
    use super::*;

    pub struct LaunchRecord {
        resource: ResourceAddress,
        supply: Amount,
        locks: Vec<LaunchedLock>,
        created_at_epoch: u64,
    }

    impl LaunchRecord {
        /// Launches a token. `locks` lists `(amount, unlock_epoch, beneficiary_key)` allocations to
        /// lock in Lockbox; `amount`s and `supply` are in atomic units (18 decimals). Returns the
        /// unlocked remainder of the supply. Panics, undoing everything, on a wrong fee or any
        /// invalid input.
        pub fn launch(
            fee: Bucket,
            symbol: String,
            name: String,
            image_url: String,
            supply: Amount,
            locks: Vec<(Amount, u64, RistrettoPublicKeyBytes)>,
        ) -> Bucket {
            take_fee(fee);
            let now = Consensus::current_epoch();
            check_inputs(&symbol, &name, &image_url, supply, &locks, now);

            // Allocate the record's address first so the token's metadata can name it.
            let allocation = CallerContext::allocate_component_address(None);
            let record = allocation.get_address();

            let mut tokens = ResourceBuilder::public_fungible()
                .with_owner_rule(OwnerRule::None)
                .with_access_rules(
                    ResourceAccessRules::deny_all()
                        .withdrawable(rule!(allow_all), UpdateRule::Locked)
                        .depositable(rule!(allow_all), UpdateRule::Locked),
                )
                .with_divisibility(DIVISIBILITY)
                .with_token_symbol(symbol.clone())
                .add_metadata("name", name)
                .with_image_url(image_url)
                .add_metadata("launched_by", record.to_string())
                .initial_supply(supply);
            let resource = tokens.resource_address();

            let lockbox = TemplateManager::get(lockbox_template());
            let memo = format!("launch {record}");
            let launched: Vec<LaunchedLock> = locks
                .into_iter()
                .map(|(amount, unlock_epoch, beneficiary)| {
                    let allocation = tokens.take(amount);
                    let lock: ComponentAddress =
                        lockbox.call("lock", tari_template_lib::invoke_args![allocation, unlock_epoch, beneficiary, memo.clone()]);
                    LaunchedLock { lock, amount, unlock_epoch, beneficiary }
                })
                .collect();

            emit_event("LaunchCreated", metadata![
                "record" => record.to_string(),
                "resource" => resource.to_string(),
                "symbol" => symbol,
                "supply" => supply.to_string(),
                "locks" => launched.len().to_string(),
                "created_at_epoch" => now.to_string(),
            ]);

            Component::new(Self { resource, supply, locks: launched, created_at_epoch: now })
                .with_address_allocation(allocation)
                .with_access_rules(ComponentAccessRules::new().method("info", rule!(allow_all)).default(rule!(deny_all)))
                .with_owner_rule(OwnerRule::None)
                .create();
            tokens
        }

        /// Read-only view of the launch for proof pages.
        pub fn info(&self) -> LaunchInfo {
            LaunchInfo {
                resource: self.resource,
                supply: self.supply,
                locks: self.locks.clone(),
                created_at_epoch: self.created_at_epoch,
            }
        }
    }

    /// One allocation locked at launch.
    #[derive(Clone)]
    pub struct LaunchedLock {
        pub lock: ComponentAddress,
        pub amount: Amount,
        pub unlock_epoch: u64,
        pub beneficiary: RistrettoPublicKeyBytes,
    }

    /// What `info` returns.
    pub struct LaunchInfo {
        pub resource: ResourceAddress,
        pub supply: Amount,
        pub locks: Vec<LaunchedLock>,
        pub created_at_epoch: u64,
    }
}

/// Accepts exactly `LAUNCH_FEE` of revealed native TARI and deposits it into the fee account.
fn take_fee(fee: Bucket) {
    assert!(fee.resource_address() == TARI_TOKEN, "The fee must be paid in TARI");
    fee.assert_contains_no_confidential_funds();
    assert!(fee.amount() == LAUNCH_FEE, "The fee is exactly {LAUNCH_FEE} (atomic TARI), got {}", fee.amount());
    ComponentManager::get(fee_account()).invoke("deposit", tari_template_lib::invoke_args![fee]);
}

fn check_inputs(
    symbol: &str,
    name: &str,
    image_url: &str,
    supply: Amount,
    locks: &[(Amount, u64, RistrettoPublicKeyBytes)],
    now: u64,
) {
    assert!(!symbol.is_empty() && symbol.len() <= MAX_SYMBOL_BYTES, "Symbol must be 1-{MAX_SYMBOL_BYTES} bytes");
    assert!(!name.is_empty() && name.len() <= MAX_NAME_BYTES, "Name must be 1-{MAX_NAME_BYTES} bytes");
    assert!(image_url.len() <= MAX_IMAGE_URL_BYTES, "Image URL is longer than {MAX_IMAGE_URL_BYTES} bytes");
    assert!(image_url.is_empty() || image_url.starts_with("https://"), "Image URL must start with https://");
    assert!(supply.is_positive(), "Supply must be positive");
    assert!((1..=MAX_LOCKS).contains(&locks.len()), "Give 1-{MAX_LOCKS} locks");
    let mut locked = Amount::zero();
    for (amount, unlock_epoch, _) in locks {
        assert!(amount.is_positive(), "Each lock needs a positive amount");
        assert!(*unlock_epoch > now, "Unlock epoch {unlock_epoch} must be after the current epoch {now}");
        assert!(*unlock_epoch - now <= MAX_LOCK_EPOCHS, "Unlock epoch {unlock_epoch} is more than {MAX_LOCK_EPOCHS} epochs away");
        locked = locked.checked_add(*amount).expect("Lock amounts overflow");
    }
    assert!(locked <= supply, "Locks total {locked}, more than the supply {supply}");
}
