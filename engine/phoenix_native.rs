// Built as an example of the pinned Phoenix Rust SDK 0.6.6. JSON on stdin only;
// never read a wallet file, environment secret, or default wallet.
use std::{io::{self, Read}, str::FromStr, time::Duration};

use phoenix_rise::{accounts::owned::Trader, core::{FetchedAccount, TraderKey}, ix::PHOENIX_PROGRAM_ID};
use serde::Deserialize;
use serde_json::{json, Value};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_signer::Signer;

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
