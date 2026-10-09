//! Copy into the pinned SDK's sdk/examples/ directory; build with its lockfile.
//! JSON stdin -> unsigned instruction JSON. No keypairs, VM, network or sender.
#[path = "../tests/test_harness/mod.rs"]
mod test_harness;

use std::{io::{self, Read}, str::FromStr, sync::Arc};
use phoenix_rise::core::{BracketLeg, BracketLegOrders, BracketLegTicket, LimitOrderTicket, PhoenixTxBuilder};
use phoenix_rise::ix::prelude::{OrderFlags, SelfTradeBehavior, Side};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    mode: String, fixture_sha256: String, setup_sha256: String, attempt_id: String,
    actor: String, instrument: String, direction: String, base_lots: String,
    limit_price_usd: String, stop_trigger_price_usd: String, stop_execution_price_usd: String,
    target_trigger_price_usd: String, target_execution_price_usd: String,
    stop_order_kind: String, target_order_kind: String,
}

fn exact_integer_price(value: &str) -> Result<f64, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || value.split('.').nth(1).is_some_and(|fraction| fraction.is_empty() || fraction.bytes().any(|b| b != b'0')) {
        return Err("plain positive price required".into());
    }
    let parsed: f64 = value.parse().map_err(|_| "price parse")?;
    if !parsed.is_finite() || parsed <= 0.0 || parsed > 1_000_000_000.0 || parsed.fract() != 0.0 {
        return Err("exact pinned-fixture integer price required".into());
    }
    Ok(parsed)
}

#[tokio::main]
async fn main() {
    if let Err(error) = build().await {
        eprintln!("unsigned fixture refused: {error}");
        std::process::exit(2);
    }
}

async fn build() -> Result<(), String> {
    let mut raw = String::new();
    io::stdin().take(16_385).read_to_string(&mut raw).map_err(|_| "input read")?;
    if raw.len() > 16_384 { return Err("bounded request required".into()); }
    let request: Request = serde_json::from_str(&raw).map_err(|_| "request shape")?;
    let fixture_hash = format!("{:x}", Sha256::digest(test_harness::DEFAULT_SDK_LOCALNET_FIXTURE_JSON.as_bytes()));
    if request.mode != "unsigned-synthetic-only" || request.fixture_sha256 != fixture_hash
        || fixture_hash != "7527b2344ee3462f844092b5821ef80d8662bce9a1ec553b71c4e34499edf309"
        || request.actor != "taker0" || request.instrument != "BTC" || request.direction != "LONG"
        || request.stop_order_kind != "IOC" || request.target_order_kind != "LIMIT"
        || request.setup_sha256.len() != 64
        || !request.setup_sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || request.attempt_id.is_empty() || request.attempt_id.len() > 81 {
        return Err("pinned synthetic binding required".into());
    }
    if request.base_lots.is_empty() || request.base_lots.len() > 10
        || !request.base_lots.bytes().all(|b| b.is_ascii_digit()) {
        return Err("explicit base lots required".into());
    }
    let lots: u64 = request.base_lots.parse().map_err(|_| "base lots")?;
    let entry = exact_integer_price(&request.limit_price_usd)?;
    let stop = exact_integer_price(&request.stop_trigger_price_usd)?;
    let stop_execution = exact_integer_price(&request.stop_execution_price_usd)?;
    let target = exact_integer_price(&request.target_trigger_price_usd)?;
    let target_execution = exact_integer_price(&request.target_execution_price_usd)?;
    if lots == 0 || stop >= entry || target <= entry || stop_execution > stop || target_execution != target {
        return Err("explicit bracket geometry required".into());
    }
    let fixture = test_harness::default_sdk_localnet_fixture().map_err(|_| "fixture")?;
    let actor = fixture.actors.iter().find(|a| a.name == request.actor).ok_or("fixture actor")?;
    let metadata = test_harness::phoenix_metadata_from_fixture(&fixture);
    let builder = PhoenixTxBuilder::new(&metadata);
    let bracket = BracketLegTicket::new(Arc::new(RpcClient::new_mock("succeeds".to_string())),
        BracketLegOrders {
            stop_loss: Some(BracketLeg::new(stop).with_ioc_order().with_execution_price(stop_execution)),
            take_profit: Some(BracketLeg::new(target).with_limit_order().with_execution_price(target_execution)),
        });
    let ticket = LimitOrderTicket::builder()
        .authority(Pubkey::from_str(&actor.pubkey).map_err(|_| "fixture authority")?)
        .trader_account(Pubkey::from_str(&actor.trader_account).map_err(|_| "fixture trader")?)
        .symbol("BTC").side(Side::Bid).price(entry).num_base_lots(lots)
        .self_trade_behavior(SelfTradeBehavior::CancelProvide).order_flags(OrderFlags::None)
        .bracket_leg_ticket(bracket).build().map_err(|_| "ticket")?;
    let instructions = builder.place_limit_order(ticket).await.map_err(|_| "SDK builder")?;
    if instructions.len() != 2 { return Err("unexpected fixture instruction count".into()); }
    let encoded: Vec<_> = instructions.iter().map(|ix| json!({
        "program_id": ix.program_id.to_string(),
        "accounts": ix.accounts.iter().map(|a| json!({"pubkey": a.pubkey.to_string(),
            "is_signer": a.is_signer, "is_writable": a.is_writable})).collect::<Vec<_>>(),
        "data_hex": ix.data.iter().map(|b| format!("{b:02x}")).collect::<String>(),
    })).collect();
    println!("{}", json!({"mode": "unsigned-synthetic-only",
        "request_sha256": format!("{:x}", Sha256::digest(raw.as_bytes())), "instructions": encoded}));
    Ok(())
}
