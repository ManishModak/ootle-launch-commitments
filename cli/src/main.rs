//! launch-cli: publishes the Launch Commitments template and launches a token from a local key.
//!
//! Usage: `launch-cli <esmeralda|mainnet> <command> [args] [--submit]`
//!   address                       print the Ootle address, account and public key
//!   status                        current epoch and account balances
//!   publish <wasm>                publish the template
//!   launch <template> <fee_microtari> <symbol> <name> <supply> <lock>... [--image <https url>]
//!                                 launch a token; `supply` and lock amounts are whole tokens
//!                                 (18 decimals), each lock is `AMOUNT@UNLOCK_EPOCH[@BENEFICIARY_KEY]`
//!                                 (the beneficiary defaults to this key)
//!
//! Every transaction is dry-run first; `--submit` then sends that same transaction. It reserves
//! at most `MAX_FEE` for network fees and refuses a dry run that needs more. The launch and the
//! remainder deposit run in the main instructions, so any failure undoes the whole launch.
//! The key is Lockbox's: `~/.config/secrets/ootle-lockbox-<network>.key` (create it with
//! `lockbox-cli <network> init`); it is never printed.

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, str::FromStr, time::Duration};

use anyhow::{Context, Result, anyhow, bail, ensure};
use ootle_rs::{
    Network,
    ToAccountAddress,
    TransactionRequest,
    builtin_templates::{
        UnsignedTransactionBuilder,
        component::{ComponentInvokeBuilder, IComponent, TransactionBuildable},
    },
    default_indexer_url,
    key_provider::PrivateKeyProvider,
    keys::OotleSecretKey,
    provider::{IndexerProvider, ProviderBuilder, WalletProvider},
    template_types::{Amount, ComponentAddress, constants::TARI_TOKEN, crypto::RistrettoPublicKeyBytes},
    wallet::OotleWallet,
};
use tari_crypto::{
    ristretto::{RistrettoPublicKey, RistrettoSecretKey},
    tari_utilities::{ByteArray, hex::Hex},
};
use tari_ootle_common_types::engine_types::published_template::PublishedTemplateAddress;
use tari_ootle_transaction::{Epoch, args};

/// Most a transaction may reserve, and so spend, on network fees: 5 TARI, in µT.
const MAX_FEE: u64 = 5_000_000;
/// How many epochs ahead a transaction may still be sequenced.
const EPOCH_WINDOW: u64 = 10;
/// Launched tokens have 18 decimals.
const TOKEN_UNIT: u128 = 10u128.pow(18);

#[tokio::main]
async fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let submit = take_flag(&mut args, "--submit");
    let image_url = take_option(&mut args, "--image")?.unwrap_or_default();
    let [network, command, rest @ ..] = args.as_slice() else {
        bail!("usage: launch-cli <esmeralda|mainnet> <address|status|publish|launch> [args] [--submit]");
    };
    let network = match network.as_str() {
        "esmeralda" => Network::Esmeralda,
        "mainnet" => Network::MainNet,
        other => bail!("unknown network {other}"),
    };
    let secret = load_key(&key_path(network)?, network)?;
    let address = secret.to_address();
    let account = address.to_account_address();
    let public_key = *address.account_public_key();

    if command == "address" {
        println!("address:    {address}");
        println!("account:    {account}");
        println!("public key: {public_key}");
        return Ok(());
    }

    let provider = ProviderBuilder::new()
        .wallet(OotleWallet::from(PrivateKeyProvider::new(secret)))
        .connect_with_transaction_timeout(default_indexer_url(network), Duration::from_secs(180))
        .await
        .context("connect to indexer")?;
    let epoch = provider.get_epoch().await?.as_u64();
    let max_epoch = Epoch(epoch + EPOCH_WINDOW);
    match (command.as_str(), rest) {
        ("status", []) => {
            println!("epoch: {epoch}");
            for (resource, balance) in provider.get_account_balances(account).await? {
                println!("{resource}: {balance}");
            }
        },
        ("publish", [wasm]) => {
            let binary = fs::read(wasm).with_context(|| format!("read {wasm}"))?;
            println!("template size: {} bytes", binary.len());
            run(provider, account, max_epoch, Amount::zero(), submit, |b| {
                b.then(|t| t.publish_template(binary.clone()))
            })
            .await?;
        },
        ("launch", [template, fee, symbol, name, supply, locks @ ..]) if !locks.is_empty() => {
            let template = PublishedTemplateAddress::from_str(template)
                .map_err(|e| anyhow!("bad template address: {e:?}"))?
                .as_template_address();
            let fee = Amount::new(fee.parse().context("fee must be whole µT")?);
            let supply = whole_tokens(supply)?;
            let own_key = key_bytes(&public_key.to_string())?;
            let locks = locks
                .iter()
                .map(|spec| parse_lock(spec, own_key, epoch))
                .collect::<Result<Vec<_>>>()?;
            let (symbol, name) = (symbol.clone(), name.clone());
            run(provider, account, max_epoch, fee, submit, |b| {
                b.call_method(account, "withdraw", args![TARI_TOKEN, fee])
                    .put_last_instruction_output_on_workspace("fee")
                    .call_function(template, "launch", args![Workspace("fee"), symbol, name, image_url, supply, locks])
                    .put_last_instruction_output_on_workspace("rest")
                    .call_method(account, "deposit", args![Workspace("rest")])
            })
            .await?;
        },
        _ => bail!("unknown command or wrong arguments: {command} {rest:?}"),
    }
    Ok(())
}

/// `AMOUNT@UNLOCK_EPOCH[@BENEFICIARY_KEY]`, amount in whole tokens; the key defaults to `own_key`.
fn parse_lock(spec: &str, own_key: RistrettoPublicKeyBytes, epoch: u64) -> Result<(Amount, u64, RistrettoPublicKeyBytes)> {
    let parts: Vec<&str> = spec.split('@').collect();
    let (amount, unlock_epoch, key) = match parts.as_slice() {
        [amount, unlock_epoch] => (*amount, *unlock_epoch, own_key),
        [amount, unlock_epoch, key] => (*amount, *unlock_epoch, key_bytes(key)?),
        _ => bail!("lock {spec:?} must be AMOUNT@UNLOCK_EPOCH[@BENEFICIARY_KEY]"),
    };
    let unlock_epoch: u64 = unlock_epoch.parse().with_context(|| format!("bad unlock epoch in {spec:?}"))?;
    ensure!(unlock_epoch > epoch, "lock {spec:?}: unlock epoch must be after the current epoch {epoch}");
    Ok((whole_tokens(amount)?, unlock_epoch, key))
}

fn whole_tokens(text: &str) -> Result<Amount> {
    let n: u128 = text.parse().with_context(|| format!("{text:?} is not a whole number of tokens"))?;
    ensure!(n > 0, "token amounts must be positive");
    Ok(Amount::new(n.checked_mul(TOKEN_UNIT).ok_or_else(|| anyhow!("{text} tokens is too large"))?))
}

/// A beneficiary key someone can sign for: a canonical Ristretto point that isn't the identity.
fn key_bytes(hex: &str) -> Result<RistrettoPublicKeyBytes> {
    let key = RistrettoPublicKey::from_hex(hex).map_err(|_| anyhow!("{hex} is not a valid public key"))?;
    let bytes = key.as_bytes();
    ensure!(bytes.iter().any(|b| *b != 0), "{hex} is the identity point, which nobody can sign for");
    RistrettoPublicKeyBytes::try_from(bytes).map_err(|_| anyhow!("{hex} is not a 32-byte key"))
}

type Provider = IndexerProvider<OotleWallet>;
type Builder<'p> = ComponentInvokeBuilder<'p, Provider>;

/// Builds `instructions` with this account paying the fee, dry-runs it, prints the network fee,
/// and with `submit` sends exactly that transaction. `spend` is the TARI the instructions
/// themselves withdraw, kept out of the fee reservation. Fails unless the transaction commits.
async fn run<F>(mut provider: Provider, account: ComponentAddress, max_epoch: Epoch, spend: Amount, submit: bool, instructions: F) -> Result<()>
where
    F: FnOnce(Builder<'_>) -> Builder<'_>,
{
    let tari = provider.get_account_balance(account, TARI_TOKEN).await?;
    let available = tari
        .checked_sub(spend)
        .filter(Amount::is_positive)
        .ok_or_else(|| anyhow!("the account holds {tari} µT, not enough for {spend} µT plus fees"))?;
    let reserve = available.to_u64_checked().map_or(MAX_FEE, |a| a.min(MAX_FEE));
    // A fresh nonce keeps two identical commands from producing the same transaction id.
    let nonce: u64 = rand::random();
    let tx = instructions(IComponent::new(&provider, max_epoch).pay_fee(reserve).then(|t| t.with_nonce(nonce)))
        .prepare()
        .await?;

    let dry = provider.sign_and_send_dry_run(tx.clone()).await?;
    if let Some(reason) = dry.finalize.any_reject() {
        bail!("dry run rejected: {reason}");
    }
    // Dry runs are not rejected for an insufficient fee, so check the reservation covers it.
    let fee = dry.finalize.required_fees();
    ensure!(fee <= reserve, "the transaction needs {fee} µT in fees but only {reserve} µT is reserved");
    println!("dry run ok; network fee {fee} µT ({:.6} TARI) of {reserve} µT reserved", fee as f64 / 1e6);
    if !submit {
        println!("not sent (add --submit to send)");
        return Ok(());
    }

    let signed = TransactionRequest::default().with_transaction(tx).build(provider.wallet()).await?;
    let pending = provider.send_transaction(signed).await?;
    println!("sent {}", pending.tx_id());
    let outcome = pending.watch().await?;
    ensure!(outcome.is_commit(), "transaction {} did not commit: {outcome:?}", pending.tx_id());
    let receipt = pending.get_receipt().await?;
    println!("committed in epoch {}, fee paid {} µT", receipt.epoch, receipt.fee_receipt.total_fees_paid());
    for up in &receipt.diff_summary.upped {
        println!("upped: {}", up.substate_id);
    }
    for event in &receipt.events {
        println!("event: {} {{{}}}", event.topic(), event.payload());
    }
    Ok(())
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

/// Removes `name <value>` from `args` and returns the value.
fn take_option(args: &mut Vec<String>, name: &str) -> Result<Option<String>> {
    let Some(i) = args.iter().position(|a| a == name) else { return Ok(None) };
    ensure!(i + 1 < args.len(), "{name} needs a value");
    let value = args.remove(i + 1);
    args.remove(i);
    Ok(Some(value))
}

fn network_name(network: Network) -> &'static str {
    match network {
        Network::MainNet => "mainnet",
        _ => "esmeralda",
    }
}

fn key_path(network: Network) -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(format!(".config/secrets/ootle-lockbox-{}.key", network_name(network))))
}

/// Loads Lockbox's key file, refusing one other users can read or made for another network.
fn load_key(file: &PathBuf, network: Network) -> Result<OotleSecretKey> {
    let mode = fs::metadata(file)
        .with_context(|| format!("read {} (create it with lockbox-cli init)", file.display()))?
        .permissions()
        .mode();
    ensure!(mode & 0o077 == 0, "{} must be mode 600", file.display());
    let text = fs::read_to_string(file)?;
    let [file_network, account, view] = <[&str; 3]>::try_from(text.lines().map(str::trim).collect::<Vec<_>>())
        .map_err(|_| anyhow!("key file must have three lines"))?;
    ensure!(file_network == network_name(network), "{} holds a {file_network} key, not {}", file.display(), network_name(network));
    let parse = |hex: &str| RistrettoSecretKey::from_hex(hex).map_err(|_| anyhow!("malformed key file"));
    let (account, view) = (parse(account)?, parse(view)?);
    Ok(OotleSecretKey::new(network, account, view))
}
