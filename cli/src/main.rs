//! launch-cli: publishes the Launch Commitments template and launches a token from a local key.
//!
//! Usage: `launch-cli <esmeralda|mainnet> <command> <args> [--image <https url>] [--submit]`
//!   address                       print the Ootle address, account and public key
//!   status                        current epoch and account balances
//!   publish <wasm>                publish the template
//!   launch <template> <fee_microtari> <symbol> <name> <supply> <lock>...
//!                                 launch a token; `supply` and lock amounts are whole tokens
//!                                 (18 decimals), each lock is `AMOUNT@UNLOCK_EPOCH[@BENEFICIARY_KEY]`
//!                                 (the beneficiary defaults to this key)
//!
//! Options go after a command's fixed arguments, so a symbol or name may be any text.
//! Every transaction is dry-run first; `--submit` then sends that same transaction. The launch and
//! the remainder deposit run in the main instructions, so any failure undoes the whole launch.
//! The key file and transaction runner come from lockbox-cli (create the key with
//! `lockbox-cli <network> init`); the key is never printed.

use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail, ensure};
use lockbox_cli::{Session, key_path, load_key, parse_network};
use ootle_rs::{
    ToAccountAddress,
    builtin_templates::component::TransactionBuildable,
    template_types::{Amount, constants::TARI_TOKEN, crypto::RistrettoPublicKeyBytes},
};
use tari_crypto::{
    ristretto::RistrettoPublicKey,
    tari_utilities::{ByteArray, hex::Hex},
};
use tari_ootle_common_types::engine_types::published_template::PublishedTemplateAddress;
use tari_ootle_transaction::args;

/// Launched tokens have 18 decimals.
const TOKEN_UNIT: u128 = 10u128.pow(18);
const USAGE: &str = "usage: launch-cli <esmeralda|mainnet> <address|status|publish|launch> <args> [--image <url>] [--submit]";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [network, command, rest @ ..] = args.as_slice() else { bail!(USAGE) };
    let network = parse_network(network)?;
    let fixed = match command.as_str() {
        "address" | "status" => 0,
        "publish" => 1,
        "launch" => 5,
        _ => bail!("unknown command {command}\n{USAGE}"),
    };
    ensure!(rest.len() >= fixed, "{command} needs {fixed} arguments\n{USAGE}");
    let (fixed, tail) = rest.split_at(fixed);
    let Options { submit, image_url, rest } = Options::parse(tail)?;
    ensure!(image_url.is_none() || command == "launch", "--image only applies to launch");

    let secret = load_key(&key_path(network)?, network)?;
    let address = secret.to_address();
    let account = address.to_account_address();
    let public_key = *address.account_public_key();

    if command == "address" {
        ensure!(rest.is_empty(), "address takes no arguments");
        println!("address:    {address}");
        println!("account:    {account}");
        println!("public key: {public_key}");
        return Ok(());
    }

    let session = Session::connect(secret, network).await?;
    match (command.as_str(), fixed, rest.as_slice()) {
        ("status", [], []) => session.print_status(account).await?,
        ("publish", [wasm], []) => session.publish(account, wasm, submit).await?,
        ("launch", [template, fee, symbol, name, supply], locks) if !locks.is_empty() => {
            let template = PublishedTemplateAddress::from_str(template)
                .map_err(|e| anyhow!("bad template address: {e:?}"))?
                .as_template_address();
            let fee = Amount::new(fee.parse().context("fee must be whole µT")?);
            let supply = whole_tokens(supply)?;
            let own_key = key_bytes(&public_key.to_string())?;
            let locks = locks
                .iter()
                .map(|spec| parse_lock(spec, own_key, session.epoch))
                .collect::<Result<Vec<_>>>()?;
            let (symbol, name, image_url) = (symbol.clone(), name.clone(), image_url.unwrap_or_default());
            session
                .run(account, fee, submit, |b| {
                    b.call_method(account, "withdraw", args![TARI_TOKEN, fee])
                        .put_last_instruction_output_on_workspace("fee")
                        .call_function(template, "launch", args![Workspace("fee"), symbol, name, image_url, supply, locks])
                        .put_last_instruction_output_on_workspace("rest")
                        .call_method(account, "deposit", args![Workspace("rest")])
                })
                .await?;
        },
        _ => bail!("wrong arguments for {command}: {rest:?}\n{USAGE}"),
    }
    Ok(())
}

/// What follows a command's fixed arguments: `--submit`, `--image <url>`, and the rest (locks).
struct Options {
    submit: bool,
    image_url: Option<String>,
    rest: Vec<String>,
}

impl Options {
    fn parse(tail: &[String]) -> Result<Self> {
        let mut options = Options { submit: false, image_url: None, rest: Vec::new() };
        let mut tail = tail.iter();
        while let Some(arg) = tail.next() {
            match arg.as_str() {
                "--submit" => options.submit = true,
                "--image" => {
                    let url = tail.next().ok_or_else(|| anyhow!("--image needs a value"))?;
                    ensure!(options.image_url.replace(url.clone()).is_none(), "--image given twice");
                },
                other if other.starts_with("--") => bail!("unknown option {other}"),
                _ => options.rest.push(arg.clone()),
            }
        }
        Ok(options)
    }
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
