// Built as an example of the pinned Phoenix Rust SDK 0.6.6. JSON on stdin only;
// never read a wallet file, environment secret, or default wallet.
use std::{io::{self, Read}, str::FromStr, time::Duration};

use phoenix_rise::{accounts::owned::{ConditionalOrderCollection, Orderbook, Trader}, core::{BracketLeg, BracketLegOrders, BracketLegSize, FetchedAccount, MarketOrderTicket, PhoenixMetadata, PhoenixTxBuilder, TraderKey}, ix::{PHOENIX_PROGRAM_ID, prelude::{OrderFlags, SelfTradeBehavior, Side, CancelConditionalOrderParams, create_cancel_conditional_order_ix}}};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{json, Value};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_signer::Signer;
use solana_transaction::Transaction;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    wallet_address: String,
    private_key: String,
    rpc_url: String,
    genesis_hash: String,
}

pub fn parse_identity(wallet: &str, secret: &str) -> Result<(Pubkey, Keypair), &'static str> {
    let owner = Pubkey::from_str(wallet).map_err(|_| "wallet_address_invalid")?;
    let signer = Keypair::try_from_base58_string(secret).map_err(|_| "delegate_keypair_invalid")?;
    if signer.pubkey() == owner || owner == Pubkey::default() {
        return Err("dedicated_delegate_required");
    }
    Ok((owner, signer))
}

pub fn validate_account(owner: Pubkey, delegate: Pubkey, address: Pubkey, account: &FetchedAccount) -> Result<Value, &'static str> {
    // The existing setup fields select the primary cross-margin trader only.
    let expected = TraderKey::new(owner).pda();
    if owner == delegate || address != expected || account.owner != Some(*PHOENIX_PROGRAM_ID) || account.executable {
        return Err("trader_account_identity_invalid");
    }
    let trader = Trader::try_from_account_bytes(&account.data).map_err(|_| "trader_account_invalid")?;
    if trader.authority != owner || trader.key != expected || trader.trader_pda_index != 0 || trader.trader_subaccount_index != 0 {
        return Err("trader_account_binding_invalid");
    }
    if trader.position_authority != delegate {
        return Err("delegate_not_authorized");
    }
    Ok(json!({
        "status": "ok", "platform": "phoenix", "scope": "primary_cross_margin",
        "wallet_address": owner.to_string(), "delegate_address": delegate.to_string(),
        "trader_account": expected.to_string(), "program_id": PHOENIX_PROGRAM_ID.to_string(),
        "delegation_verified": true,
        "execution_ready": false
    }))
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub price: f64,
    pub base_lots: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryPlan {
    pub symbol: String,
    pub direction: String,
    pub base_lots: u64,
    pub max_quote_lots: u64,
    pub reference_price: f64,
    pub limit_price: f64,
    pub stop_price: f64,
    // Required explicitly: the SDK's default 10% IOC band is not product policy.
    pub stop_execution_price: f64,
    pub targets: Vec<Target>,
    pub client_order_id: u128,
    pub last_valid_slot: u64,
}

pub async fn build_protected_entry(
    metadata: &PhoenixMetadata, owner: Pubkey, delegate: Pubkey,
    trader_account: &FetchedAccount, conditional_account: Option<&FetchedAccount>,
    current_slot: u64, plan: &EntryPlan,
) -> Result<Vec<Instruction>, &'static str> {
    let trader = TraderKey::new(owner).pda();
    validate_account(owner, delegate, trader, trader_account)?;
    let builder = PhoenixTxBuilder::new(metadata);
    let market = metadata.get_market(&plan.symbol).ok_or("market_unknown")?;
    let state = Trader::try_from_account_bytes(&trader_account.data).map_err(|_| "trader_account_invalid")?;
    if state.position_entries().any(|row| row.asset_id == u64::from(market.asset_id) && row.position.base_lot_position.as_inner() != 0) {
        return Err("existing_position_refused");
    }
    let long = match plan.direction.as_str() { "LONG" => true, "SHORT" => false, _ => return Err("direction_invalid") };
    if current_slot == 0 || plan.last_valid_slot <= current_slot || plan.client_order_id == 0
        || plan.base_lots == 0 || plan.max_quote_lots == 0 || !(1..=2).contains(&plan.targets.len())
        || plan.targets.iter().try_fold(0_u64, |sum, target| sum.checked_add(target.base_lots)) != Some(plan.base_lots)
        || plan.targets.iter().any(|target| target.base_lots == 0) {
        return Err("entry_size_or_expiry_invalid");
    }
    let calc = builder.order_ticket_metadata(&plan.symbol).map_err(|_| "market_unknown")?.market_calc;
    let ticks = |price: f64| -> Result<u64, &'static str> {
        if !price.is_finite() || price <= 0.0 { return Err("price_invalid"); }
        let value = calc.price_to_ticks(price).map_err(|_| "price_invalid")?.as_inner();
        if value == 0 || value == u64::MAX { return Err("price_invalid"); }
        Ok(value)
    };
    let entry = ticks(plan.reference_price)?;
    let limit = ticks(plan.limit_price)?;
    let stop = ticks(plan.stop_price)?;
    let stop_execution = ticks(plan.stop_execution_price)?;
    if (long && (limit < entry || stop >= entry || stop_execution > stop))
        || (!long && (limit > entry || stop <= entry || stop_execution < stop)) {
        return Err("protection_geometry_invalid");
    }
    let mut previous = limit;
    for target in &plan.targets {
        let price = ticks(target.price)?;
        if (long && price <= previous) || (!long && price >= previous) { return Err("target_geometry_invalid"); }
        previous = price;
    }
    let mut ixs = Vec::new();
    if let Some(account) = conditional_account {
        if account.owner != Some(*PHOENIX_PROGRAM_ID) || account.executable { return Err("conditional_account_invalid"); }
        let collection = ConditionalOrderCollection::try_from_account_bytes(&account.data).map_err(|_| "conditional_account_invalid")?;
        if collection.header.trader_key != trader
            || collection.active_orders().any(|(_, order)| order.asset_id == market.asset_id)
            || (collection.header.capacity as usize) < collection.active_order_indices.len() + plan.targets.len() + 1 {
            return Err("conditional_inventory_refused");
        }
    } else {
        ixs.extend(builder.build_create_conditional_orders_account(delegate, delegate, trader, 8).map_err(|_| "conditional_build_failed")?);
    }
    let side = if long { Side::Bid } else { Side::Ask };
    let ticket = MarketOrderTicket::builder().authority(delegate).trader_account(trader)
        .symbol(&plan.symbol).side(side).price(plan.limit_price).num_base_lots(plan.base_lots)
        .num_quote_lots(plan.max_quote_lots).min_base_lots_to_fill(plan.base_lots)
        .client_order_id(plan.client_order_id).last_valid_slot(plan.last_valid_slot)
        .self_trade_behavior(SelfTradeBehavior::Abort).order_flags(OrderFlags::None)
        .cancel_existing(false).build().map_err(|_| "entry_build_failed")?;
    ixs.extend(builder.place_market_order(ticket).await.map_err(|_| "entry_build_failed")?);
    // Atomic transaction: all entry lots and all protection, or none. The stop
    // tracks the remaining position; fixed TP lots preserve the configured split.
    ixs.extend(builder.build_bracket_leg_orders(delegate, trader, &plan.symbol, side, &BracketLegOrders {
        stop_loss: Some(BracketLeg::new(plan.stop_price).with_ioc_order().with_execution_price(plan.stop_execution_price)
            .with_size(BracketLegSize::PositionPercent(100))), take_profit: None,
    }).map_err(|_| "stop_build_failed")?);
    for target in &plan.targets {
        ixs.extend(builder.build_bracket_leg_orders(delegate, trader, &plan.symbol, side, &BracketLegOrders {
            stop_loss: None, take_profit: Some(BracketLeg::new(target.price).with_limit_order().with_execution_price(target.price)
                .with_size(BracketLegSize::BaseLots(target.base_lots))),
        }).map_err(|_| "target_build_failed")?);
    }
    Ok(ixs)
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosePlan {
    pub symbol: String,
    pub signed_base_lots: i64,
    pub position_sequence: u8,
    pub conditionals: Vec<(u8, u64)>,
    pub limit_price: f64,
    pub max_quote_lots: u64,
    pub client_order_id: u128,
    pub last_valid_slot: u64,
}

pub async fn build_protected_close(
    metadata: &PhoenixMetadata, owner: Pubkey, delegate: Pubkey,
    trader_account: &FetchedAccount, conditional_account: &FetchedAccount,
    current_slot: u64, plan: &ClosePlan,
) -> Result<Vec<Instruction>, &'static str> {
    let trader = TraderKey::new(owner).pda();
    validate_account(owner, delegate, trader, trader_account)?;
    let market = metadata.get_market(&plan.symbol).ok_or("market_unknown")?;
    let state = Trader::try_from_account_bytes(&trader_account.data).map_err(|_| "trader_account_invalid")?;
    let position = state.position_entries().find(|row| row.asset_id == u64::from(market.asset_id)).ok_or("position_absent")?;
    if plan.signed_base_lots == 0 || plan.signed_base_lots == i64::MIN
        || position.position.base_lot_position.as_inner() != plan.signed_base_lots
        || position.position.position_sequence_number != plan.position_sequence {
        return Err("position_binding_changed");
    }
    if current_slot == 0 || plan.last_valid_slot <= current_slot || plan.client_order_id == 0
        || plan.max_quote_lots == 0 || !plan.limit_price.is_finite() || plan.limit_price <= 0.0 {
        return Err("close_price_or_expiry_invalid");
    }
    if conditional_account.owner != Some(*PHOENIX_PROGRAM_ID) || conditional_account.executable {
        return Err("conditional_account_invalid");
    }
    let collection = ConditionalOrderCollection::try_from_account_bytes(&conditional_account.data).map_err(|_| "conditional_account_invalid")?;
    let actual: Vec<_> = collection.active_orders().filter(|(_, order)| order.asset_id == market.asset_id)
        .map(|(index, order)| (index, order.sequence_number)).collect();
    if collection.header.trader_key != trader || actual != plan.conditionals || actual.is_empty() || actual.len() > 3 {
        return Err("conditional_inventory_changed");
    }
    let builder = PhoenixTxBuilder::new(metadata);
    let ticks = builder.order_ticket_metadata(&plan.symbol).map_err(|_| "market_unknown")?.market_calc
        .price_to_ticks(plan.limit_price).map_err(|_| "close_price_or_expiry_invalid")?.as_inner();
    if ticks == 0 || ticks == u64::MAX { return Err("close_price_or_expiry_invalid"); }
    let lots = plan.signed_base_lots.unsigned_abs();
    let ticket = MarketOrderTicket::builder().authority(delegate).trader_account(trader)
        .symbol(&plan.symbol).side(if plan.signed_base_lots > 0 { Side::Ask } else { Side::Bid })
        .price(plan.limit_price).num_base_lots(lots).num_quote_lots(plan.max_quote_lots).min_base_lots_to_fill(lots)
        .client_order_id(plan.client_order_id).last_valid_slot(plan.last_valid_slot)
        .self_trade_behavior(SelfTradeBehavior::Abort).order_flags(OrderFlags::ReduceOnly)
        .cancel_existing(false).build().map_err(|_| "close_build_failed")?;
    let mut ixs = builder.place_market_order(ticket).await.map_err(|_| "close_build_failed")?;
    // Close and disable exactly the observed, ledger-bound protection together.
    // No cancel-all and no source of authority to adopt an external position.
    let orderbook = Pubkey::from_str(&market.market_pubkey).map_err(|_| "market_invalid")?;
    for (index, _) in actual {
        let params = CancelConditionalOrderParams::builder().trader_account(trader).position_authority(delegate)
            .orderbook(orderbook).conditional_order_index(index).disable_first(true).disable_second(true)
            .build().map_err(|_| "close_build_failed")?;
        ixs.push(create_cancel_conditional_order_ix(params).map_err(|_| "close_build_failed")?.into());
    }
    Ok(ixs)
}

// Pure decoding boundary exercised with retained local VM bytes. This function
// does not fetch data, sign, submit, or activate a runtime path.
pub fn project_state(
    metadata: &PhoenixMetadata, owner: Pubkey, delegate: Pubkey, symbol: &str,
    trader_account: &FetchedAccount, conditional_account: Option<&FetchedAccount>,
    orderbook_account: &FetchedAccount,
) -> Result<Value, &'static str> {
    let trader = TraderKey::new(owner).pda();
    validate_account(owner, delegate, trader, trader_account)?;
    let market = metadata.get_market(symbol).ok_or("market_unknown")?;
    if orderbook_account.owner != Some(*PHOENIX_PROGRAM_ID) || orderbook_account.executable {
        return Err("orderbook_account_invalid");
    }
    let book = Orderbook::try_from_account_bytes(&orderbook_account.data).map_err(|_| "orderbook_account_invalid")?;
    if book.header.asset_id != market.asset_id || book.header.asset_symbol != market.symbol
        || book.header.base_lots_decimals != market.base_lots_decimals
        || book.header.tick_size_in_quote_lots_per_base_lot != market.tick_size {
        return Err("market_metadata_mismatch");
    }
    let state = Trader::try_from_account_bytes(&trader_account.data).map_err(|_| "trader_account_invalid")?;
    let position = state.position_entries().find(|row| row.asset_id == u64::from(market.asset_id));
    let orders = if let Some(account) = conditional_account {
        if account.owner != Some(*PHOENIX_PROGRAM_ID) || account.executable { return Err("conditional_account_invalid"); }
        let collection = ConditionalOrderCollection::try_from_account_bytes(&account.data).map_err(|_| "conditional_account_invalid")?;
        if collection.header.trader_key != trader { return Err("conditional_account_invalid"); }
        collection.active_orders().filter(|(_, order)| order.asset_id == market.asset_id)
            .map(|(index, order)| json!({"index": index, "order": order})).collect::<Vec<_>>()
    } else { Vec::new() };
    let calc = metadata.get_market_calculator(symbol).ok_or("market_unknown")?;
    Ok(json!({
        "symbol": symbol, "asset_id": market.asset_id, "market_status": market.market_status,
        "isolated_only": market.isolated_only, "base_lots_decimals": market.base_lots_decimals,
        "tick_size": market.tick_size, "leverage_tiers": market.leverage_tiers, "taker_fee": market.taker_fee,
        "best_bid": book.bids.first().map(|row| calc.ticks_to_price(row.order_id.price_in_ticks)),
        "best_ask": book.asks.first().map(|row| calc.ticks_to_price(row.order_id.price_in_ticks)),
        "signed_base_lots": position.map(|row| row.position.base_lot_position.as_inner()).unwrap_or(0),
        "position_sequence": position.map(|row| row.position.position_sequence_number),
        "virtual_quote_lots": position.map(|row| row.position.virtual_quote_lot_position.as_inner()).unwrap_or(0),
        "quote_lot_collateral": state.state.quote_lot_collateral.as_inner(),
        "nonzero_position_count": state.position_entries().filter(|row| row.position.base_lot_position.as_inner() != 0).count(),
        "conditionals": orders
    }))
}

// Serialize an already-signed transaction; no signing or submission here.
pub fn prepared_transaction(tx: &Transaction, delegate: Pubkey) -> Result<Value, &'static str> {
    if tx.signatures.len() != 1 || tx.message.header.num_required_signatures != 1
        || tx.message.account_keys.first() != Some(&delegate) || tx.verify().is_err()
        || tx.message.instructions.is_empty()
        || tx.message.instructions.iter().any(|ix| tx.message.account_keys.get(usize::from(ix.program_id_index)) != Some(&*PHOENIX_PROGRAM_ID)) {
        return Err("prepared_transaction_invalid");
    }
    let bytes = bincode::serialize(tx).map_err(|_| "prepared_transaction_invalid")?;
    if bytes.len() > 1232 { return Err("transaction_packet_too_large"); }
    Ok(json!({"transaction_base64": STANDARD.encode(&bytes), "transaction_sha256": format!("{:x}", Sha256::digest(&bytes)),
        "signature": tx.signatures[0].to_string(), "instruction_count": tx.message.instructions.len(),
        "transaction_bytes": bytes.len(), "submitted": false}))
}

fn run(request: Request) -> Result<Value, &'static str> {
    if request.operation != "validate_connection" {
        return Err("operation_unsupported");
    }
    if !request.rpc_url.starts_with("https://") || request.rpc_url.trim() != request.rpc_url || request.genesis_hash.is_empty() {
        return Err("rpc_identity_required");
    }
    let (owner, signer) = parse_identity(&request.wallet_address, &request.private_key)?;
    let rpc = RpcClient::new_with_timeout_and_commitment(request.rpc_url, Duration::from_secs(15), CommitmentConfig::finalized());
    let genesis = rpc.get_genesis_hash().map_err(|_| "rpc_unavailable")?;
    if genesis.to_string() != request.genesis_hash {
        return Err("rpc_genesis_mismatch");
    }
    let address = TraderKey::new(owner).pda();
    let response = rpc.get_account_with_commitment(&address, CommitmentConfig::finalized()).map_err(|_| "rpc_unavailable")?;
    let account = response.value.ok_or("trader_account_absent")?;
    let mut result = validate_account(owner, signer.pubkey(), address, &FetchedAccount::new(account.data, account.owner, account.executable))?;
    result["finalized_slot"] = json!(response.context.slot);
    result["genesis_hash"] = json!(genesis.to_string());
    Ok(result)
}

fn main() {
    let mut input = String::new();
    let result = io::stdin().take(16_385).read_to_string(&mut input)
        .map_err(|_| "request_invalid")
        .and_then(|_| if input.len() > 16_384 { Err("request_too_large") } else { Ok(()) })
        .and_then(|_| serde_json::from_str::<Request>(&input).map_err(|_| "request_invalid"))
        .and_then(run);
    match result {
        Ok(value) => println!("{value}"),
        Err(code) => {
            // Never emit RPC error text, request bodies, endpoints, or key material.
            println!("{}", json!({"status": "refused", "code": code}));
            std::process::exit(1);
        }
    }
}
