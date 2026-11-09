//! Refuses to build the template's WASM unless the pinned addresses and fee are set and well formed, so a
//! template that could never launch can't be published by mistake. Host builds (`cargo test`)
//! skip the check; the tests supply their own pins.
fn main() {
    println!("cargo:rerun-if-env-changed=LOCKBOX_TEMPLATE");
    println!("cargo:rerun-if-env-changed=FEE_ACCOUNT");
    println!("cargo:rerun-if-env-changed=LAUNCH_FEE_MICROTARI");
    if !std::env::var("TARGET").unwrap_or_default().starts_with("wasm32") {
        return;
    }
    let is_hex64 = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let lockbox = std::env::var("LOCKBOX_TEMPLATE").unwrap_or_default();
    assert!(is_hex64(&lockbox), "set LOCKBOX_TEMPLATE to the Lockbox template address (64 hex characters)");
    let fee = std::env::var("FEE_ACCOUNT").unwrap_or_default();
    assert!(
        fee.strip_prefix("component_").is_some_and(is_hex64),
        "set FEE_ACCOUNT to the fee account's address (component_ followed by 64 hex characters)"
    );
    let fee = std::env::var("LAUNCH_FEE_MICROTARI").unwrap_or_default();
    assert!(
        fee.parse::<u64>().is_ok_and(|f| f > 0),
        "set LAUNCH_FEE_MICROTARI to the launch fee in atomic TARI (2500000000 = 2,500 TARI)"
    );
}
