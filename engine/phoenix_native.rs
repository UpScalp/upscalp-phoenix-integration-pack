// Built as an example of the pinned Phoenix Rust SDK 0.6.6. JSON on stdin only;
// never read a wallet file, environment secret, or default wallet.
use std::{io::{self, Read}, str::FromStr, time::Duration};

use phoenix_rise::{accounts::owned::{ConditionalOrderCollection, Orderbook, Trader, StopLossDirection, StopLossOrderKind, StopLossTradeSide}, core::{BracketLeg, BracketLegOrders, BracketLegSize, FetchedAccount, MarketOrderTicket, PhoenixMetadata, PhoenixTxBuilder, TraderKey}, ix::{PHOENIX_PROGRAM_ID, prelude::{OrderFlags, SelfTradeBehavior, Side, CancelConditionalOrderParams, create_cancel_conditional_order_ix}}};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client::api::config::{RpcSendTransactionConfig, RpcSimulateTransactionConfig};
use solana_signer::Signer;
use solana_transaction::Transaction;
use sha2::{Digest, Sha256};
use phoenix_rise::api::PhoenixHttpClient;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    operation: String,
    wallet_address: String,
    private_key: String,
    rpc_url: String,
    genesis_hash: String,
    api_url: Option<String>,
    symbol: Option<String>,
    entry: Option<EntryPlan>,
    close: Option<ClosePlan>,
    replace_stop: Option<ReplaceStopPlan>,
    expected_state_sha256: Option<String>,
    prepared: Option<Value>,
}

impl Request {
    // Reject ambiguous operation inputs before any RPC or key use.
    pub fn operation_symbol(&self) -> Result<Option<&str>, &'static str> {
        let retained = matches!(self.operation.as_str(), "read_submission" | "submit_prepared");
        if retained != self.prepared.is_some() {
            return Err("prepared_inputs_invalid");
        }
        let plans = usize::from(self.entry.is_some()) + usize::from(self.close.is_some()) + usize::from(self.replace_stop.is_some());
        let symbol = match self.operation.as_str() {
            "validate_connection" if self.symbol.is_none() && plans == 0
                && self.expected_state_sha256.is_none() && self.api_url.is_none() => return Ok(None),
            "read_state" if plans == 0 && self.expected_state_sha256.is_none() => self.symbol.as_deref(),
            "prepare_entry" if self.symbol.is_none() && plans == 1 => self.entry.as_ref().map(|p| p.symbol.as_str()),
            "prepare_close" if self.symbol.is_none() && plans == 1 => self.close.as_ref().map(|p| p.symbol.as_str()),
            "prepare_replace_stop" if self.symbol.is_none() && plans == 1 => self.replace_stop.as_ref().map(|p| p.symbol.as_str()),
            "read_submission" | "submit_prepared" if self.symbol.is_none() && plans == 1 =>
                self.entry.as_ref().map(|p| p.symbol.as_str()).or_else(|| self.close.as_ref().map(|p| p.symbol.as_str()))
                    .or_else(|| self.replace_stop.as_ref().map(|p| p.symbol.as_str())),
            _ => return Err("operation_inputs_invalid"),
        }.filter(|s| !s.is_empty() && s.trim() == *s).ok_or("symbol_required")?;
        if !self.api_url.as_deref().is_some_and(valid_https_endpoint) { return Err("metadata_endpoint_required"); }
        if (self.operation.starts_with("prepare_") || retained) && !self.expected_state_sha256.as_deref()
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))) {
            return Err("expected_state_required");
        }
        Ok(Some(symbol))
    }
}

fn valid_https_endpoint(value: &str) -> bool {
    value.strip_prefix("https://").is_some_and(|rest| {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        !authority.is_empty() && !authority.contains('@') && !value.contains('#')
            && !value.chars().any(|c| c.is_whitespace() || c.is_control())
    })
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub price: f64,
    pub base_lots: u64,
}

#[derive(Clone, Deserialize, Serialize)]
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
    pub stop_execution_band_pct: f64,
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
    // Phoenix-only 1% maximum, measured from the encoded trigger.
    // Integer tick arithmetic also refuses nearest rounding outside the band.
    if plan.stop_execution_band_pct != 1.0
        || u128::from(stop.abs_diff(stop_execution)) * 100 > u128::from(stop) {
        return Err("stop_execution_band_invalid");
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

#[derive(Clone, Deserialize, Serialize)]
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
    if collection.header.trader_key != trader || actual != plan.conditionals || actual.len() > 3 {
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceStopPlan {
    pub symbol: String,
    pub signed_base_lots: i64,
    pub position_sequence: u8,
    pub conditionals: Vec<(u8, u64)>,
    pub old_stop: (u8, u64),
    pub trigger_price: f64,
    pub execution_price: f64,
    pub last_valid_slot: u64,
}

pub fn build_replace_stop(metadata: &PhoenixMetadata, owner: Pubkey, delegate: Pubkey,
    trader_account: &FetchedAccount, conditional_account: &FetchedAccount,
    current_slot: u64, plan: &ReplaceStopPlan) -> Result<Vec<Instruction>, &'static str> {
    let trader = TraderKey::new(owner).pda();
    validate_account(owner, delegate, trader, trader_account)?;
    let market = metadata.get_market(&plan.symbol).ok_or("market_unknown")?;
    let state = Trader::try_from_account_bytes(&trader_account.data).map_err(|_| "trader_account_invalid")?;
    let position = state.position_entries().find(|p| p.asset_id == u64::from(market.asset_id)).ok_or("position_absent")?;
    if plan.signed_base_lots == 0 || plan.signed_base_lots == i64::MIN
        || position.position.base_lot_position.as_inner() != plan.signed_base_lots
        || position.position.position_sequence_number != plan.position_sequence {
        return Err("position_binding_changed");
    }
    if current_slot == 0 || plan.last_valid_slot <= current_slot { return Err("replacement_expired"); }
    if conditional_account.owner != Some(*PHOENIX_PROGRAM_ID) || conditional_account.executable {
        return Err("conditional_account_invalid");
    }
    let collection = ConditionalOrderCollection::try_from_account_bytes(&conditional_account.data).map_err(|_| "conditional_account_invalid")?;
    let orders: Vec<_> = collection.active_orders().filter(|(_, o)| o.asset_id == market.asset_id).collect();
    if collection.header.trader_key != trader || orders.is_empty() || orders.len() > 3
        || orders.iter().map(|(i, o)| (*i, o.sequence_number)).collect::<Vec<_>>() != plan.conditionals {
        return Err("conditional_inventory_changed");
    }
    let long = plan.signed_base_lots > 0;
    let old = orders.iter().find(|(i, o)| (*i, o.sequence_number) == plan.old_stop).ok_or("owned_stop_missing")?.1;
    let (trigger, opposite) = if long { (&old.less_trigger_order, &old.greater_trigger_order) }
        else { (&old.greater_trigger_order, &old.less_trigger_order) };
    if !trigger.is_active || opposite.is_active || !old.use_percent || old.percent != 100
        || trigger.position_sequence_number != plan.position_sequence || trigger.order_kind != StopLossOrderKind::IOC
        || trigger.trade_side != if long { StopLossTradeSide::Ask } else { StopLossTradeSide::Bid }
        || trigger.execution_direction != if long { StopLossDirection::LessThan } else { StopLossDirection::GreaterThan }
        || orders.iter().filter(|(_, o)| if long { o.less_trigger_order.is_active } else { o.greater_trigger_order.is_active }).count() != 1 {
        return Err("owned_full_stop_required");
    }
    let builder = PhoenixTxBuilder::new(metadata);
    let calc = builder.order_ticket_metadata(&plan.symbol).map_err(|_| "market_unknown")?.market_calc;
    let ticks = |p: f64| -> Result<u64, &'static str> {
        if !p.is_finite() || p <= 0.0 { return Err("replacement_geometry_invalid"); }
        let t = calc.price_to_ticks(p).map_err(|_| "replacement_geometry_invalid")?.as_inner();
        if t == 0 || t == u64::MAX { return Err("replacement_geometry_invalid"); } Ok(t)
    };
    let stop = ticks(plan.trigger_price)?;
    let execution = ticks(plan.execution_price)?;
    if (long && (stop < trigger.trigger_price.as_inner() || execution > stop))
        || (!long && (stop > trigger.trigger_price.as_inner() || execution < stop))
        || u128::from(stop.abs_diff(execution)) * 100 > u128::from(stop) {
        return Err("replacement_geometry_invalid");
    }
    let params = CancelConditionalOrderParams::builder().trader_account(trader).position_authority(delegate)
        .orderbook(Pubkey::from_str(&market.market_pubkey).map_err(|_| "market_invalid")?)
        .conditional_order_index(plan.old_stop.0).disable_first(true).disable_second(true)
        .build().map_err(|_| "replacement_build_failed")?;
    let mut ixs = vec![create_cancel_conditional_order_ix(params).map_err(|_| "replacement_build_failed")?.into()];
    ixs.extend(builder.build_bracket_leg_orders(delegate, trader, &plan.symbol,
        if long { Side::Bid } else { Side::Ask }, &BracketLegOrders {
            stop_loss: Some(BracketLeg::new(plan.trigger_price).with_ioc_order().with_execution_price(plan.execution_price)
                .with_size(BracketLegSize::PositionPercent(100))), take_profit: None,
        }).map_err(|_| "replacement_build_failed")?);
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
    let last = book.header.trade_list.iter().max_by_key(|t| t.trade_sequence_number).filter(|t|
        t.slot > 0 && t.timestamp > 0 && t.trade_sequence_number.checked_add(1) == Some(book.header.trade_sequence_number.sequence_number)
        && t.slot == book.header.trade_sequence_number.last_update_slot && t.maker_fill.price > 0)
        .map(|t| (calc.ticks_to_price(t.maker_fill.price.into()), t.slot, t.timestamp))
        .filter(|(price, _, _)| price.is_finite() && *price > 0.0);
    Ok(json!({
        "symbol": symbol, "asset_id": market.asset_id, "market_status": market.market_status,
        "isolated_only": market.isolated_only, "base_lots_decimals": market.base_lots_decimals,
        "quote_lot_decimals": calc.quote_lot_decimals,
        "margin_admission": "native_simulation_required",
        "tick_size": market.tick_size, "leverage_tiers": market.leverage_tiers, "taker_fee": market.taker_fee,
        "default_taker_fee_micro": book.header.default_taker_fee_micro,
        "price_source": "phoenix_orderbook_last_trade",
        "last_trade_price": last.map(|t| t.0), "last_trade_slot": last.map(|t| t.1),
        "last_trade_timestamp": last.map(|t| t.2),
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

// Status readback must remain tied to the original plan, not just the market.
// The detached preparation receipt is not a submitted transaction.
pub fn preparation_context(request: &Request, prepared: &Value) -> Result<Vec<u8>, &'static str> {
    // Serialize typed plans directly: Value's number range excludes full u128 IDs.
    #[derive(Serialize)]
    struct Context<'a> {
        domain: &'static str,
        wallet: &'a str,
        genesis: &'a str,
        state: &'a Option<String>,
        entry: &'a Option<EntryPlan>,
        close: &'a Option<ClosePlan>,
        #[serde(skip_serializing_if = "Option::is_none")]
        replace_stop: &'a Option<ReplaceStopPlan>,
        prepared: Value,
    }
    let mut fields = prepared.clone();
    fields.as_object_mut().ok_or("prepared_transaction_invalid")?.remove("binding_signature");
    serde_json::to_vec(&Context {domain:"upscalp-phoenix-prepared-v1", wallet:&request.wallet_address,
        genesis:&request.genesis_hash, state:&request.expected_state_sha256,
        entry:&request.entry, close:&request.close, replace_stop:&request.replace_stop, prepared:fields})
        .map_err(|_| "prepared_context_invalid")
}

pub fn validate_prepared(request: &Request, delegate: Pubkey) -> Result<Transaction, &'static str> {
    request.operation_symbol()?;
    let prepared = request.prepared.as_ref().ok_or("prepared_required")?;
    let bytes = STANDARD.decode(prepared["transaction_base64"].as_str().ok_or("prepared_transaction_invalid")?)
        .map_err(|_| "prepared_transaction_invalid")?;
    if bytes.is_empty() || bytes.len() > 1232 { return Err("prepared_transaction_invalid"); }
    let tx: Transaction = bincode::deserialize(&bytes).map_err(|_| "prepared_transaction_invalid")?;
    // Only status reads use the historical signer recorded in the signed bytes.
    // Current account authority is still checked separately; submission never falls back.
    let delegate = if request.operation == "read_submission" {
        let original = *tx.message.account_keys.first().ok_or("prepared_transaction_invalid")?;
        let owner = Pubkey::from_str(&request.wallet_address).map_err(|_| "wallet_address_invalid")?;
        if original == Pubkey::default() || original == owner { return Err("dedicated_delegate_required"); }
        original
    } else { delegate };
    let canonical = prepared_transaction(&tx, delegate)?;
    if canonical.as_object().unwrap().iter().any(|(key, value)| prepared.get(key) != Some(value))
        || prepared["blockhash"].as_str() != Some(tx.message.recent_blockhash.to_string().as_str()) {
        return Err("prepared_transaction_invalid");
    }
    // Infer the installed signature type from the verified transaction; no new dependency.
    let signature = prepared["binding_signature"].as_str().ok_or("prepared_context_invalid")?
        .parse().map_err(|_| "prepared_context_invalid")?;
    let signatures = [tx.signatures[0], signature];
    if !signatures[1].verify(delegate.as_ref(), &preparation_context(request, prepared)?) {
        return Err("prepared_context_invalid");
    }
    if request.replace_stop.is_some() {
        let before = prepared.get("replace_before").ok_or("prepared_context_invalid")?;
        let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(before).map_err(|_| "prepared_context_invalid")?));
        if request.expected_state_sha256.as_deref() != Some(hash.as_str()) { return Err("prepared_context_invalid"); }
    }
    Ok(tx)
}

pub fn project_submission(signature: &str, observed: Option<(bool, bool, u64)>,
    snapshot_slot: u64, block_heights: Option<(u64, u64)>) -> Result<Value, &'static str> {
    let (finalized, failed, transaction_slot) = observed.unwrap_or((false, false, 0));
    if finalized && (transaction_slot == 0 || snapshot_slot < transaction_slot) {
        return Err("post_transaction_snapshot_stale");
    }
    // Heights are finalized current height and the signed preparation's limit.
    // A known (even nonfinalized) signature cannot be classified as absent.
    if let Some((height, limit)) = block_heights {
        if height == 0 || limit == 0 { return Err("block_height_invalid"); }
        if observed.is_none() && height > limit {
            return Ok(json!({"signature":signature, "submission_state":"expired",
                "confirmation_slot":null, "send_attempted":false,
                "expiry":{"finalized_block_height":height, "last_valid_block_height":limit,
                    "history_status_absent":true}}));
        }
    }
    Ok(json!({"signature":signature,
        "submission_state":if finalized { if failed { "failed" } else { "finalized" } } else { "pending" },
        "confirmation_slot":if finalized { Some(transaction_slot) } else { None },
        "send_attempted":false}))
}

// Decode executed events only after the RPC transaction bytes and slot bind to
// the original receipt. Missing/truncated events remain explicitly unavailable.
pub fn project_execution(metadata: &PhoenixMetadata, owner: Pubkey, delegate: Pubkey,
    plan: &EntryPlan, tx: &Transaction, transaction_slot: u64, record: &Value) -> Result<Value, &'static str> {
    use phoenix_rise::core::events::{RpcInstructionView, RpcInnerInstructionView, RpcInnerInstructionGroup,
        parse_events_from_rpc_instructions_with_errors};
    use phoenix_rise::events::MarketEvent;
    let fail = "execution_evidence_unavailable";
    let bytes = STANDARD.decode(record["transaction"][0].as_str().ok_or(fail)?).map_err(|_| fail)?;
    if record["transaction"][1] != "base64" || bincode::serialize(tx).map_err(|_| fail)? != bytes
        || record["slot"].as_u64() != Some(transaction_slot) || transaction_slot == 0
        || record["meta"].get("err") != Some(&Value::Null) || tx.verify().is_err() {
        return Err(fail);
    }
    let fee = record["meta"]["fee"].as_u64().ok_or(fail)?;
    let account_key = |index: u64| tx.message.account_keys.get(usize::try_from(index).map_err(|_| fail)?).ok_or(fail);
    let outer = tx.message.instructions.iter().map(|ix| Ok(RpcInstructionView {
        program_id: account_key(u64::from(ix.program_id_index))?.as_ref(), data:&ix.data,
    })).collect::<Result<Vec<_>, &'static str>>()?;
    let groups = record["meta"]["innerInstructions"].as_array().ok_or(fail)?;
    let mut decoded = Vec::new();
    let mut indices = Vec::new();
    for group in groups {
        let index = u8::try_from(group["index"].as_u64().ok_or(fail)?).map_err(|_| fail)?;
        if usize::from(index) >= outer.len() || indices.contains(&index) { return Err(fail); }
        indices.push(index);
        let mut instructions = Vec::new();
        for ix in group["instructions"].as_array().ok_or(fail)? {
            let program = account_key(ix["programIdIndex"].as_u64().ok_or(fail)?)?;
            // Reuse the installed RPC client's base58 byte decoder; no new crate.
            let encoded = solana_rpc_client::api::filter::Memcmp::new(0,
                solana_rpc_client::api::filter::MemcmpEncodedBytes::Base58(ix["data"].as_str().ok_or(fail)?.to_owned()));
            let data = encoded.bytes().ok_or(fail)?.into_owned();
            let height = u32::try_from(ix["stackHeight"].as_u64().ok_or(fail)?).map_err(|_| fail)?;
            if height < 2 { return Err(fail); }
            instructions.push((program, data, height));
        }
        decoded.push(instructions);
    }
    let views: Vec<Vec<_>> = decoded.iter().map(|g| g.iter().map(|(program, data, height)|
        RpcInnerInstructionView {program_id:program.as_ref(), data, stack_height:Some(*height)}).collect()).collect();
    let groups: Vec<_> = indices.iter().zip(&views).map(|(index, instructions)|
        RpcInnerInstructionGroup {index:*index, instructions}).collect();
    let parsed = parse_events_from_rpc_instructions_with_errors(&outer, &groups, PHOENIX_PROGRAM_ID.as_ref());
    if !parsed.orphan_logs.is_empty() || parsed.instructions.iter().any(|i| !i.failures.is_empty() || !i.skipped_event_bytes.is_empty()) {
        return Err(fail);
    }
    let market = metadata.get_market(&plan.symbol).ok_or(fail)?;
    let calc = metadata.get_market_calculator(&plan.symbol).ok_or(fail)?;
    let trader = TraderKey::new(owner).pda();
    let mut summaries = Vec::new();
    for instruction in &parsed.instructions {
        let packet_matches = instruction.events.iter().any(|event| match event {
            MarketEvent::OrderPacket(e) if e.trader == trader.to_bytes() => match e.order_packet.kind() {
                phoenix_rise::events::market_events::OrderPacketKind::ImmediateOrCancel {client_order_id, ..} =>
                    u128::from_le_bytes(*client_order_id) == plan.client_order_id,
                _ => false,
            }, _ => false,
        });
        if !packet_matches { continue; }
        let headers: Vec<_> = instruction.events.iter().filter_map(|e| if let MarketEvent::Header(h) = e {Some(h)} else {None}).collect();
        if headers.len() != 1 || headers[0].trader_account != trader.to_bytes() || headers[0].signer != delegate.to_bytes()
            || headers[0].asset_id != market.asset_id || headers[0].base_lot_decimals != market.base_lots_decimals
            || headers[0].quote_lot_decimals != calc.quote_lot_decimals
            || u64::from(headers[0].tick_size) != market.tick_size { return Err(fail); }
        for event in &instruction.events {
            if let MarketEvent::TradeSummary(e) = event {
                if e.trader != trader.to_bytes() { return Err(fail); }
                summaries.push(e);
            }
        }
    }
    if summaries.len() != 1 { return Err(fail); }
    let summary = summaries[0];
    let base = summary.base_lots_filled.as_inner();
    let quote = summary.quote_lots_filled.as_inner();
    let signed = summary.base_lot_position.as_inner();
    if base == 0 || base != plan.base_lots || signed.unsigned_abs() != base
        || !matches!(plan.direction.as_str(), "LONG" | "SHORT") || (signed > 0) != (plan.direction == "LONG")
        || (summary.side == phoenix_rise::events::market_events::Side::Bid) != (plan.direction == "LONG")
        || quote == 0 || quote > plan.max_quote_lots { return Err(fail); }
    let quote_scale = 10_f64.powi(i32::from(calc.quote_lot_decimals));
    let average = (quote as f64 / quote_scale) / (base as f64 / 10_f64.powi(i32::from(market.base_lots_decimals)));
    let fee_quote = summary.fee_in_quote_lots.as_inner() as f64 / quote_scale;
    if !average.is_finite() || average <= 0.0 || !fee_quote.is_finite() || fee_quote < 0.0 { return Err(fail); }
    Ok(json!({"status":"verified", "source":"phoenix_finalized_transaction_events", "signature":tx.signatures[0].to_string(),
        "slot":transaction_slot, "client_order_id":plan.client_order_id.to_string(), "signed_base_lots":signed,
        "base_lots_filled":base, "quote_lots_filled":quote, "fee_quote_lots":summary.fee_in_quote_lots.as_inner(),
        "average_price":average, "fee_quote":fee_quote, "fee_lamports":fee}))
}

pub fn project_replacement(metadata: &PhoenixMetadata, plan: &ReplaceStopPlan,
    before: &Value, after: &Value) -> Result<Value, &'static str> {
    let fail = "replacement_unverified";
    if before["symbol"] != plan.symbol || after["symbol"] != plan.symbol
        || before["signed_base_lots"].as_i64() != Some(plan.signed_base_lots)
        || after["signed_base_lots"] != before["signed_base_lots"]
        || before["position_sequence"].as_u64() != Some(u64::from(plan.position_sequence))
        || after["position_sequence"] != before["position_sequence"] { return Err(fail); }
    let old = before["conditionals"].as_array().ok_or(fail)?;
    let current = after["conditionals"].as_array().ok_or(fail)?;
    let identity = |v: &Value| -> Option<(u8, u64)> { Some((u8::try_from(v["index"].as_u64()?).ok()?, v["order"]["sequence_number"].as_u64()?)) };
    if old.iter().map(identity).collect::<Option<Vec<_>>>().ok_or(fail)? != plan.conditionals
        || old.len() != current.len() || current.iter().any(|o| identity(o) == Some(plan.old_stop)) { return Err(fail); }
    let preserved: Vec<_> = old.iter().filter(|o| identity(o) != Some(plan.old_stop)).collect();
    if preserved.len() + 1 != old.len() || preserved.iter().any(|o| current.iter().filter(|v| *v == *o).count() != 1) { return Err(fail); }
    let added: Vec<_> = current.iter().filter(|v| !preserved.contains(v)).collect();
    if added.len() != 1 { return Err(fail); }
    let new = added[0];
    let order = &new["order"];
    let long = plan.signed_base_lots > 0;
    let (trigger, opposite) = if long {(&order["less_trigger_order"], &order["greater_trigger_order"])}
        else {(&order["greater_trigger_order"], &order["less_trigger_order"])};
    let calc = metadata.get_market_calculator(&plan.symbol).ok_or(fail)?;
    let stop = calc.price_to_ticks(plan.trigger_price).map_err(|_| fail)?.as_inner();
    let execution = calc.price_to_ticks(plan.execution_price).map_err(|_| fail)?.as_inner();
    if order["is_active"] != true || order["use_percent"] != true || order["percent"] != 100
        || trigger["is_active"] != true || opposite["is_active"] != false
        || order["asset_id"] != metadata.get_market(&plan.symbol).ok_or(fail)?.asset_id
        || trigger["position_sequence_number"] != plan.position_sequence || trigger["order_kind"] != json!(StopLossOrderKind::IOC)
        || trigger["trade_side"] != json!(if long {StopLossTradeSide::Ask} else {StopLossTradeSide::Bid})
        || trigger["execution_direction"] != json!(if long {StopLossDirection::LessThan} else {StopLossDirection::GreaterThan})
        || trigger["trigger_price"] != stop.to_string() || trigger["execution_price"] != execution.to_string() { return Err(fail); }
    Ok(json!({"status":"verified", "old_stop":plan.old_stop, "new_stop":identity(new).ok_or(fail)?,
        "trigger_ticks":stop, "execution_ticks":execution, "remaining_base_lots":plan.signed_base_lots.unsigned_abs()}))
}

// The caller persists the signature before invocation and reconciles uncertainty.
pub fn dispatch_prepared_once(tx: &Transaction,
    send: impl FnOnce(&Transaction) -> Result<String, ()>) -> Result<Value, &'static str> {
    let signature = tx.signatures.first().ok_or("prepared_transaction_invalid")?.to_string();
    let response = send(tx);
    if response.as_ref().is_ok_and(|returned| returned != &signature) {
        return Err("submission_signature_mismatch");
    }
    // A transport error may follow successful submission; it is not failure proof.
    Ok(json!({"signature":signature,"submission_state":"pending","confirmation_slot":null,
        "send_attempted":true,"transport_uncertain":response.is_err()}))
}

fn run(request: Request) -> Result<Value, &'static str> {
    let symbol = request.operation_symbol()?;
    if !valid_https_endpoint(&request.rpc_url) || request.genesis_hash.is_empty() {
        return Err("rpc_identity_required");
    }
    let (owner, signer) = parse_identity(&request.wallet_address, &request.private_key)?;
    let supplied_tx = if request.prepared.is_some() { Some(validate_prepared(&request, signer.pubkey())?) } else { None };
    let rpc = RpcClient::new_with_timeout_and_commitment(request.rpc_url.clone(), Duration::from_secs(15), CommitmentConfig::finalized());
    let genesis = rpc.get_genesis_hash().map_err(|_| "rpc_unavailable")?;
    if genesis.to_string() != request.genesis_hash {
        return Err("rpc_genesis_mismatch");
    }
    // Read height first, then history: an earlier absence must not be combined
    // with a later expiry while a transaction could land between those reads.
    let block_heights = if supplied_tx.is_some() {
        let limit = request.prepared.as_ref().unwrap()["last_valid_block_height"].as_u64()
            .filter(|limit| *limit > 0).ok_or("prepared_block_height_invalid")?;
        Some((rpc.get_block_height_with_commitment(CommitmentConfig::finalized())
            .map_err(|_| "block_height_unavailable")?, limit))
    } else { None };
    // Read the precise signature with history before the finalized account snapshot.
    let observed = if let Some(tx) = &supplied_tx {
        let statuses = rpc.get_signature_statuses_with_history(&[tx.signatures[0]]).map_err(|_| "signature_status_unavailable")?;
        if statuses.value.len() != 1 { return Err("signature_status_invalid"); }
        statuses.value.into_iter().next().flatten()
    } else { None };
    let address = TraderKey::new(owner).pda();
    let response = rpc.get_account_with_commitment(&address, CommitmentConfig::finalized()).map_err(|_| "rpc_unavailable")?;
    let account = response.value.ok_or("trader_account_absent")?;
    let mut result = validate_account(owner, signer.pubkey(), address, &FetchedAccount::new(account.data, account.owner, account.executable))?;
    result["finalized_slot"] = json!(response.context.slot);
    result["genesis_hash"] = json!(genesis.to_string());
    let Some(symbol) = symbol else { return Ok(result) };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| "native_runtime_unavailable")?;
    let http = PhoenixHttpClient::new_public(request.api_url.clone().ok_or("metadata_endpoint_required")?)
        .map_err(|_| "metadata_unavailable")?;
    let exchange = runtime.block_on(async { tokio::time::timeout(Duration::from_secs(15), http.get_exchange()).await })
        .map_err(|_| "metadata_unavailable")?.map_err(|_| "metadata_unavailable")?;
    let metadata = PhoenixMetadata::new(exchange.into());
    let market = metadata.get_market(symbol).ok_or("market_unknown")?;
    let book = Pubkey::from_str(&market.market_pubkey).map_err(|_| "market_identity_invalid")?;
    let conditional = phoenix_rise::ix::prelude::get_conditional_orders_address(&address).map_err(|_| "conditional_address_invalid")?;
    let snapshot = rpc.get_multiple_accounts_with_commitment(&[address, book, conditional], CommitmentConfig::finalized())
        .map_err(|_| "rpc_unavailable")?;
    if snapshot.context.slot == 0 || snapshot.value.len() != 3 { return Err("snapshot_invalid"); }
    let accounts: Vec<_> = snapshot.value.into_iter().map(|a| a.map(|a| FetchedAccount::new(a.data, a.owner, a.executable))).collect();
    let trader_account = accounts[0].as_ref().ok_or("trader_account_absent")?;
    let orderbook_account = accounts[1].as_ref().ok_or("orderbook_account_absent")?;
    let conditional_account = accounts[2].as_ref();
    let state = project_state(&metadata, owner, signer.pubkey(), symbol, trader_account, conditional_account, orderbook_account)?;
    let state_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&state).map_err(|_| "snapshot_invalid")?));
    result["operation"] = json!(request.operation);
    result["finalized_slot"] = json!(snapshot.context.slot);
    result["state"] = state;
    result["state_sha256"] = json!(state_hash);
    if request.operation == "read_state" { return Ok(result); }
    if let Some(tx) = &supplied_tx {
        if !tx.message.account_keys.contains(&address) || !tx.message.account_keys.contains(&book) {
            return Err("prepared_market_binding_invalid");
        }
        let status = project_submission(&tx.signatures[0].to_string(),
            observed.as_ref().map(|s| (s.satisfies_commitment(CommitmentConfig::finalized()),
                s.err.is_some(), s.slot)), snapshot.context.slot, block_heights)?;
        result.as_object_mut().unwrap().extend(status.as_object().unwrap().clone());
        result["transaction_sha256"] = request.prepared.as_ref().unwrap()["transaction_sha256"].clone();
        if let Some(plan) = &request.entry {
            result["execution"] = json!({"status":"unavailable", "reason":"transaction_not_finalized"});
            if result["submission_state"] == "finalized" {
                let record = rpc.send::<Value>(solana_rpc_client::api::request::RpcRequest::GetTransaction,
                    json!([tx.signatures[0].to_string(), {"encoding":"base64", "commitment":"finalized", "maxSupportedTransactionVersion":0}]));
                result["execution"] = record.map_err(|_| "execution_evidence_unavailable").and_then(|record|
                    project_execution(&metadata, owner, tx.message.account_keys[0], plan, tx, observed.as_ref().unwrap().slot, &record))
                    .unwrap_or_else(|reason| json!({"status":"unavailable", "reason":reason}));
            }
        }
        if let Some(plan) = &request.replace_stop {
            result["replacement"] = json!({"status":"unverified"});
            if result["submission_state"] == "finalized" {
                result["replacement"] = project_replacement(&metadata, plan,
                    &request.prepared.as_ref().unwrap()["replace_before"], &result["state"])
                    .unwrap_or_else(|_| json!({"status":"unverified"}));
            }
        }
        // Known signatures are never sent again, regardless of confirmation level.
        if observed.is_some() || request.operation == "read_submission" || result["submission_state"] == "expired" { return Ok(result); }
    }
    if request.expected_state_sha256.as_deref() != Some(state_hash.as_str()) { return Err("state_changed"); }
    let ixs = if request.entry.is_some() {
        runtime.block_on(build_protected_entry(&metadata, owner, signer.pubkey(), trader_account,
            conditional_account, snapshot.context.slot, request.entry.as_ref().ok_or("entry_required")?))?
    } else if let Some(plan) = &request.replace_stop {
        build_replace_stop(&metadata, owner, signer.pubkey(), trader_account,
            conditional_account.ok_or("conditional_account_absent")?, snapshot.context.slot, plan)?
    } else {
        runtime.block_on(build_protected_close(&metadata, owner, signer.pubkey(), trader_account,
            conditional_account.ok_or("conditional_account_absent")?, snapshot.context.slot, request.close.as_ref().ok_or("close_required")?))?
    };
    if let Some(tx) = supplied_tx {
        let mut expected = Transaction::new_with_payer(&ixs, Some(&signer.pubkey()));
        expected.message.recent_blockhash = tx.message.recent_blockhash;
        if expected.message != tx.message { return Err("prepared_instructions_changed"); }
        if !rpc.is_blockhash_valid(&tx.message.recent_blockhash, CommitmentConfig::finalized()).map_err(|_| "blockhash_unavailable")? {
            return Err("prepared_blockhash_expired");
        }
        let simulation = rpc.simulate_transaction_with_config(&tx, RpcSimulateTransactionConfig {
            sig_verify:true, commitment:Some(CommitmentConfig::finalized()),
            min_context_slot:Some(snapshot.context.slot), ..Default::default()
        }).map_err(|_| "simulation_unavailable")?;
        if simulation.value.err.is_some() { return Err("simulation_refused"); }
        let outcome = dispatch_prepared_once(&tx, |original| rpc.send_transaction_with_config(original, RpcSendTransactionConfig {
            skip_preflight:false, preflight_commitment:Some(CommitmentConfig::finalized().commitment),
            max_retries:Some(0), min_context_slot:Some(snapshot.context.slot), ..Default::default()
        }).map(|s| s.to_string()).map_err(|_| ()))?;
        result.as_object_mut().unwrap().extend(outcome.as_object().unwrap().clone());
        return Ok(result);
    }
    let (blockhash, last_valid_block_height) = rpc.get_latest_blockhash_with_commitment(CommitmentConfig::finalized())
        .map_err(|_| "rpc_unavailable")?;
    let mut tx = Transaction::new_with_payer(&ixs, Some(&signer.pubkey()));
    tx.try_sign(&[&signer], blockhash).map_err(|_| "transaction_signing_failed")?;
    let mut prepared = prepared_transaction(&tx, signer.pubkey())?;
    let fee = rpc.get_fee_for_message(&tx.message).map_err(|_| "fee_unavailable")?;
    let balance = rpc.get_balance_with_commitment(&signer.pubkey(), CommitmentConfig::finalized()).map_err(|_| "balance_unavailable")?;
    if balance.value < fee { return Err("fee_balance_insufficient"); }
    let simulation = rpc.simulate_transaction_with_config(&tx, RpcSimulateTransactionConfig {
        sig_verify:true, commitment:Some(CommitmentConfig::finalized()),
        min_context_slot:Some(snapshot.context.slot), ..Default::default()
    }).map_err(|_| "simulation_unavailable")?;
    if simulation.value.err.is_some() { return Err("simulation_refused"); }
    prepared["fee_lamports"] = json!(fee);
    prepared["payer_balance_lamports"] = json!(balance.value);
    prepared["simulation_slot"] = json!(simulation.context.slot);
    prepared["last_valid_block_height"] = json!(last_valid_block_height);
    prepared["blockhash"] = json!(blockhash.to_string());
    if request.replace_stop.is_some() { prepared["replace_before"] = result["state"].clone(); }
    prepared["binding_signature"] = json!(signer.sign_message(&preparation_context(&request, &prepared)?).to_string());
    result["prepared"] = prepared;
    // Preparation only. Submission belongs to the separately gated account flow.
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
