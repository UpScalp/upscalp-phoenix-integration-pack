// Run inside the pinned SDK test target with its unchanged test_harness.
// Synthetic VM accounts only. This is not a proof over all program entrypoints.
mod test_harness;

use phoenix_rise::accounts::owned::{GlobalConfiguration, Trader};
use phoenix_rise::core::{PhoenixTxBuilder, TraderKey};
use phoenix_rise::ix::prelude::{
    DelegateTraderParams, WithdrawFundsParams, create_delegate_trader_ix,
    create_withdraw_funds_ix,
};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use std::str::FromStr;
use test_harness::{
    SdkLocalnetContext, default_sdk_localnet_fixture, find_sdk_localnet_program_paths,
    phoenix_metadata_from_fixture,
};

#[test]
fn delegated_position_authority_custody_boundary() {
    let paths = find_sdk_localnet_program_paths().expect("pinned programs required; no skip");
    let mut ctx = SdkLocalnetContext::new(default_sdk_localnet_fixture().unwrap(), paths);
    ctx.execute_setup();
    let owner = ctx.actor_pubkey("taker0");
    let delegate = ctx.actor_pubkey("taker1");
    let owner_seed = ctx.actor("taker0").seed;
    let delegate_seed = ctx.actor("taker1").seed;
    let parent = ctx.actor_trader("taker0");
    let foreign = ctx.actor_trader("taker1");
    let owner_token = Pubkey::from_str(&ctx.actor("taker0").phoenix_token_account).unwrap();
    let foreign_token = Pubkey::from_str(&ctx.actor("taker1").phoenix_token_account).unwrap();
    let metadata = phoenix_metadata_from_fixture(&ctx.fixture);
    let builder = PhoenixTxBuilder::new(&metadata);
    let child_key = TraderKey::new_with_idx(owner, 0, 1);
    let child = child_key.pda();
    let delegate_ix: Instruction = create_delegate_trader_ix(
        DelegateTraderParams::builder().trader_wallet(owner).trader_account(parent)
            .new_position_authority(delegate).build().unwrap(),
    ).unwrap().into();
    ctx.send_instructions(vec![delegate_ix], &owner_seed, "owner-delegates-position");
    let trader = Trader::try_from_account_bytes(&ctx.account_data(&parent)).unwrap();
    assert_eq!(trader.authority, owner);
    assert_eq!(trader.position_authority, delegate);
    let mut child_ixs = builder.build_register_trader(owner, 0, 1).unwrap();
    child_ixs.extend(builder.build_sync_parent_to_child(owner, parent, child).unwrap());
    ctx.send_instructions(child_ixs, &owner_seed, "owner-creates-same-wallet-child");
    assert_eq!(Trader::try_from_account_bytes(&ctx.account_data(&child)).unwrap().authority, owner);

    // The SDK documents same-wallet collateral movement for position authority.
    let internal = builder.build_transfer_collateral(delegate, parent, child, 1.0).unwrap();
    let internal_result = ctx.try_send_instructions_with_metadata(internal, &delegate_seed);
    println!("UPSCALP_CUSTODY same_owner_subaccount_transfer={}", internal_result.is_ok());
    assert!(internal_result.is_ok(), "same-owner control failed: {internal_result:?}");

    let addresses = ctx.fixture.addresses.clone();
    let config = GlobalConfiguration::try_from_account_bytes(
        &ctx.account_data(&Pubkey::from_str(&addresses.global_config).unwrap()),
    ).unwrap();
    let mature_slot = trader.last_deposit_slot + config.deposit_cooldown_period_in_slots + 1;
    ctx.svm.warp_to_slot(mature_slot);
    println!("UPSCALP_CUSTODY synthetic_slot={mature_slot} deposit_cooldown_slots={}", config.deposit_cooldown_period_in_slots);
    let withdraw = |signer, destination| -> Instruction {
        create_withdraw_funds_ix(WithdrawFundsParams::builder()
            .trader(signer).trader_account(parent)
            .perp_asset_map(Pubkey::from_str(&addresses.perp_asset_map).unwrap())
            .global_vault(Pubkey::from_str(&addresses.global_vault).unwrap())
            .trader_token_account(destination)
            .global_trader_index(addresses.global_trader_index.iter().map(|v| Pubkey::from_str(v).unwrap()).collect())
            .active_trader_buffer(addresses.active_trader_buffer.iter().map(|v| Pubkey::from_str(v).unwrap()).collect())
            .withdraw_queue(Pubkey::from_str(&addresses.withdraw_queue).unwrap())
            .amount(1_000_000).build().unwrap()).unwrap().into()
    };
    for (label, ix) in [
        ("delegate_withdraw_to_owner", withdraw(delegate, owner_token)),
        ("delegate_withdraw_to_foreign", withdraw(delegate, foreign_token)),
        ("delegate_transfer_to_foreign", builder.build_transfer_collateral(delegate, parent, foreign, 1.0).unwrap().remove(0)),
    ] {
        let guarded = [parent, child, foreign, owner_token, foreign_token];
        let before: Vec<_> = guarded.iter().map(|p| ctx.account_data(p)).collect();
        let result = ctx.try_send_instructions_with_metadata(vec![ix], &delegate_seed);
        let error = result.expect_err("custody boundary violated: delegated outflow succeeded");
        assert_eq!(format!("{:?}", error.err), "InstructionError(0, IncorrectAuthority)");
        assert_eq!(before, guarded.iter().map(|p| ctx.account_data(p)).collect::<Vec<_>>());
        println!("UPSCALP_CUSTODY {label}=refused error={:?} guarded_accounts_unchanged=true", error.err);
        for log in error.meta.logs { println!("UPSCALP_CUSTODY_LOG {log}"); }
    }
    // Same withdrawal accounts/amount with the wallet signer must be supported.
    let result = ctx.try_send_instructions_with_metadata(vec![withdraw(owner, owner_token)], &owner_seed);
    assert!(result.is_ok(), "owner withdrawal control failed: {result:?}");
    println!("UPSCALP_CUSTODY owner_withdraw=succeeded delegate_distinct=true");
}
