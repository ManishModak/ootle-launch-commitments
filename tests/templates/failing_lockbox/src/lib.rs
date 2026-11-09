//! Test-only stand-in for Lockbox with the same `lock` signature: it locks normally but panics
//! for `unlock_epoch == 666`, so a test can make the second nested lock of a launch fail after the
//! fee was paid and the first lock was created.
use tari_template_lib::prelude::*;

#[template]
mod failing_lockbox {
    use super::*;

    pub struct FailingLock {
        vault: Vault,
    }

    impl FailingLock {
        pub fn lock(tokens: Bucket, unlock_epoch: u64, _beneficiary: RistrettoPublicKeyBytes, _memo: String) -> Component<Self> {
            assert!(unlock_epoch != 666, "Injected failure inside the nested lock");
            Component::new(Self { vault: Vault::from_bucket(tokens) })
                .with_access_rules(ComponentAccessRules::allow_all())
                .create()
        }
    }
}
