//! Engine tests for Launch Commitments, run in-process on the real Ootle engine with the real
//! Lockbox template (compiled from `../ootle-lockbox`, the sibling checkout).
//!
//! Composition gate: one transaction pays the fee, creates the token, locks allocations through
//! Lockbox, creates the launch record and hands back the remainder; any failure undoes all of it.

use tari_template_lib::prelude::{Amount, ComponentAddress, Metadata, ResourceAddress, RistrettoPublicKeyBytes};
use tari_template_test_tooling::byte_type::ToByteType;
use tari_template_test_tooling::crypto::{PublicKey, RistrettoPublicKey, RistrettoSecretKey, SecretKey};
use tari_template_test_tooling::engine_types::commit_result::ExecuteResult;
use tari_template_test_tooling::engine_types::component::derive_component_address_from_public_key;
use tari_template_test_tooling::engine_types::virtual_substate::{VirtualSubstate, VirtualSubstateId};
use tari_template_test_tooling::template_lib_types::constants::TARI_TOKEN;
use tari_template_test_tooling::template_lib_types::{NonFungibleAddress, TemplateAddress};
use tari_template_test_tooling::transaction::{Transaction, args};
use tari_template_test_tooling::{Package, TemplateTest};

const LAUNCH_FEE: Amount = Amount::new(2_500 * 1_000_000);
const SUPPLY: u128 = 1_000_000 * 10u128.pow(18);
const TEAM: u128 = 200_000 * 10u128.pow(18);
const ADVISORS: u128 = 50_000 * 10u128.pow(18);
const NOW: u64 = 10;

type Account = (ComponentAddress, NonFungibleAddress, RistrettoSecretKey);

struct Env {
    test: TemplateTest,
    lockbox: TemplateAddress,
    launcher: TemplateAddress,
    probe: TemplateAddress,
    fee_account: ComponentAddress,
    creator: Account,
}

/// Compiles Lockbox, then Launch Commitments pinned to that Lockbox and to a fee account derived
/// from a fixed key (so the build is cached across runs), and creates the fee account and a
/// creator holding enough TARI for the fee.
fn setup() -> Env {
    let fee_key = RistrettoSecretKey::from_uniform_bytes(&[7u8; 64]).unwrap();
    let fee_public: RistrettoPublicKeyBytes = RistrettoPublicKey::from_secret_key(&fee_key).to_byte_type();
    let fee_account = derive_component_address_from_public_key(&TemplateAddress::from_array([0; 32]), &fee_public);

    let mut builder = Package::builder();
    builder.add_all_builtin_templates();
    let lockbox = builder.add_template("../ootle-lockbox");
    let launcher = builder.add_template_opts(".", &[], [
        ("LOCKBOX_TEMPLATE", lockbox.to_string()),
        ("FEE_ACCOUNT", fee_account.to_string()),
    ]);
    let probe = builder.add_template("tests/templates/probe");
    let mut test = TemplateTest::from_package(builder.build());
    test.bootstrap_state();
    test.set_virtual_substate(VirtualSubstateId::CurrentEpoch, VirtualSubstate::CurrentEpoch(NOW));

    let created = test.create_account(fee_public, None, vec![NonFungibleAddress::from_public_key(fee_public)]);
    assert_eq!(created, fee_account, "the fee account must sit at the address the template was built with");

    let creator = test.create_funded_account();
    // A funded account starts with 1,000 TARI and the faucet pays once per account, but the fee
    // is 2,500: two more funded accounts send theirs to the creator.
    for _ in 0..2 {
        let donor = test.create_funded_account();
        test.execute_expect_success(
            test.transaction()
                .call_method(donor.0, "withdraw", args![TARI_TOKEN, Amount::from(TemplateTest::FUNDED_ACCOUNT_INITIAL_BALANCE)])
                .put_last_instruction_output_on_workspace("tari")
                .call_method(creator.0, "deposit", args![Workspace("tari")])
                .build_and_seal(&donor.2),
            vec![donor.1.clone()],
        );
    }
    Env { test, lockbox, launcher, probe, fee_account, creator }
}

fn key(seed: u8) -> RistrettoPublicKeyBytes {
    let secret = RistrettoSecretKey::from_uniform_bytes(&[seed; 64]).unwrap();
    RistrettoPublicKey::from_secret_key(&secret).to_byte_type()
}

/// The launch inputs other than the fee; `Inputs::default()` is a valid launch.
#[derive(Clone)]
struct Inputs {
    symbol: String,
    name: String,
    image_url: String,
    supply: Amount,
    locks: Vec<(Amount, u64, RistrettoPublicKeyBytes)>,
}

impl Default for Inputs {
    fn default() -> Self {
        Inputs {
            symbol: "TEST".into(),
            name: "Test Token".into(),
            image_url: "https://example.com/test.png".into(),
            supply: Amount::new(SUPPLY),
            locks: two_locks(),
        }
    }
}

/// A launch transaction: withdraw `fee` TARI, launch, and (if `deposit`) deposit the remainder.
fn launch_tx_with(env: &Env, fee: Amount, inputs: Inputs, deposit: bool) -> Transaction {
    let Inputs { symbol, name, image_url, supply, locks } = inputs;
    let tx = env
        .test
        .transaction()
        .call_method(env.creator.0, "withdraw", args![TARI_TOKEN, fee])
        .put_last_instruction_output_on_workspace("fee")
        .call_function(env.launcher, "launch", args![Workspace("fee"), symbol, name, image_url, supply, locks])
        .put_last_instruction_output_on_workspace("rest");
    let tx = if deposit { tx.call_method(env.creator.0, "deposit", args![Workspace("rest")]) } else { tx };
    tx.build_and_seal(&env.creator.2)
}

fn launch_tx(env: &Env, fee: Amount, locks: Vec<(Amount, u64, RistrettoPublicKeyBytes)>, deposit: bool) -> Transaction {
    launch_tx_with(env, fee, Inputs { locks, ..Inputs::default() }, deposit)
}

fn two_locks() -> Vec<(Amount, u64, RistrettoPublicKeyBytes)> {
    vec![(Amount::new(TEAM), 100, key(1)), (Amount::new(ADVISORS), 200, key(2))]
}

fn balance(env: &mut Env, account: ComponentAddress, resource: ResourceAddress) -> Amount {
    env.test.call_method(account, "balance", args![resource], vec![])
}

/// The events with this topic suffix emitted by `template`.
fn events<'a>(result: &'a ExecuteResult, template: TemplateAddress, topic: &str) -> Vec<&'a Metadata> {
    result
        .finalize
        .events
        .iter()
        .filter(|e| e.topic().ends_with(topic) && *e.template_address() == template)
        .map(|e| e.payload())
        .collect()
}

#[test]
fn one_transaction_creates_token_locks_and_record() {
    let mut env = setup();
    let (fee_account, creator) = (env.fee_account, env.creator.0);
    let fee_before = balance(&mut env, fee_account, TARI_TOKEN);
    let tx = launch_tx(&env, LAUNCH_FEE, two_locks(), true);
    let result = env.test.execute_expect_success(tx, vec![env.creator.1.clone()]);

    // The fee landed in the fee account.
    assert_eq!(balance(&mut env, fee_account, TARI_TOKEN), fee_before + LAUNCH_FEE);

    // Each nested lock was created by Lockbox; the launch event by this template.
    let locks = events(&result, env.lockbox, "LockCreated");
    assert_eq!(locks.len(), 2, "two LockCreated events from Lockbox: {:?}", result.finalize.events);
    let launched = events(&result, env.launcher, "LaunchCreated");
    assert_eq!(launched.len(), 1, "one LaunchCreated from this template: {:?}", result.finalize.events);
    let record: ComponentAddress = launched[0].get_str("record").unwrap().parse().unwrap();
    let resource: ResourceAddress = launched[0].get_str("resource").unwrap().parse().unwrap();

    // The creator holds exactly the unlocked remainder; the locks hold the rest.
    assert_eq!(balance(&mut env, creator, resource), Amount::new(SUPPLY - TEAM - ADVISORS));
    for (event, (amount, epoch, beneficiary)) in locks.iter().zip(two_locks()) {
        assert_eq!(event.get_str("resource").unwrap(), resource.to_string());
        assert_eq!(event.get_str("amount").unwrap(), amount.to_string());
        assert_eq!(event.get_str("unlock_epoch").unwrap(), epoch.to_string());
        assert_eq!(event.get_str("beneficiary").unwrap(), beneficiary.to_string());
    }

    // The record names the token, the supply and the same locks, in order.
    type Info = (ResourceAddress, Amount, Vec<(ComponentAddress, Amount, u64, RistrettoPublicKeyBytes)>, u64);
    let (info_resource, info_supply, info_locks, created_at): Info = env.test.call_method(record, "info", args![], vec![]);
    assert_eq!((info_resource, info_supply, created_at), (resource, Amount::new(SUPPLY), NOW));
    let expected: Vec<_> = locks
        .iter()
        .zip(two_locks())
        .map(|(event, (amount, epoch, beneficiary))| (event.get_str("lock").unwrap().parse().unwrap(), amount, epoch, beneficiary))
        .collect();
    assert_eq!(info_locks, expected);
}

#[test]
fn a_failure_after_the_fee_undoes_everything() {
    let mut env = setup();
    let (fee_account, creator) = (env.fee_account, env.creator.0);
    let fee_before = balance(&mut env, fee_account, TARI_TOKEN);
    let creator_before = balance(&mut env, creator, TARI_TOKEN);

    // Lock 2's epoch is in the past: rejected after the fee was already deposited.
    let bad = vec![(Amount::new(TEAM), 100, key(1)), (Amount::new(ADVISORS), NOW, key(2))];
    let reason = env.test.execute_expect_failure(launch_tx(&env, LAUNCH_FEE, bad, true), vec![env.creator.1.clone()]);
    assert!(format!("{reason:?}").contains("must be after the current epoch"), "{reason:?}");

    // The remainder left undeposited: the transaction fails after every lock was made.
    let reason = env.test.execute_expect_failure(launch_tx(&env, LAUNCH_FEE, two_locks(), false), vec![env.creator.1.clone()]);
    let _ = reason;

    assert_eq!(balance(&mut env, fee_account, TARI_TOKEN), fee_before, "no fee may be kept");
    assert_eq!(balance(&mut env, creator, TARI_TOKEN), creator_before, "the creator keeps their TARI");
}

#[test]
fn the_fee_must_be_exact() {
    let mut env = setup();
    for fee in [LAUNCH_FEE - Amount::new(1), LAUNCH_FEE + Amount::new(1)] {
        let reason = env.test.execute_expect_failure(launch_tx(&env, fee, two_locks(), true), vec![env.creator.1.clone()]);
        assert!(format!("{reason:?}").contains("The fee is exactly"), "{reason:?}");
    }
}

/// Runs and commits a transaction signed by the creator, the signer of both tokens' creation
/// transactions. Err holds the rejection.
fn as_creator(env: &mut Env, build: impl FnOnce(&Env) -> Transaction) -> Result<ExecuteResult, String> {
    let tx = build(env);
    let proofs = vec![env.creator.1.clone()];
    let result = env.test.execute_and_commit_on_success(tx, proofs);
    match result.finalize.result.any_reject() {
        None => Ok(result),
        Some(reason) => Err(reason.to_string()),
    }
}

/// Launches a token and parks 1,000 of it in a probe component; returns (resource, probe component).
fn launched_token(env: &mut Env) -> (ResourceAddress, ComponentAddress) {
    let result = env.test.execute_expect_success(launch_tx(env, LAUNCH_FEE, two_locks(), true), vec![env.creator.1.clone()]);
    let resource: ResourceAddress = events(&result, env.launcher, "LaunchCreated")[0].get_str("resource").unwrap().parse().unwrap();
    let (creator, probe) = (env.creator.0, env.probe);
    as_creator(env, |env| {
        env.test
            .transaction()
            .call_method(creator, "withdraw", args![resource, Amount::new(1_000)])
            .put_last_instruction_output_on_workspace("t")
            .call_function(probe, "hold", args![Workspace("t")])
            .build_and_seal(&env.creator.2)
    })
    .unwrap();
    (resource, env.test.expect_return::<ComponentAddress>(2))
}

/// An owned twin with the same rules, created by the same signer: (resource, probe component).
fn owned_twin(env: &mut Env) -> (ResourceAddress, ComponentAddress) {
    let probe = env.probe;
    as_creator(env, |env| env.test.transaction().call_function(probe, "twin", args![Amount::new(SUPPLY)]).build_and_seal(&env.creator.2))
        .unwrap();
    let holder: ComponentAddress = env.test.expect_return(0);
    let resource: ResourceAddress = env.test.call_method(holder, "resource_address", args![], vec![]);
    (resource, holder)
}

/// Tries each privileged action on `resource` (held in `holder`); returns the outcome per action.
fn try_privileged_actions(env: &mut Env, resource: ResourceAddress, holder: ComponentAddress) -> Vec<(&'static str, Result<(), String>)> {
    let (creator, probe) = (env.creator.0, env.probe);
    let mut outcomes = vec![];
    let mut attempt = |env: &mut Env, name: &'static str, build: &dyn Fn(&Env) -> Transaction| {
        outcomes.push((name, as_creator(env, |env| build(env)).map(|_| ())));
    };
    attempt(env, "mint", &|env| {
        env.test
            .transaction()
            .call_function(probe, "mint", args![resource, Amount::new(1)])
            .put_last_instruction_output_on_workspace("m")
            .call_method(creator, "deposit", args![Workspace("m")])
            .build_and_seal(&env.creator.2)
    });
    attempt(env, "burn", &|env| {
        env.test
            .transaction()
            .call_method(holder, "take", args![Amount::new(1)])
            .put_last_instruction_output_on_workspace("b")
            .call_function(probe, "burn", args![Workspace("b")])
            .build_and_seal(&env.creator.2)
    });
    attempt(env, "metadata", &|env| env.test.transaction().call_function(probe, "set_metadata", args![resource]).build_and_seal(&env.creator.2));
    attempt(env, "rule update", &|env| env.test.transaction().call_function(probe, "open_minting", args![resource]).build_and_seal(&env.creator.2));
    attempt(env, "hook", &|env| env.test.transaction().call_method(holder, "install_hook", args![resource]).build_and_seal(&env.creator.2));
    attempt(env, "freeze", &|env| env.test.transaction().call_method(holder, "freeze", args![]).build_and_seal(&env.creator.2));
    attempt(env, "recall", &|env| {
        env.test
            .transaction()
            .call_method(holder, "recall", args![])
            .put_last_instruction_output_on_workspace("r")
            .call_method(creator, "deposit", args![Workspace("r")])
            .build_and_seal(&env.creator.2)
    });
    outcomes
}

#[test]
fn nobody_can_change_a_launched_token_but_its_owned_twin_can_be() {
    let mut env = setup();
    let (twin, twin_holder) = owned_twin(&mut env);
    let twin_outcomes = try_privileged_actions(&mut env, twin, twin_holder);
    // Positive control: the owner bypass is real, so these work on the owned twin.
    for (name, outcome) in &twin_outcomes {
        if ["mint", "metadata", "freeze"].contains(name) {
            assert!(outcome.is_ok(), "the twin's owner should be able to {name}: {outcome:?}");
        }
    }

    let (token, holder) = launched_token(&mut env);
    for (name, outcome) in try_privileged_actions(&mut env, token, holder) {
        let reason = outcome.expect_err(&format!("nobody may {name} a launched token, its creator included"));
        assert!(reason.contains("AccessDenied"), "{name} must be refused by an access rule: {reason}");
    }

    // Ordinary transfers still work.
    let other = env.test.create_empty_account();
    let creator = env.creator.0;
    as_creator(&mut env, |env| {
        env.test
            .transaction()
            .call_method(creator, "withdraw", args![token, Amount::new(5)])
            .put_last_instruction_output_on_workspace("t")
            .call_method(other.0, "deposit", args![Workspace("t")])
            .build_and_seal(&env.creator.2)
    })
    .unwrap();
    assert_eq!(balance(&mut env, other.0, token), Amount::new(5));
}

#[test]
fn launch_rejects_bad_inputs() {
    let mut env = setup();
    let lock = |amount: u128, epoch: u64| (Amount::new(amount), epoch, key(1));
    let cases: Vec<(Inputs, &str)> = vec![
        (Inputs { locks: vec![], ..Default::default() }, "Give 1-8 locks"),
        (Inputs { locks: vec![lock(1, 100); 9], ..Default::default() }, "Give 1-8 locks"),
        (Inputs { locks: vec![lock(SUPPLY, 100), lock(1, 100)], ..Default::default() }, "more than the supply"),
        (Inputs { locks: vec![lock(0, 100)], ..Default::default() }, "positive amount"),
        (Inputs { locks: vec![lock(1, NOW + 530_001)], ..Default::default() }, "epochs away"),
        (Inputs { supply: Amount::zero(), ..Default::default() }, "Supply must be positive"),
        (Inputs { symbol: "".into(), ..Default::default() }, "Symbol must be"),
        (Inputs { symbol: "S".repeat(13), ..Default::default() }, "Symbol must be"),
        (Inputs { name: "N".repeat(65), ..Default::default() }, "Name must be"),
        (Inputs { image_url: format!("https://{}", "u".repeat(250)), ..Default::default() }, "Image URL is longer"),
        (Inputs { image_url: "http://example.com/x.png".into(), ..Default::default() }, "must start with https://"),
        (Inputs { image_url: "javascript:alert(1)".into(), ..Default::default() }, "must start with https://"),
    ];
    for (inputs, expected) in cases {
        let reason = env.test.execute_expect_failure(launch_tx_with(&env, LAUNCH_FEE, inputs, true), vec![env.creator.1.clone()]);
        assert!(format!("{reason:?}").contains(expected), "expected {expected:?}, got {reason:?}");
    }

    // Eight locks summing exactly to the supply is the largest valid launch; nothing remains.
    let eight = vec![(Amount::new(SUPPLY / 8), 100, key(1)); 8];
    env.test.execute_expect_success(launch_tx_with(&env, LAUNCH_FEE, Inputs { locks: eight, ..Default::default() }, true), vec![env.creator.1.clone()]);
}

#[test]
fn the_fee_must_be_tari() {
    let mut env = setup();
    // The owned twin stands in for any token that isn't TARI.
    let (_, holder) = owned_twin(&mut env);
    let tx = env
        .test
        .transaction()
        .call_method(holder, "take", args![LAUNCH_FEE])
        .put_last_instruction_output_on_workspace("fee")
        .call_function(env.launcher, "launch", args![Workspace("fee"), "TEST", "Test Token", "", Amount::new(SUPPLY), two_locks()])
        .put_last_instruction_output_on_workspace("rest")
        .call_method(env.creator.0, "deposit", args![Workspace("rest")])
        .build_and_seal(&env.creator.2);
    let reason = env.test.execute_expect_failure(tx, vec![env.creator.1.clone()]);
    assert!(format!("{reason:?}").contains("must be paid in TARI"), "{reason:?}");
}
