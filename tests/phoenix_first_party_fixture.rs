// Copy with the unchanged pinned SDK test_harness; native source is the owner
// engine/phoenix_native.rs copied to examples/upscalp_phoenix_native.rs.
mod test_harness;
#[allow(dead_code)]
#[path = "../examples/upscalp_phoenix_native.rs"]
mod native;

use phoenix_rise::{core::FetchedAccount, ix::{PHOENIX_PROGRAM_ID, prelude::{DelegateTraderParams, create_delegate_trader_ix}}};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;
use test_harness::{SdkLocalnetContext, default_sdk_localnet_fixture, find_sdk_localnet_program_paths, sdk_localnet_seed_bytes};

#[test]
fn native_connection_binds_dedicated_delegate_and_program_account() {
    let paths = find_sdk_localnet_program_paths().expect("pinned programs required; no skip");
    let mut ctx = SdkLocalnetContext::new(default_sdk_localnet_fixture().unwrap(), paths);
    ctx.execute_setup();
    let owner = ctx.actor_pubkey("taker0");
    let delegate = ctx.actor_pubkey("taker1");
    let address = ctx.actor_trader("taker0");
    let owner_seed = ctx.actor("taker0").seed;
    let signer = Keypair::new_from_array(sdk_localnet_seed_bytes(&ctx.fixture.keypair_base_seed, &ctx.actor("taker1").seed));
    let (bound_owner, parsed_signer) = native::parse_identity(&owner.to_string(), &signer.to_base58_string()).unwrap();
    assert_eq!(bound_owner, owner);
    assert_eq!(parsed_signer.pubkey(), delegate);
    assert!(native::parse_identity(&delegate.to_string(), &signer.to_base58_string()).is_err());
    assert!(native::parse_identity(&owner.to_string(), "not-a-keypair").is_err());
    let before = FetchedAccount::new(ctx.account_data(&address), *PHOENIX_PROGRAM_ID, false);
    assert_eq!(native::validate_account(owner, delegate, address, &before).unwrap_err(), "delegate_not_authorized");
    let ix: Instruction = create_delegate_trader_ix(DelegateTraderParams::builder()
        .trader_wallet(owner).trader_account(address).new_position_authority(delegate).build().unwrap()).unwrap().into();
    ctx.send_instructions(vec![ix], &owner_seed, "synthetic-connection-delegation");
    let data = ctx.account_data(&address);
    let account = FetchedAccount::new(data.clone(), *PHOENIX_PROGRAM_ID, false);
    let proof = native::validate_account(owner, delegate, address, &account).unwrap();
    assert_eq!(proof["delegation_verified"], true);
    assert_eq!(proof["execution_ready"], false);
    assert_eq!(proof["trader_account"], address.to_string());
    assert!(native::validate_account(delegate, owner, address, &account).is_err());
    assert!(native::validate_account(owner, delegate, ctx.actor_trader("taker1"), &account).is_err());
    for invalid in [
        FetchedAccount::new(data.clone(), owner, false),
        FetchedAccount::new(data.clone(), *PHOENIX_PROGRAM_ID, true),
        FetchedAccount::unchecked(data.clone()),
        FetchedAccount::new(vec![0; 64], *PHOENIX_PROGRAM_ID, false),
    ] {
        assert!(native::validate_account(owner, delegate, address, &invalid).is_err());
    }
    assert_eq!(data, ctx.account_data(&address));
    println!("UPSCALP_NATIVE_CONNECTION delegated=true account_and_program_bound=true rejects_owner_key_and_foreign_scope=true probe_writes=0 execution_ready=false");
}
