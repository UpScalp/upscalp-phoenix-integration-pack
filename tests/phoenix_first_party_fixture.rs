// Copy with the unchanged pinned SDK test_harness; native source is the owner
// engine/phoenix_native.rs copied to examples/upscalp_phoenix_native.rs.
mod test_harness;
#[allow(dead_code)]
#[path = "../examples/upscalp_phoenix_native.rs"]
mod native;

use phoenix_rise::{accounts::owned::{Trader, ConditionalOrderCollection, StopLossDirection, StopLossOrderKind, StopLossTradeSide}, core::{FetchedAccount, PhoenixTxBuilder, PhoenixMetadata}, ix::{PHOENIX_PROGRAM_ID, prelude::{DelegateTraderParams, create_delegate_trader_ix, get_conditional_orders_address}}};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::str::FromStr;
use base64::{Engine, engine::general_purpose::STANDARD};
use test_harness::{SdkLocalnetContext, default_sdk_localnet_fixture, find_sdk_localnet_program_paths, sdk_localnet_seed_bytes, phoenix_metadata_from_fixture};

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

#[tokio::test]
async fn delegated_entry_is_atomic_with_full_stop_and_exact_split_targets() {
    let paths = find_sdk_localnet_program_paths().expect("pinned programs required; no skip");
    let mut ctx = SdkLocalnetContext::new(default_sdk_localnet_fixture().unwrap(), paths);
    ctx.execute_setup();
    let owner = ctx.actor_pubkey("taker0");
    let delegate = ctx.actor_pubkey("taker1");
    let address = ctx.actor_trader("taker0");
    let delegate_seed = ctx.actor("taker1").seed;
    let ix: Instruction = create_delegate_trader_ix(DelegateTraderParams::builder()
        .trader_wallet(owner).trader_account(address).new_position_authority(delegate).build().unwrap()).unwrap().into();
    ctx.send_instructions(vec![ix], &ctx.actor("taker0").seed, "synthetic-entry-delegation");
    let data = ctx.account_data(&address);
    let account = FetchedAccount::new(data.clone(), *PHOENIX_PROGRAM_ID, false);
    let metadata = phoenix_metadata_from_fixture(&ctx.fixture);
    let slot = 1_000;
    ctx.svm.warp_to_slot(slot);
    let plan = native::EntryPlan {
        symbol: "BTC".into(), direction: "LONG".into(), base_lots: 100, max_quote_lots: 2_000_000_000,
        reference_price: 100_000.0, limit_price: 101_000.0, stop_price: 98_000.0, stop_execution_price: 97_900.0,
        targets: vec![native::Target { price: 102_000.0, base_lots: 40 }, native::Target { price: 103_000.0, base_lots: 60 }],
        client_order_id: 123, last_valid_slot: slot + 100,
    };
    for invalid in [
        native::EntryPlan { last_valid_slot: slot, ..plan.clone() },
        native::EntryPlan { stop_price: f64::NAN, ..plan.clone() },
        native::EntryPlan { stop_price: 101_000.0, ..plan.clone() },
        native::EntryPlan { base_lots: 99, ..plan.clone() },
        native::EntryPlan { targets: vec![native::Target { price: 99_000.0, base_lots: 100 }], ..plan.clone() },
        native::EntryPlan { targets: vec![native::Target { price: 100_500.0, base_lots: 100 }], ..plan.clone() },
    ] {
        assert!(native::build_protected_entry(&metadata, owner, delegate, &account, None, slot, &invalid).await.is_err());
    }
    let ixs = native::build_protected_entry(&metadata, owner, delegate, &account, None, slot, &plan).await.unwrap();
    let signer = Keypair::new_from_array(sdk_localnet_seed_bytes(&ctx.fixture.keypair_base_seed, &delegate_seed));
    let tx = solana_transaction::Transaction::new_signed_with_payer(&ixs, Some(&delegate), &[&signer], ctx.svm.latest_blockhash());
    let bytes = bincode::serialize(&tx).unwrap();
    assert!(bytes.len() <= 1232, "transaction exceeds Solana packet: {}", bytes.len());
    let prepared = native::prepared_transaction(&tx, delegate).unwrap();
    assert_eq!(STANDARD.decode(prepared["transaction_base64"].as_str().unwrap()).unwrap(), bytes);
    assert_eq!(prepared["signature"], tx.signatures[0].to_string());
    assert_eq!(prepared["submitted"], false);
    assert!(native::prepared_transaction(&tx, owner).is_err());
    let mut tampered = tx.clone();
    tampered.message.instructions[0].data[0] ^= 0xff;
    assert!(native::prepared_transaction(&tampered, delegate).is_err());
    let conditional = get_conditional_orders_address(&address).unwrap();
    let mut broken = ixs.clone();
    broken.last_mut().unwrap().data[0] ^= 0xff;
    assert!(ctx.try_send_instructions_with_metadata(broken, &delegate_seed).is_err());
    assert_eq!(data, ctx.account_data(&address));
    assert!(ctx.svm.get_account(&conditional).is_none());
    ctx.send_instructions(ixs, &delegate_seed, "delegated-entry-and-all-protection");
    let state = Trader::try_from_account_bytes(&ctx.account_data(&address)).unwrap();
    assert_eq!(state.position_entries().find(|row| row.asset_id == 0).unwrap().position.base_lot_position.as_inner(), 100);
    let collection = ConditionalOrderCollection::try_from_account_bytes(&ctx.account_data(&conditional)).unwrap();
    let orders: Vec<_> = collection.active_orders().map(|(_, order)| order).collect();
    assert_eq!(orders.len(), 3);
    assert_eq!(orders.iter().filter(|order| order.use_percent && order.percent == 100).count(), 1);
    let mut target_lots: Vec<_> = orders.iter().filter(|order| !order.use_percent).map(|order| order.max_size.as_inner()).collect();
    target_lots.sort();
    assert_eq!(target_lots, vec![40, 60]);
    let builder = PhoenixTxBuilder::new(&metadata);
    let calc = builder.order_ticket_metadata("BTC").unwrap().market_calc;
    let stop = orders.iter().find(|order| order.use_percent).unwrap();
    assert!(stop.less_trigger_order.is_active);
    assert_eq!(stop.less_trigger_order.trigger_price, calc.price_to_ticks(plan.stop_price).unwrap());
    assert_eq!(stop.less_trigger_order.execution_price, calc.price_to_ticks(plan.stop_execution_price).unwrap());
    assert_eq!(stop.less_trigger_order.order_kind, StopLossOrderKind::IOC);
    assert_eq!(stop.less_trigger_order.trade_side, StopLossTradeSide::Ask);
    assert_eq!(stop.less_trigger_order.execution_direction, StopLossDirection::LessThan);
    for target in &plan.targets {
        let order = orders.iter().find(|order| !order.use_percent && order.max_size.as_inner() == target.base_lots).unwrap();
        assert!(order.greater_trigger_order.is_active);
        assert_eq!(order.greater_trigger_order.trigger_price, calc.price_to_ticks(target.price).unwrap());
        assert_eq!(order.greater_trigger_order.execution_price, calc.price_to_ticks(target.price).unwrap());
        assert_eq!(order.greater_trigger_order.order_kind, StopLossOrderKind::Limit);
        assert_eq!(order.greater_trigger_order.trade_side, StopLossTradeSide::Ask);
        assert_eq!(order.greater_trigger_order.execution_direction, StopLossDirection::GreaterThan);
    }
    let current = FetchedAccount::new(ctx.account_data(&address), *PHOENIX_PROGRAM_ID, false);
    assert_eq!(native::build_protected_entry(&metadata, owner, delegate, &current, None, slot, &plan).await.unwrap_err(), "existing_position_refused");
    let book_address = solana_pubkey::Pubkey::from_str(&metadata.get_market("BTC").unwrap().market_pubkey).unwrap();
    let book_account = FetchedAccount::new(ctx.account_data(&book_address), *PHOENIX_PROGRAM_ID, false);
    let projection = native::project_state(&metadata, owner, delegate, "BTC", &current,
        Some(&FetchedAccount::new(ctx.account_data(&conditional), *PHOENIX_PROGRAM_ID, false)), &book_account).unwrap();
    assert_eq!(projection["signed_base_lots"], 100);
    assert_eq!(projection["conditionals"].as_array().unwrap().len(), 3);
    assert_eq!(projection["nonzero_position_count"], 1);
    assert!(native::project_state(&metadata, owner, delegate, "BTC", &current, None,
        &FetchedAccount::new(book_account.data.clone(), owner, false)).is_err());
    let mut wrong_market = metadata.exchange().clone();
    wrong_market.markets.get_mut("BTC").unwrap().tick_size += 1;
    assert!(native::project_state(&PhoenixMetadata::new(wrong_market), owner, delegate, "BTC", &current, None, &book_account).is_err());
    let close = native::ClosePlan {
        symbol: "BTC".into(), signed_base_lots: 100,
        position_sequence: state.position_entries().find(|row| row.asset_id == 0).unwrap().position.position_sequence_number,
        conditionals: collection.active_orders().map(|(index, order)| (index, order.sequence_number)).collect(),
        limit_price: 99_000.0, max_quote_lots: 2_000_000_000, client_order_id: 124, last_valid_slot: slot + 100,
    };
    let conditional_data = ctx.account_data(&conditional);
    let conditional_account = FetchedAccount::new(conditional_data.clone(), *PHOENIX_PROGRAM_ID, false);
    for invalid in [
        native::ClosePlan { signed_base_lots: -100, ..close.clone() },
        native::ClosePlan { position_sequence: close.position_sequence.wrapping_add(1), ..close.clone() },
        native::ClosePlan { conditionals: vec![], ..close.clone() },
        native::ClosePlan { conditionals: vec![(close.conditionals[0].0, close.conditionals[0].1 + 1)], ..close.clone() },
        native::ClosePlan { last_valid_slot: slot, ..close.clone() },
    ] {
        assert!(native::build_protected_close(&metadata, owner, delegate, &current, &conditional_account, slot, &invalid).await.is_err());
    }
    let close_ixs = native::build_protected_close(&metadata, owner, delegate, &current, &conditional_account, slot, &close).await.unwrap();
    let mut broken_close = close_ixs.clone();
    broken_close.last_mut().unwrap().data[0] ^= 0xff;
    assert!(ctx.try_send_instructions_with_metadata(broken_close, &delegate_seed).is_err());
    assert_eq!(current.data, ctx.account_data(&address));
    assert_eq!(conditional_data, ctx.account_data(&conditional));
    ctx.send_instructions(close_ixs, &delegate_seed, "delegated-reduce-only-close-and-exact-protection-cleanup");
    let closed = Trader::try_from_account_bytes(&ctx.account_data(&address)).unwrap();
    assert_eq!(closed.position_entries().find(|row| row.asset_id == 0).map(|row| row.position.base_lot_position.as_inner()).unwrap_or(0), 0);
    let after_close = ConditionalOrderCollection::try_from_account_bytes(&ctx.account_data(&conditional)).unwrap();
    assert_eq!(after_close.active_orders().count(), 0);
    let closed_projection = native::project_state(&metadata, owner, delegate, "BTC",
        &FetchedAccount::new(ctx.account_data(&address), *PHOENIX_PROGRAM_ID, false),
        Some(&FetchedAccount::new(ctx.account_data(&conditional), *PHOENIX_PROGRAM_ID, false)), &book_account).unwrap();
    assert_eq!(closed_projection["signed_base_lots"], 0);
    assert_eq!(closed_projection["conditionals"].as_array().unwrap().len(), 0);
    println!("UPSCALP_NATIVE_CLOSE position_lots=0 retained_conditional_orders=0 close_cleanup_atomic=true");
    println!("UPSCALP_NATIVE_ENTRY delegated=true filled_lots=100 stop_remaining_percent=100 target_lots=40,60 transaction_bytes={} protection_failure_rolls_back_entry=true duplicate_entry_refused=true", bytes.len());
}
