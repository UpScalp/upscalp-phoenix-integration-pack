// Copy with the unchanged pinned SDK test_harness; native source is the owner
// engine/phoenix_native.rs copied to examples/upscalp_phoenix_native.rs.
mod test_harness;
#[allow(dead_code)]
#[path = "../examples/upscalp_phoenix_native.rs"]
mod native;

use phoenix_rise::{accounts::owned::{Trader, ConditionalOrderCollection, StopLossDirection, StopLossOrderKind, StopLossTradeSide}, core::{FetchedAccount, PhoenixTxBuilder, PhoenixMetadata}, ix::{PHOENIX_PROGRAM_ID, prelude::{DelegateTraderParams, create_delegate_trader_ix, get_conditional_orders_address, CancelConditionalOrderParams, create_cancel_conditional_order_ix}}};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::str::FromStr;
use base64::{Engine, engine::general_purpose::STANDARD};
use test_harness::{SdkLocalnetContext, default_sdk_localnet_fixture, find_sdk_localnet_program_paths, sdk_localnet_seed_bytes, phoenix_metadata_from_fixture};

#[test]
fn native_runtime_request_refuses_ambiguous_inputs_before_io() {
    let connection = serde_json::json!({"operation":"validate_connection", "wallet_address":"synthetic",
        "private_key":"synthetic-not-consumed", "rpc_url":"https://rpc.example.invalid", "genesis_hash":"synthetic"});
    let request: native::Request = serde_json::from_value(connection.clone()).unwrap();
    assert_eq!(request.operation_symbol().unwrap(), None);
    let mut state = connection.clone();
    state["operation"] = serde_json::json!("read_state");
    state["api_url"] = serde_json::json!("https://metadata.example.invalid");
    state["symbol"] = serde_json::json!("BTC");
    let request: native::Request = serde_json::from_value(state.clone()).unwrap();
    assert_eq!(request.operation_symbol().unwrap(), Some("BTC"));
    let mut entry = state.clone();
    entry.as_object_mut().unwrap().remove("symbol");
    entry["operation"] = serde_json::json!("prepare_entry");
    entry["expected_state_sha256"] = serde_json::json!("0".repeat(64));
    entry["entry"] = serde_json::json!({"symbol":"BTC", "direction":"SHORT", "base_lots":100,
        "max_quote_lots":2_000_000_000_u64, "reference_price":100_000.0, "limit_price":99_000.0,
        "stop_price":102_000.0, "stop_execution_price":102_100.0, "stop_execution_band_pct":1.0,
        "targets":[{"price":98_000.0,"base_lots":40},{"price":97_000.0,"base_lots":60}],
        "client_order_id":123,"last_valid_slot":1100});
    let request: native::Request = serde_json::from_value(entry.clone()).unwrap();
    assert_eq!(request.operation_symbol().unwrap(), Some("BTC"));
    let mut close = state.clone();
    close.as_object_mut().unwrap().remove("symbol");
    close["operation"] = serde_json::json!("prepare_close");
    close["expected_state_sha256"] = serde_json::json!("0".repeat(64));
    close["close"] = serde_json::json!({"symbol":"BTC", "signed_base_lots":-100,
        "position_sequence":1,"conditionals":[[0,1]],"limit_price":101_000.0,
        "max_quote_lots":2_000_000_000_u64,"client_order_id":124,"last_valid_slot":1100});
    let request: native::Request = serde_json::from_value(close.clone()).unwrap();
    assert_eq!(request.operation_symbol().unwrap(), Some("BTC"));
    for (mut input, key, value) in [
        (connection.clone(), "entry", entry["entry"].clone()),
        (state.clone(), "entry", entry["entry"].clone()),
        (state.clone(), "symbol", serde_json::json!(" BTC ")),
        (state.clone(), "api_url", serde_json::json!("http://metadata.example.invalid")),
        (state.clone(), "api_url", serde_json::json!("https://user@metadata.example.invalid")),
        (entry.clone(), "symbol", serde_json::json!("OTHER")),
        (entry.clone(), "expected_state_sha256", serde_json::Value::Null),
        (entry.clone(), "expected_state_sha256", serde_json::json!("G".repeat(64))),
        (entry.clone(), "close", close["close"].clone()),
        (close.clone(), "entry", entry["entry"].clone()),
        (state.clone(), "operation", serde_json::json!("submit")),
    ] {
        input[key] = value;
        let request: native::Request = serde_json::from_value(input).unwrap();
        assert!(request.operation_symbol().is_err(), "invalid {key} accepted");
    }
    entry["entry"].as_object_mut().unwrap().remove("stop_execution_price");
    assert!(serde_json::from_value::<native::Request>(entry).is_err());
    println!("UPSCALP_NATIVE_REQUEST four_operations_bound=true ambiguous_inputs_refused=true explicit_stop_required=true io_calls=0");
}

#[test]
fn prepared_status_preserves_original_plan_and_finality_without_io() {
    let signer = Keypair::new_from_array([71; 32]);
    let current = Keypair::new_from_array([73; 32]);
    let owner = Keypair::new_from_array([72; 32]).pubkey();
    let mut input = serde_json::json!({"operation":"prepare_entry", "wallet_address":owner.to_string(),
        "private_key":"not-used", "rpc_url":"https://rpc.example.invalid", "genesis_hash":"test",
        "api_url":"https://metadata.example.invalid", "expected_state_sha256":"a".repeat(64),
        "entry":{"symbol":"BTC","direction":"LONG","base_lots":100,"max_quote_lots":500,
            "reference_price":100.0,"limit_price":101.0,"stop_price":90.0,"stop_execution_price":89.1,
            "stop_execution_band_pct":1.0,"targets":[{"price":110.0,"base_lots":100}],
            "client_order_id":123,"last_valid_slot":1000}});
    let request: native::Request = serde_json::from_value(input.clone()).unwrap();
    let ix = Instruction { program_id:*PHOENIX_PROGRAM_ID, accounts:vec![], data:vec![1] };
    let mut tx = solana_transaction::Transaction::new_with_payer(&[ix], Some(&signer.pubkey()));
    tx.try_sign(&[&signer], tx.message.recent_blockhash).unwrap();
    let mut prepared = native::prepared_transaction(&tx, signer.pubkey()).unwrap();
    prepared["blockhash"] = serde_json::json!(tx.message.recent_blockhash.to_string());
    prepared["binding_signature"] = serde_json::json!(signer.sign_message(&native::preparation_context(&request, &prepared).unwrap()).to_string());
    input["operation"] = serde_json::json!("read_submission");
    input["prepared"] = prepared;
    let request: native::Request = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(native::validate_prepared(&request, signer.pubkey()).unwrap(), tx);
    for (key, value) in [("base_lots",serde_json::json!(101)),("stop_execution_price",serde_json::json!(88.0)),
        ("symbol",serde_json::json!("OTHER")),("last_valid_slot",serde_json::json!(1001))] {
        let mut changed = input.clone(); changed["entry"][key] = value;
        assert!(native::validate_prepared(&serde_json::from_value(changed).unwrap(), signer.pubkey()).is_err());
    }
    for (key,value) in [("expected_state_sha256",serde_json::json!("b".repeat(64))),
        ("wallet_address",serde_json::json!(signer.pubkey().to_string())),("genesis_hash",serde_json::json!("other"))] {
        let mut changed = input.clone(); changed[key] = value;
        assert!(native::validate_prepared(&serde_json::from_value(changed).unwrap(), signer.pubkey()).is_err());
    }
    let mut changed = input.clone(); changed["prepared"]["signature"] = serde_json::json!("wrong");
    assert!(native::validate_prepared(&serde_json::from_value(changed).unwrap(), signer.pubkey()).is_err());
    assert_eq!(native::validate_prepared(&request, current.pubkey()).unwrap(), tx);
    let mut changed = input.clone(); changed["operation"] = serde_json::json!("submit_prepared");
    assert!(native::validate_prepared(&serde_json::from_value(changed).unwrap(), owner).is_err());
    let mut changed = input.clone();
    changed["prepared"]["binding_signature"] = serde_json::json!(current.sign_message(
        &native::preparation_context(&request, &input["prepared"]).unwrap()).to_string());
    assert!(native::validate_prepared(&serde_json::from_value(changed).unwrap(), current.pubkey()).is_err());
    let mut changed_tx = tx.clone(); changed_tx.message.account_keys[0] = current.pubkey();
    let mut changed = input.clone();
    changed["prepared"]["transaction_base64"] = serde_json::json!(STANDARD.encode(bincode::serialize(&changed_tx).unwrap()));
    assert!(native::validate_prepared(&serde_json::from_value(changed).unwrap(), current.pubkey()).is_err());
    // Even a valid owner-signed receipt is not a dedicated-delegate receipt.
    let owner_signer = Keypair::new_from_array([72; 32]);
    let mut owner_tx = tx.clone(); owner_tx.message.account_keys[0] = owner;
    owner_tx.try_sign(&[&owner_signer], owner_tx.message.recent_blockhash).unwrap();
    let mut owner_receipt = native::prepared_transaction(&owner_tx, owner).unwrap();
    owner_receipt["blockhash"] = serde_json::json!(owner_tx.message.recent_blockhash.to_string());
    owner_receipt["binding_signature"] = serde_json::json!(owner_signer.sign_message(
        &native::preparation_context(&request, &owner_receipt).unwrap()).to_string());
    let mut changed = input.clone(); changed["prepared"] = owner_receipt;
    assert!(native::validate_prepared(&serde_json::from_value(changed).unwrap(), current.pubkey()).is_err());
    // Parse the wire request directly: Value itself cannot retain a full u128.
    for kind in ["entry", "close"] {
        for id in [123_u128, u64::MAX as u128, u64::MAX as u128 + 1, u128::MAX] {
            let mut wire = input.clone();
            wire["operation"] = serde_json::json!(format!("prepare_{kind}"));
            wire.as_object_mut().unwrap().remove("prepared");
            if kind == "close" {
                wire.as_object_mut().unwrap().remove("entry");
                wire["close"] = serde_json::json!({"symbol":"BTC", "signed_base_lots":100,
                    "position_sequence":1,"conditionals":[[0,1]],"limit_price":99.0,
                    "max_quote_lots":500,"client_order_id":123,"last_valid_slot":1000});
            }
            let decode = |value: &serde_json::Value, client_id: u128| -> native::Request {
                serde_json::from_str(&serde_json::to_string(value).unwrap()
                    .replace("\"client_order_id\":123", &format!("\"client_order_id\":{client_id}"))).unwrap()
            };
            let mut receipt = native::prepared_transaction(&tx, signer.pubkey()).unwrap();
            receipt["blockhash"] = serde_json::json!(tx.message.recent_blockhash.to_string());
            let context = native::preparation_context(&decode(&wire, id), &receipt).unwrap();
            assert!(!std::str::from_utf8(&context).unwrap().contains("replace_stop"));
            assert_eq!(context, native::preparation_context(&decode(&wire, id), &receipt).unwrap());
            assert!(std::str::from_utf8(&context).unwrap().contains(&format!("\"client_order_id\":{id},")));
            receipt["binding_signature"] = serde_json::json!(signer.sign_message(&context).to_string());
            wire["prepared"] = receipt;
            for operation in ["read_submission", "submit_prepared"] {
                wire["operation"] = serde_json::json!(operation);
                assert_eq!(native::validate_prepared(&decode(&wire, id), signer.pubkey()).unwrap(), tx);
                let frozen = serde_json::to_vec(&wire).unwrap();
                let rotated = native::validate_prepared(&decode(&wire, id), current.pubkey());
                if operation == "read_submission" { assert_eq!(rotated.unwrap(), tx); }
                else { assert!(rotated.is_err()); }
                assert_eq!(serde_json::to_vec(&wire).unwrap(), frozen);
                assert!(native::validate_prepared(&decode(&wire, id - 1), signer.pubkey()).is_err());
                for (key, value) in [("expected_state_sha256",serde_json::json!("b".repeat(64))),
                    ("wallet_address",serde_json::json!(signer.pubkey().to_string())),
                    ("genesis_hash",serde_json::json!("other"))] {
                    let mut changed = wire.clone(); changed[key] = value;
                    assert!(native::validate_prepared(&decode(&changed, id), signer.pubkey()).is_err());
                }
                let mut changed = wire.clone(); changed[kind]["last_valid_slot"] = serde_json::json!(1001);
                assert!(native::validate_prepared(&decode(&changed, id), signer.pubkey()).is_err());
                let mut changed = wire.clone(); changed["prepared"]["signature"] = serde_json::json!("wrong");
                assert!(native::validate_prepared(&decode(&changed, id), signer.pubkey()).is_err());
            }
        }
    }
    let signature = tx.signatures[0].to_string();
    for (finalized,failed,slot,want) in [(false,false,0,"pending"),(false,true,8,"pending"),
        (true,false,8,"finalized"),(true,true,8,"failed")] {
        let result = native::project_submission(&signature,Some((finalized,failed,slot)),9,Some((101,100))).unwrap();
        assert_eq!(result["signature"],signature); assert_eq!(result["submission_state"],want);
        assert_eq!(result["send_attempted"],false);
    }
    assert!(native::project_submission(&signature,Some((true,false,10)),9,None).is_err());
    assert!(native::project_submission(&signature,Some((true,false,0)),9,None).is_err());
    for (height,want) in [(99,"pending"),(100,"pending"),(101,"expired")] {
        let result = native::project_submission(&signature,None,9,Some((height,100))).unwrap();
        assert_eq!(result["signature"],signature); assert_eq!(result["submission_state"],want);
        assert!(result["confirmation_slot"].is_null()); assert_eq!(result["send_attempted"],false);
        if want == "expired" {
            assert_eq!(result["expiry"],serde_json::json!({"finalized_block_height":101,
                "last_valid_block_height":100,"history_status_absent":true}));
        } else { assert!(result.get("expiry").is_none()); }
    }
    assert_eq!(native::project_submission(&signature,None,9,None).unwrap()["submission_state"],"pending");
    for invalid in [(0,100),(101,0)] {
        assert!(native::project_submission(&signature,None,9,Some(invalid)).is_err());
    }
    let frozen = bincode::serialize(&tx).unwrap();
    for uncertain in [false,true] {
        let mut calls=0;
        let result=native::dispatch_prepared_once(&tx, |original| {
            calls+=1;
            assert_eq!(bincode::serialize(original).unwrap(),frozen);
            if uncertain { Err(()) } else { Ok(signature.clone()) }
        }).unwrap();
        assert_eq!(calls,1); assert_eq!(result["submission_state"],"pending");
        assert_eq!(result["signature"],signature); assert_eq!(result["transport_uncertain"],uncertain);
        assert_eq!(bincode::serialize(&tx).unwrap(),frozen);
    }
    assert!(native::dispatch_prepared_once(&tx, |_| Ok("different".to_owned())).is_err());
    let before = serde_json::json!({"symbol":"BTC","signed_base_lots":100,"conditionals":[]});
    use sha2::{Digest, Sha256};
    let hash = format!("{:x}",Sha256::digest(serde_json::to_vec(&before).unwrap()));
    let mut replacement = input.clone();
    replacement.as_object_mut().unwrap().remove("entry");
    replacement["expected_state_sha256"] = serde_json::json!(hash);
    replacement["replace_stop"] = serde_json::json!({"symbol":"BTC","signed_base_lots":100,"position_sequence":1,
        "conditionals":[[0,1]],"old_stop":[0,1],"trigger_price":95.0,"execution_price":94.5,"last_valid_slot":1000});
    let mut receipt = native::prepared_transaction(&tx,signer.pubkey()).unwrap();
    receipt["blockhash"] = serde_json::json!(tx.message.recent_blockhash.to_string());
    receipt["replace_before"] = before;
    let request: native::Request = serde_json::from_value(replacement.clone()).unwrap();
    receipt["binding_signature"] = serde_json::json!(signer.sign_message(&native::preparation_context(&request,&receipt).unwrap()).to_string());
    replacement["prepared"] = receipt;
    for operation in ["read_submission","submit_prepared"] {
        replacement["operation"] = serde_json::json!(operation);
        assert_eq!(native::validate_prepared(&serde_json::from_value(replacement.clone()).unwrap(),signer.pubkey()).unwrap(),tx);
        let frozen = serde_json::to_vec(&replacement).unwrap();
        let rotated = native::validate_prepared(&serde_json::from_value(replacement.clone()).unwrap(),current.pubkey());
        if operation == "read_submission" { assert_eq!(rotated.unwrap(),tx); }
        else { assert!(rotated.is_err()); }
        assert_eq!(serde_json::to_vec(&replacement).unwrap(),frozen);
        let mut bad = replacement.clone(); bad["replace_stop"]["old_stop"] = serde_json::json!([0,2]);
        assert!(native::validate_prepared(&serde_json::from_value(bad).unwrap(),signer.pubkey()).is_err());
        let mut bad = replacement.clone(); bad["prepared"]["replace_before"]["signed_base_lots"] = serde_json::json!(99);
        assert!(native::validate_prepared(&serde_json::from_value(bad).unwrap(),signer.pubkey()).is_err());
        let mut bad = replacement.clone(); bad["close"] = serde_json::json!({"symbol":"BTC","signed_base_lots":100,
            "position_sequence":1,"conditionals":[],"limit_price":99.0,"max_quote_lots":500,"client_order_id":123,"last_valid_slot":1000});
        assert!(serde_json::from_value::<native::Request>(bad).unwrap().operation_symbol().is_err());
    }
    println!("UPSCALP_STATUS original_plan_bound=true exact_signature=true nonfinal_errors_pending=true stale_snapshot_refused=true io=0 vm=0");
}

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
    let current = Keypair::new_from_array([73; 32]).pubkey();
    let ix: Instruction = create_delegate_trader_ix(DelegateTraderParams::builder()
        .trader_wallet(owner).trader_account(address).new_position_authority(current).build().unwrap()).unwrap().into();
    ctx.send_instructions(vec![ix], &owner_seed, "synthetic-rotation");
    let rotated = FetchedAccount::new(ctx.account_data(&address), *PHOENIX_PROGRAM_ID, false);
    assert_eq!(native::validate_account(owner, delegate, address, &rotated).unwrap_err(), "delegate_not_authorized");
    assert_eq!(native::validate_account(owner, current, address, &rotated).unwrap()["delegate_address"],current.to_string());
    println!("UPSCALP_NATIVE_CONNECTION delegated=true account_and_program_bound=true rejects_owner_key_and_foreign_scope=true probe_writes=0 execution_ready=false");
}

#[tokio::test]
async fn delegated_entry_is_atomic_with_full_stop_and_exact_split_targets() {
    delegated_protected_lifecycle(true, 0).await;
}

#[tokio::test]
async fn delegated_short_is_atomic_with_full_stop_and_exact_split_targets() {
    delegated_protected_lifecycle(false, 0).await;
}

#[tokio::test]
async fn delegated_close_preserves_exact_remaining_protection_inventory() {
    for long in [true, false] {
        for removed in [0, 1, 3] {
            delegated_protected_lifecycle(long, removed).await;
        }
    }
}

#[tokio::test]
async fn stop_band_is_explicit_one_percent_after_tick_rounding() {
    let paths = find_sdk_localnet_program_paths().expect("pinned programs required; no skip");
    let mut ctx = SdkLocalnetContext::new(default_sdk_localnet_fixture().unwrap(), paths);
    ctx.execute_setup();
    let owner = ctx.actor_pubkey("taker0");
    let delegate = ctx.actor_pubkey("taker1");
    let address = ctx.actor_trader("taker0");
    let ix: Instruction = create_delegate_trader_ix(DelegateTraderParams::builder()
        .trader_wallet(owner).trader_account(address).new_position_authority(delegate).build().unwrap()).unwrap().into();
    ctx.send_instructions(vec![ix], &ctx.actor("taker0").seed, "synthetic-band-delegation");
    let data = ctx.account_data(&address);
    let account = FetchedAccount::new(data.clone(), *PHOENIX_PROGRAM_ID, false);
    let metadata = phoenix_metadata_from_fixture(&ctx.fixture);
    let builder = PhoenixTxBuilder::new(&metadata);
    let calc = builder.order_ticket_metadata("BTC").unwrap().market_calc;
    for long in [true, false] {
        let base = calc.price_to_ticks(if long { 98_000.0 } else { 102_000.0 }).unwrap().as_inner();
        let stop = base / 100 * 100 + 51;
        let boundary = if long { stop - stop / 100 } else { stop + stop / 100 };
        let plan = native::EntryPlan {
            symbol:"BTC".into(), direction:if long { "LONG" } else { "SHORT" }.into(),
            base_lots:100, max_quote_lots:2_000_000_000, reference_price:100_000.0,
            limit_price:if long { 101_000.0 } else { 99_000.0 },
            stop_price:calc.ticks_to_price(stop.into()), stop_execution_price:calc.ticks_to_price(boundary.into()),
            stop_execution_band_pct:1.0,
            targets:vec![native::Target {price:if long { 102_000.0 } else { 98_000.0 },base_lots:100}],
            client_order_id:123,last_valid_slot:1100,
        };
        assert!(native::build_protected_entry(&metadata,owner,delegate,&account,None,1000,&plan).await.is_ok());
        let outside = if long { boundary - 1 } else { boundary + 1 };
        for bad in [
            native::EntryPlan {stop_execution_price:calc.ticks_to_price(outside.into()),..plan.clone()},
            native::EntryPlan {stop_execution_price:plan.stop_price * if long {0.99} else {1.01},..plan.clone()},
            native::EntryPlan {stop_execution_band_pct:10.0,..plan.clone()},
            native::EntryPlan {stop_execution_band_pct:0.0,..plan.clone()},
            native::EntryPlan {stop_execution_band_pct:f64::NAN,..plan.clone()},
        ] {
            assert_eq!(native::build_protected_entry(&metadata,owner,delegate,&account,None,1000,&bad).await.unwrap_err(),"stop_execution_band_invalid");
        }
    }
    assert_eq!(data,ctx.account_data(&address));
    println!("UPSCALP_STOP_BAND explicit_one_percent=true long_short_tick_boundary=true outward_rounding_refused=true builder_writes=0");
}

async fn delegated_protected_lifecycle(long: bool, removed: usize) {
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
    // Pinned solana-clock Clock layout: slot, epoch-start seconds, epoch,
    // leader-schedule epoch, Unix seconds (40 bytes). Synthetic VM only.
    let clock_key = solana_pubkey::Pubkey::from_str("SysvarC1ock11111111111111111111111111111111").unwrap();
    let mut clock_account = ctx.svm.get_account(&clock_key).unwrap();
    let mut clock: (u64, i64, u64, u64, i64) = bincode::deserialize(&clock_account.data).unwrap();
    clock.4 = 1_791_650_000;
    clock_account.data = bincode::serialize(&clock).unwrap();
    ctx.svm.set_account(clock_key, clock_account).unwrap();
    let mut signed_lots = if long { 100 } else { -100 };
    let plan = native::EntryPlan {
        symbol: "BTC".into(), direction: if long { "LONG" } else { "SHORT" }.into(), base_lots: 100, max_quote_lots: 2_000_000_000,
        reference_price: 100_000.0, limit_price: if long { 101_000.0 } else { 99_000.0 },
        stop_price: if long { 98_000.0 } else { 102_000.0 }, stop_execution_price: if long { 97_900.0 } else { 102_100.0 },
        stop_execution_band_pct: 1.0,
        targets: vec![native::Target { price: if long { 102_000.0 } else { 98_000.0 }, base_lots: 40 },
            native::Target { price: if long { 103_000.0 } else { 97_000.0 }, base_lots: 60 }],
        client_order_id: u128::MAX - 7, last_valid_slot: slot + 100,
    };
    for invalid in [
        native::EntryPlan { last_valid_slot: slot, ..plan.clone() },
        native::EntryPlan { stop_price: f64::NAN, ..plan.clone() },
        native::EntryPlan { stop_price: if long { 101_000.0 } else { 99_000.0 }, ..plan.clone() },
        native::EntryPlan { base_lots: 99, ..plan.clone() },
        native::EntryPlan { targets: vec![native::Target { price: if long { 99_000.0 } else { 101_000.0 }, base_lots: 100 }], ..plan.clone() },
        native::EntryPlan { targets: vec![native::Target { price: if long { 100_500.0 } else { 99_500.0 }, base_lots: 100 }], ..plan.clone() },
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
    // Execute the exact retained bytes so economics bind to this transaction,
    // not a rebuilt message or the later position snapshot.
    let mut executed_tx = tx.clone();
    executed_tx.try_sign(&[&signer], ctx.svm.latest_blockhash()).unwrap();
    let receipt = ctx.svm.send_transaction(executed_tx.clone()).unwrap();
    let inner: Vec<_> = receipt.inner_instructions.iter().enumerate().map(|(index, instructions)|
        serde_json::json!({"index":index, "instructions":instructions.iter().map(|ix|
            serde_json::json!({"programIdIndex":ix.instruction.program_id_index,
                "data":serde_json::to_value(solana_rpc_client::api::filter::Memcmp::new_base58_encoded(0, &ix.instruction.data)).unwrap()["bytes"],
                "stackHeight":ix.stack_height})).collect::<Vec<_>>()})).collect();
    let record = serde_json::json!({"slot":slot, "transaction":[STANDARD.encode(bincode::serialize(&executed_tx).unwrap()),"base64"],
        "meta":{"err":null,"fee":receipt.fee,"innerInstructions":inner}});
    let execution = native::project_execution(&metadata, owner, delegate, &plan, &executed_tx, slot, &record).unwrap();
    assert!(native::project_execution(&metadata, owner, Keypair::new_from_array([73; 32]).pubkey(), &plan, &executed_tx, slot, &record).is_err());
    assert_eq!(native::project_execution(&metadata, owner, executed_tx.message.account_keys[0],
        &plan, &executed_tx, slot, &record).unwrap(), execution);
    assert_eq!(execution["base_lots_filled"], 100);
    assert_eq!(execution["signed_base_lots"], signed_lots);
    assert!(execution["average_price"].as_f64().unwrap() > 0.0);
    assert_eq!(execution["fee_lamports"], receipt.fee);
    for (key, value) in [("slot", serde_json::json!(slot+1)), ("transaction",serde_json::json!(["wrong","base64"]))] {
        let mut bad = record.clone(); bad[key] = value;
        assert!(native::project_execution(&metadata, owner, delegate, &plan, &executed_tx, slot, &bad).is_err());
    }
    let mut no_events = record.clone(); no_events["meta"]["innerInstructions"] = serde_json::json!([]);
    assert!(native::project_execution(&metadata, owner, delegate, &plan, &executed_tx, slot, &no_events).is_err());
    let wrong_plan = native::EntryPlan {client_order_id:plan.client_order_id+1, ..plan.clone()};
    assert!(native::project_execution(&metadata, owner, delegate, &wrong_plan, &executed_tx, slot, &record).is_err());
    let state = Trader::try_from_account_bytes(&ctx.account_data(&address)).unwrap();
    assert_eq!(state.position_entries().find(|row| row.asset_id == 0).unwrap().position.base_lot_position.as_inner(), signed_lots);
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
    let exit_side = if long { StopLossTradeSide::Ask } else { StopLossTradeSide::Bid };
    let stop_trigger = if long { &stop.less_trigger_order } else { &stop.greater_trigger_order };
    assert!(stop_trigger.is_active);
    assert!(!(if long { &stop.greater_trigger_order } else { &stop.less_trigger_order }).is_active);
    assert_eq!(stop_trigger.trigger_price, calc.price_to_ticks(plan.stop_price).unwrap());
    assert_eq!(stop_trigger.execution_price, calc.price_to_ticks(plan.stop_execution_price).unwrap());
    assert_eq!(stop_trigger.order_kind, StopLossOrderKind::IOC);
    assert_eq!(stop_trigger.trade_side, exit_side);
    assert_eq!(stop_trigger.execution_direction, if long { StopLossDirection::LessThan } else { StopLossDirection::GreaterThan });
    for target in &plan.targets {
        let order = orders.iter().find(|order| !order.use_percent && order.max_size.as_inner() == target.base_lots).unwrap();
        let trigger = if long { &order.greater_trigger_order } else { &order.less_trigger_order };
        assert!(trigger.is_active);
        assert!(!(if long { &order.less_trigger_order } else { &order.greater_trigger_order }).is_active);
        assert_eq!(trigger.trigger_price, calc.price_to_ticks(target.price).unwrap());
        assert_eq!(trigger.execution_price, calc.price_to_ticks(target.price).unwrap());
        assert_eq!(trigger.order_kind, StopLossOrderKind::Limit);
        assert_eq!(trigger.trade_side, exit_side);
        assert_eq!(trigger.execution_direction, if long { StopLossDirection::GreaterThan } else { StopLossDirection::LessThan });
    }
    let current = FetchedAccount::new(ctx.account_data(&address), *PHOENIX_PROGRAM_ID, false);
    assert_eq!(native::build_protected_entry(&metadata, owner, delegate, &current, None, slot, &plan).await.unwrap_err(), "existing_position_refused");
    let book_address = solana_pubkey::Pubkey::from_str(&metadata.get_market("BTC").unwrap().market_pubkey).unwrap();
    let book_account = FetchedAccount::new(ctx.account_data(&book_address), *PHOENIX_PROGRAM_ID, false);
    let projection = native::project_state(&metadata, owner, delegate, "BTC", &current,
        Some(&FetchedAccount::new(ctx.account_data(&conditional), *PHOENIX_PROGRAM_ID, false)), &book_account).unwrap();
    assert_eq!(projection["signed_base_lots"], signed_lots);
    assert_eq!(projection["conditionals"].as_array().unwrap().len(), 3);
    assert_eq!(projection["nonzero_position_count"], 1);
    assert_eq!(projection["taker_fee"], metadata.get_market("BTC").unwrap().taker_fee);
    assert_eq!(projection["price_source"], "phoenix_orderbook_last_trade");
    assert_eq!(projection["last_trade_slot"], slot);
    assert!(projection["last_trade_price"].as_f64().unwrap() > 0.0);
    assert!(native::project_state(&metadata, owner, delegate, "BTC", &current, None,
        &FetchedAccount::new(book_account.data.clone(), owner, false)).is_err());
    let mut wrong_market = metadata.exchange().clone();
    wrong_market.markets.get_mut("BTC").unwrap().tick_size += 1;
    assert!(native::project_state(&PhoenixMetadata::new(wrong_market), owner, delegate, "BTC", &current, None, &book_account).is_err());
    if removed == 0 {
        let conditional_account = FetchedAccount::new(ctx.account_data(&conditional), *PHOENIX_PROGRAM_ID, false);
        let old_stop = collection.active_orders().find(|(_, o)| o.use_percent).map(|(i,o)| (i,o.sequence_number)).unwrap();
        let replace = native::ReplaceStopPlan {symbol:"BTC".into(),signed_base_lots:signed_lots,
            position_sequence:state.position_entries().find(|p| p.asset_id == 0).unwrap().position.position_sequence_number,
            conditionals:collection.active_orders().map(|(i,o)| (i,o.sequence_number)).collect(), old_stop,
            trigger_price:if long {99_000.0} else {101_000.0},execution_price:if long {98_900.0} else {101_100.0},last_valid_slot:slot+100};
        for bad in [
            native::ReplaceStopPlan {signed_base_lots:1,..replace.clone()},
            native::ReplaceStopPlan {position_sequence:replace.position_sequence.wrapping_add(1),..replace.clone()},
            native::ReplaceStopPlan {old_stop:(7,u64::MAX),..replace.clone()},
            native::ReplaceStopPlan {conditionals:vec![],..replace.clone()},
            native::ReplaceStopPlan {last_valid_slot:slot,..replace.clone()},
            native::ReplaceStopPlan {trigger_price:if long {97_000.0} else {103_000.0},..replace.clone()},
            native::ReplaceStopPlan {execution_price:if long {95_000.0} else {105_000.0},..replace.clone()},
            native::ReplaceStopPlan {execution_price:f64::NAN,..replace.clone()},
        ] {
            assert!(native::build_replace_stop(&metadata,owner,delegate,&current,&conditional_account,slot,&bad).is_err());
        }
        let replacement = native::build_replace_stop(&metadata,owner,delegate,&current,&conditional_account,slot,&replace).unwrap();
        assert_eq!(replacement.len(),2);
        let mut broken = replacement.clone(); broken[1].data[0] ^= 0xff;
        assert!(ctx.try_send_instructions_with_metadata(broken,&delegate_seed).is_err());
        assert_eq!(ctx.account_data(&conditional), conditional_account.data);
        assert_eq!(ctx.account_data(&address), current.data);
        ctx.send_instructions(replacement,&delegate_seed,"atomic-tighten-stop-preserving-targets");
        let after = native::project_state(&metadata,owner,delegate,"BTC",
            &FetchedAccount::new(ctx.account_data(&address),*PHOENIX_PROGRAM_ID,false),
            Some(&FetchedAccount::new(ctx.account_data(&conditional),*PHOENIX_PROGRAM_ID,false)),&book_account).unwrap();
        let verified = native::project_replacement(&metadata,&replace,&projection,&after).unwrap();
        assert_eq!(verified["status"],"verified");
        assert_eq!(verified["remaining_base_lots"],100);
        let current_after = FetchedAccount::new(ctx.account_data(&address),*PHOENIX_PROGRAM_ID,false);
        let conditional_after = FetchedAccount::new(ctx.account_data(&conditional),*PHOENIX_PROGRAM_ID,false);
        assert!(native::build_replace_stop(&metadata,owner,delegate,&current_after,&conditional_after,slot,&replace).is_err());
        let mut drift = after.clone(); drift["signed_base_lots"] = serde_json::json!(signed_lots/2);
        assert!(native::project_replacement(&metadata,&replace,&projection,&drift).is_err());
        let mut drift = after.clone(); drift["conditionals"].as_array_mut().unwrap().pop();
        assert!(native::project_replacement(&metadata,&replace,&projection,&drift).is_err());
        // A synthetic partial reduction and exact TP1 removal leave 60 lots.
        // This is replacement coverage, not evidence that a TP filled.
        let tp1 = collection.active_orders().find(|(_,o)| !o.use_percent && o.max_size.as_inner() == 40).unwrap().0;
        let partial = phoenix_rise::core::MarketOrderTicket::builder().authority(delegate).trader_account(address)
            .symbol("BTC").side(if long {phoenix_rise::ix::prelude::Side::Ask} else {phoenix_rise::ix::prelude::Side::Bid})
            .price(if long {99_000.0} else {101_000.0}).num_base_lots(40).num_quote_lots(2_000_000_000)
            .min_base_lots_to_fill(40).client_order_id(126).last_valid_slot(slot+100)
            .self_trade_behavior(phoenix_rise::ix::prelude::SelfTradeBehavior::Abort)
            .order_flags(phoenix_rise::ix::prelude::OrderFlags::ReduceOnly).cancel_existing(false).build().unwrap();
        let mut partial_ixs = builder.place_market_order(partial).await.unwrap();
        partial_ixs.push(create_cancel_conditional_order_ix(CancelConditionalOrderParams::builder()
            .trader_account(address).position_authority(delegate).orderbook(book_address)
            .conditional_order_index(tp1).disable_first(true).disable_second(true).build().unwrap()).unwrap().into());
        ctx.send_instructions(partial_ixs,&delegate_seed,"synthetic-partial-reduction-not-tp-fill");
        signed_lots = if long {60} else {-60};
        let partial_account = FetchedAccount::new(ctx.account_data(&address),*PHOENIX_PROGRAM_ID,false);
        let partial_orders = FetchedAccount::new(ctx.account_data(&conditional),*PHOENIX_PROGRAM_ID,false);
        let before_partial = native::project_state(&metadata,owner,delegate,"BTC",&partial_account,Some(&partial_orders),&book_account).unwrap();
        let partial_collection = ConditionalOrderCollection::try_from_account_bytes(&partial_orders.data).unwrap();
        let partial_plan = native::ReplaceStopPlan {signed_base_lots:signed_lots,
            conditionals:partial_collection.active_orders().map(|(i,o)| (i,o.sequence_number)).collect(),
            old_stop:partial_collection.active_orders().find(|(_,o)| o.use_percent).map(|(i,o)| (i,o.sequence_number)).unwrap(),
            trigger_price:if long {99_500.0} else {100_500.0},execution_price:if long {99_400.0} else {100_600.0},..replace.clone()};
        let partial_replace = native::build_replace_stop(&metadata,owner,delegate,&partial_account,&partial_orders,slot,&partial_plan).unwrap();
        ctx.send_instructions(partial_replace,&delegate_seed,"replace-full-remaining-sixty-lots");
        let after_partial = native::project_state(&metadata,owner,delegate,"BTC",
            &FetchedAccount::new(ctx.account_data(&address),*PHOENIX_PROGRAM_ID,false),
            Some(&FetchedAccount::new(ctx.account_data(&conditional),*PHOENIX_PROGRAM_ID,false)),&book_account).unwrap();
        assert_eq!(native::project_replacement(&metadata,&partial_plan,&before_partial,&after_partial).unwrap()["remaining_base_lots"],60);
        println!("UPSCALP_MANAGEMENT direction={} execution_verified=true replacement_atomic=true targets_preserved=true drift_refused=true",plan.direction);
    }
    if removed != 0 {
        let cancellations: Vec<Instruction> = collection.active_orders()
            .filter(|(_, order)| removed == 3 || order.use_percent)
            .map(|(index, _)| create_cancel_conditional_order_ix(CancelConditionalOrderParams::builder()
                .trader_account(address).position_authority(delegate).orderbook(book_address)
                .conditional_order_index(index).disable_first(true).disable_second(true)
                .build().unwrap()).unwrap().into()).collect();
        assert_eq!(cancellations.len(), removed);
        ctx.send_instructions(cancellations, &delegate_seed, "synthetic-missing-protection");
    }
    let current = FetchedAccount::new(ctx.account_data(&address), *PHOENIX_PROGRAM_ID, false);
    let remaining = Trader::try_from_account_bytes(&current.data).unwrap();
    let remaining_position = remaining.position_entries().find(|row| row.asset_id == 0).unwrap();
    assert_eq!(remaining_position.position.base_lot_position.as_inner(), signed_lots);
    assert_eq!(remaining_position.position.position_sequence_number,
        state.position_entries().find(|row| row.asset_id == 0).unwrap().position.position_sequence_number);
    let collection = ConditionalOrderCollection::try_from_account_bytes(&ctx.account_data(&conditional)).unwrap();
    assert_eq!(collection.active_orders().count(), if removed == 0 {2} else {3 - removed});
    let close = native::ClosePlan {
        symbol: "BTC".into(), signed_base_lots: signed_lots,
        position_sequence: state.position_entries().find(|row| row.asset_id == 0).unwrap().position.position_sequence_number,
        conditionals: collection.active_orders().map(|(index, order)| (index, order.sequence_number)).collect(),
        limit_price: if long { 99_000.0 } else { 101_000.0 }, max_quote_lots: 2_000_000_000, client_order_id: 124, last_valid_slot: slot + 100,
    };
    let conditional_data = ctx.account_data(&conditional);
    let conditional_account = FetchedAccount::new(conditional_data.clone(), *PHOENIX_PROGRAM_ID, false);
    for invalid in [
        native::ClosePlan { signed_base_lots: -signed_lots, ..close.clone() },
        native::ClosePlan { position_sequence: close.position_sequence.wrapping_add(1), ..close.clone() },
        native::ClosePlan { signed_base_lots: signed_lots + 1, ..close.clone() },
        native::ClosePlan { conditionals: if close.conditionals.is_empty() { vec![(0, 0)] } else { vec![] }, ..close.clone() },
        native::ClosePlan { conditionals: close.conditionals.iter().map(|(index, sequence)| (*index, sequence + 1))
            .chain(std::iter::once((7, u64::MAX))).collect(), ..close.clone() },
        native::ClosePlan { last_valid_slot: slot, ..close.clone() },
    ] {
        assert!(native::build_protected_close(&metadata, owner, delegate, &current, &conditional_account, slot, &invalid).await.is_err());
    }
    assert!(native::build_protected_close(&metadata, delegate, owner, &current, &conditional_account, slot, &close).await.is_err());
    assert_eq!(current.data, ctx.account_data(&address));
    assert_eq!(conditional_data, ctx.account_data(&conditional));
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
    println!("UPSCALP_NATIVE_CLOSE direction={} removed_before_close={} position_lots=0 retained_conditional_orders=0 close_cleanup_atomic=true", plan.direction, removed);
    println!("UPSCALP_NATIVE_ENTRY direction={} delegated=true signed_filled_lots={} stop_remaining_percent=100 target_lots=40,60 transaction_bytes={} protection_failure_rolls_back_entry=true duplicate_entry_refused=true", plan.direction, execution["signed_base_lots"], bytes.len());
}
