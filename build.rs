//! Refuses to build the template's WASM unless both pinned addresses are set and well formed, so a
//! template that could never launch can't be published by mistake. Host builds (`cargo test`)
//! skip the check; the tests supply their own pins.
fn main() {
    println!("cargo:rerun-if-env-changed=LOCKBOX_TEMPLATE");
    println!("cargo:rerun-if-env-changed=FEE_ACCOUNT");
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
}
