# Launch Commitments

A token factory on [Tari Ootle](https://ootle.tari.com) for launches that can be checked, not just trusted.

One transaction pays a flat launch fee and creates a fixed-supply public token that **nobody can ever change, its
creator included**. The token has no owner, every rule and updater is locked, only withdraw and deposit are open,
and it has no transfer hook. The same transaction locks the creator's chosen allocations (team, advisors, …) in
[Ootle Lockbox](https://github.com/ManishModak/ootle-lockbox) until an epoch. It also writes a read-only
`LaunchRecord` that the token's `launched_by` metadata points back to. Any failure undoes all of it, fee included.

The Lockbox web app shows each launch as separate facts: fixed supply, rules frozen, issued by Launch Commitments,
allocations locked. "Allocations locked" is creator-declared: it proves which tokens are locked until when, not who
the team is or who holds the rest.

**Why "no owner" matters.** On Ootle a resource's owner bypasses its access rules for every action except recall,
so `mint: DenyAll` on an owned token doesn't stop its owner minting. Launched tokens use `OwnerRule::None`. The
engine tests prove this with a positive control: an owned twin with identical rules, whose creator *can* mint,
freeze and edit metadata, next to the launched token, where every privileged action is refused.

## Build and test

The template pins three values at build time, so a published template's fee and destinations can never change:

- `LOCKBOX_TEMPLATE`: the Lockbox template address (64 hex);
- `FEE_ACCOUNT`: the account that receives launch fees (`component_<64 hex>`);
- `LAUNCH_FEE_MICROTARI`: the flat fee in atomic TARI (2500000000 = 2,500 TARI on mainnet).

```sh
LOCKBOX_TEMPLATE=… FEE_ACCOUNT=… LAUNCH_FEE_MICROTARI=… cargo build --release --target wasm32-unknown-unknown
cargo test   # engine tests; expects the ootle-lockbox repo checked out next to this one
```

`build.rs` refuses a WASM build without all three. Dry-run a launch with the exact release WASM before publishing:
a well-formed but wrong address still builds.

## launch-cli

`cli/` publishes the template and launches a token from a local key (shared with `lockbox-cli`):

```sh
cd cli && cargo build
launch-cli mainnet launch <template> <fee_microtari> <symbol> <name> <supply> <amount@unlock_epoch[@beneficiary_key]>... [--submit]
```

Every transaction is dry-run first; `--submit` sends that same transaction.

## License

MIT
