//! Test-only probe: tries every privileged action on a resource, and makes an owned "twin" of a
//! Launch Commitments token (same rules, default owner) as the positive control: whatever works on
//! the twin and fails on a launched token proves the launched token's `OwnerRule::None` matters.
use tari_template_lib::prelude::*;

#[template]
mod probe {
    use super::*;

    pub struct Probe {
        vault: Vault,
    }

    impl Probe {
        /// Holds a new token with the launched token's rules but the builder's default owner
        /// (the transaction signer).
        pub fn twin(supply: Amount) -> Component<Self> {
            let tokens = ResourceBuilder::public_fungible()
                .with_access_rules(
                    ResourceAccessRules::deny_all()
                        .withdrawable(rule!(allow_all), UpdateRule::Locked)
                        .depositable(rule!(allow_all), UpdateRule::Locked),
                )
                .with_divisibility(18)
                .initial_supply(supply);
            Self::hold(tokens)
        }

        pub fn resource_address(&self) -> ResourceAddress {
            self.vault.resource_address()
        }

        /// Hands out some held tokens, to test ordinary transfers and burning.
        pub fn take(&mut self, amount: Amount) -> Bucket {
            self.vault.withdraw(amount)
        }

        /// Holds tokens in a vault so freeze and recall have a target.
        pub fn hold(tokens: Bucket) -> Component<Self> {
            Component::new(Self { vault: Vault::from_bucket(tokens) })
                .with_access_rules(ComponentAccessRules::allow_all())
                .create()
        }

        pub fn mint(resource: ResourceAddress, amount: Amount) -> Bucket {
            ResourceManager::get(resource).mint_fungible(amount)
        }

        pub fn burn(tokens: Bucket) {
            tokens.burn();
        }

        pub fn set_metadata(resource: ResourceAddress) {
            let mut metadata = Metadata::new();
            metadata.insert("name", &"Changed".to_string());
            ResourceManager::get(resource).set_metadata(metadata);
        }

        pub fn open_minting(resource: ResourceAddress) {
            ResourceManager::get(resource).update_access_rule(ResourceAuthAction::Mint, AccessRule::AllowAll);
        }

        pub fn install_hook(&self, resource: ResourceAddress) {
            let hook = AuthHook::new(CallerContext::current_component_address(), "hook".try_into().unwrap());
            ResourceManager::get(resource).set_auth_hook(Some(hook));
        }

        pub fn freeze(&self) {
            ResourceManager::get(self.vault.resource_address()).freeze_vault(self.vault.vault_id());
        }

        pub fn recall(&self) -> Bucket {
            ResourceManager::get(self.vault.resource_address()).recall_all(self.vault.vault_id())
        }
    }
}
