// PO-local route, pinned SDK0.6.6. Never install in UpScalp's service.
// PO approved this code only; agent execution is throwaway-key simulation only.
use std::{fs::{self, OpenOptions}, io::{Read, Write}, path::Path, str::FromStr, time::Duration};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, MetadataExt};
use phoenix_rise::{accounts::owned::Trader, core::TraderKey,
    ix::{PHOENIX_PROGRAM_ID, prelude::{DelegateTraderParams, create_delegate_trader_ix}}};
use serde_json::{json, Value};
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::{rpc_client::RpcClient, api::config::{RpcSimulateTransactionConfig, RpcSendTransactionConfig}};
use solana_signer::Signer;
use solana_transaction::Transaction;

const RPC: &str = "https://api.mainnet-beta.solana.com";
const GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
type Result<T> = std::result::Result<T, &'static str>;
const USAGE: &str = "new-trading-key PRIVATE_FILE | probe-unfunded | status OWNER | set|revoke OWNER_FILE OWNER DELEGATE [--send]";

fn public(value: &str) -> Result<Pubkey> {
    let key = Pubkey::from_str(value).map_err(|_| "invalid_public_key")?;
    if key == Pubkey::default() { return Err("invalid_public_key"); }
    Ok(key)
}

fn decode_key(text: &str, expected: Pubkey) -> Result<Keypair> {
    let key = if text.trim_start().starts_with('[') {
        let bytes: Vec<u8> = serde_json::from_str(text).map_err(|_| "owner_key_invalid")?;
        Keypair::try_from(bytes.as_slice()).map_err(|_| "owner_key_invalid")?
    } else {
        Keypair::try_from_base58_string(text.trim()).map_err(|_| "owner_key_invalid")?
    };
    if key.pubkey() != expected { return Err("owner_key_mismatch"); }
    Ok(key)
}

fn local_owner(path: &Path, expected: Pubkey) -> Result<Keypair> {
    let before = fs::symlink_metadata(path).map_err(|_| "owner_file_unavailable")?;
    if !before.is_file() || before.permissions().mode() & 0o077 != 0 || before.len() > 1024 {
        return Err("owner_file_requires_private_regular_file");
    }
    let file = fs::File::open(path).map_err(|_| "owner_file_unavailable")?;
    let after = file.metadata().map_err(|_| "owner_file_unavailable")?;
    if before.ino() != after.ino() || before.dev() != after.dev() { return Err("owner_file_changed"); }
    let mut text = String::new();
    file.take(1025).read_to_string(&mut text).map_err(|_| "owner_file_invalid")?;
    if text.len() > 1024 { return Err("owner_file_invalid"); }
    decode_key(&text, expected)
}

fn new_key(path: &Path) -> Result<Value> {
    let parent = path.parent().ok_or("private_directory_required")?
        .canonicalize().map_err(|_| "private_directory_required")?;
    if parent.ancestors().any(|p| {
        let git = p.join(".git");
        git.is_file() || git.join("HEAD").is_file()
    })
        || parent.metadata().map_err(|_| "private_directory_required")?.permissions().mode() & 0o077 != 0 {
        return Err("private_non_repository_directory_required");
    }
    let key = Keypair::new();
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600)
        .open(path).map_err(|_| "new_key_file_must_be_absent")?;
    file.write_all(key.to_base58_string().as_bytes()).map_err(|_| "key_write_failed_do_not_regenerate")?;
    file.sync_all().map_err(|_| "key_write_failed_do_not_regenerate")?;
    Ok(json!({"public_key":key.pubkey().to_string(),"secret_written_locally":true,"sent":false}))
}

fn rpc() -> Result<RpcClient> {
    let rpc = RpcClient::new_with_timeout_and_commitment(RPC.to_string(), Duration::from_secs(20), CommitmentConfig::finalized());
    if rpc.get_genesis_hash().map_err(|_| "rpc_unavailable")?.to_string() != GENESIS { return Err("mainnet_genesis_mismatch"); }
    Ok(rpc)
}

fn authority(rpc: &RpcClient, owner: Pubkey) -> Result<(Pubkey, u64)> {
    let address = TraderKey::new(owner).pda();
    let response = rpc.get_account_with_commitment(&address, CommitmentConfig::finalized())
        .map_err(|_| "trader_read_unavailable")?;
    let account = response.value.ok_or("primary_trader_absent")?;
    if account.owner != *PHOENIX_PROGRAM_ID || account.executable { return Err("trader_program_mismatch"); }
    let trader = Trader::try_from_account_bytes(&account.data).map_err(|_| "trader_invalid")?;
    if trader.authority != owner || trader.key != address || trader.trader_pda_index != 0 || trader.trader_subaccount_index != 0 {
        return Err("primary_trader_binding_mismatch");
    }
    Ok((trader.position_authority, response.context.slot))
}

fn instruction(owner: Pubkey, delegate: Pubkey, revoke: bool) -> Result<Instruction> {
    if owner == delegate || owner == Pubkey::default() || delegate == Pubkey::default() {
        return Err("distinct_owner_delegate_required");
    }
    let params = DelegateTraderParams::builder().trader_wallet(owner)
        .trader_account(TraderKey::new(owner).pda())
        .new_position_authority(if revoke { owner } else { delegate })
        .build().map_err(|_| "instruction_invalid")?;
    Ok(create_delegate_trader_ix(params).map_err(|_| "instruction_invalid")?.into())
}

fn probe_unfunded() -> Result<Value> {
    let rpc = rpc()?;
    // Disposable in-memory signers only; neither public key has been funded or
    // registered. AccountNotFound is a refusal, never a delegation success.
    let owner = Keypair::new();
    let delegate = Keypair::new().pubkey();
    let mut results = Vec::new();
    for revoke in [false, true] {
        let ix = instruction(owner.pubkey(), delegate, revoke)?;
        let (blockhash, expiry) = rpc.get_latest_blockhash_with_commitment(CommitmentConfig::finalized())
            .map_err(|_| "blockhash_unavailable")?;
        let tx = Transaction::new_signed_with_payer(&[ix], Some(&owner.pubkey()), &[&owner], blockhash);
        // A public fee-quote failure must not be mistaken for a simulation.
        // This disposable-key diagnostic records it; the PO send path below
        // still refuses unless its own exact fee quote succeeds.
        let fee = rpc.get_fee_for_message(&tx.message);
        let simulation = rpc.simulate_transaction_with_config(&tx, RpcSimulateTransactionConfig {
            sig_verify:true, commitment:Some(CommitmentConfig::finalized()), ..Default::default()
        }).map_err(|_| "simulation_unavailable")?;
        results.push(json!({"operation":if revoke {"revoke"} else {"set"},
            "fee_lamports":fee.as_ref().ok(),"fee_quote_error":fee.err().map(|e| e.to_string()),
            "simulation_slot":simulation.context.slot,
            "simulation_error":simulation.value.err,"units_consumed":simulation.value.units_consumed,
            "last_valid_block_height":expiry,"sent":false}));
    }
    Ok(json!({"rpc":RPC,"genesis":GENESIS,"program":PHOENIX_PROGRAM_ID.to_string(),
        "scope":"throwaway_unfunded_simulation_only","transactions":results,
        "delegation_proven":false,"send_attempted":false}))
}

fn run(args: &[String]) -> Result<Value> {
    match args {
        [cmd, path] if cmd == "new-trading-key" => new_key(Path::new(path)),
        [cmd] if cmd == "probe-unfunded" => probe_unfunded(),
        [cmd, owner] if cmd == "status" => {
            let owner = public(owner)?;
            let (current, slot) = authority(&rpc()?, owner)?;
            Ok(json!({"owner":owner.to_string(),"position_authority":current.to_string(),"finalized_slot":slot}))
        }
        _ => {
            if !(args.len() == 4 || (args.len() == 5 && args[4] == "--send"))
                || !matches!(args[0].as_str(), "set" | "revoke") { return Err(USAGE); }
            let owner = public(&args[2])?;
            let delegate = public(&args[3])?;
            let revoke = args[0] == "revoke";
            let ix = instruction(owner, delegate, revoke)?;
            let signer = local_owner(Path::new(&args[1]), owner)?;
            let rpc = rpc()?;
            let (current, slot) = authority(&rpc, owner)?;
            if current != if revoke {delegate} else {owner} { return Err("current_authority_mismatch_no_overwrite"); }
            let (blockhash, expiry) = rpc.get_latest_blockhash_with_commitment(CommitmentConfig::finalized())
                .map_err(|_| "blockhash_unavailable")?;
            let tx = Transaction::new_signed_with_payer(&[ix], Some(&owner), &[&signer], blockhash);
            let fee = rpc.get_fee_for_message(&tx.message).map_err(|_| "fee_unavailable")?;
            let simulation = rpc.simulate_transaction_with_config(&tx, RpcSimulateTransactionConfig {
                sig_verify:true, commitment:Some(CommitmentConfig::finalized()), min_context_slot:Some(slot), ..Default::default()
            }).map_err(|_| "simulation_unavailable")?;
            let mut result = json!({"operation":args[0],"rpc":RPC,"genesis":GENESIS,
                "program":PHOENIX_PROGRAM_ID.to_string(),"owner":owner.to_string(),
                "trader":TraderKey::new(owner).pda().to_string(),"previous_authority":current.to_string(),
                "new_position_authority":if revoke {owner} else {delegate}.to_string(),
                "fee_lamports":fee,"simulation_slot":simulation.context.slot,"simulation_error":simulation.value.err,
                "last_valid_block_height":expiry,"signature":tx.signatures[0].to_string(),"send_attempted":false});
            if !result["simulation_error"].is_null() || args.len() == 4 { return Ok(result); }
            let balance = rpc.get_balance_with_commitment(&owner, CommitmentConfig::finalized())
                .map_err(|_| "balance_unavailable")?.value;
            if balance < fee.checked_mul(if revoke {1} else {2}).ok_or("fee_invalid")? {
                return Err("fee_balance_or_revoke_reserve_insufficient");
            }
            if authority(&rpc, owner)?.0 != current { return Err("authority_changed_no_send"); }
            // Display and flush the decoded review and original signature before
            // the sole explicit send. Uncertainty is never automatic re-sign/resend.
            println!("{result}");
            std::io::stdout().flush().map_err(|_| "receipt_output_failed_no_send")?;
            let sent = rpc.send_transaction_with_config(&tx, RpcSendTransactionConfig {
                skip_preflight:false, preflight_commitment:Some(CommitmentConfig::finalized().commitment),
                max_retries:Some(0), min_context_slot:Some(slot), ..Default::default()
            });
            result["send_attempted"] = json!(true);
            result["rpc_accepted"] = json!(sent.as_ref().is_ok_and(|s| *s == tx.signatures[0]));
            result["transport_uncertain"] = json!(sent.is_err());
            result["status"] = json!("pending_inspect_original_signature_do_not_resend");
            Ok(result)
        }
    }
}

fn main() {
    match run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(value) => { println!("{value}"); if value.get("simulation_error").is_some_and(|e| !e.is_null()) { std::process::exit(1); } }
        Err(code) => { println!("{}", json!({"status":"refused","code":code})); std::process::exit(1); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_signed_set_and_reset_have_no_implicit_send() {
        let key = Keypair::new();
        let owner = key.pubkey();
        let delegate = Keypair::new().pubkey();
        for revoke in [false, true] {
            let ix = instruction(owner, delegate, revoke).unwrap();
            assert_eq!(ix.program_id, *PHOENIX_PROGRAM_ID);
            assert_eq!(ix.accounts.iter().filter(|a| a.is_signer).count(), 1);
            assert_eq!(ix.accounts[3].pubkey, owner);
            assert_eq!(ix.accounts[4].pubkey, TraderKey::new(owner).pda());
            assert_eq!(ix.accounts[5].pubkey, if revoke {owner} else {delegate});
            assert!(!ix.accounts[5].is_signer);
        }
        assert!(instruction(owner, owner, false).is_err());
        assert!(instruction(Pubkey::default(), delegate, false).is_err());
        assert!(decode_key(&key.to_base58_string(), delegate).is_err());
        assert_eq!(decode_key(&key.to_base58_string(), owner).unwrap().pubkey(), owner);
        assert_eq!(decode_key(&serde_json::to_string(&key.to_bytes().as_slice()).unwrap(), owner).unwrap().pubkey(), owner);
        assert!(decode_key("[]", owner).is_err());
        for args in [vec![], vec!["send"], vec!["set", "wallet.json"], vec!["probe-unfunded", "--send"]] {
            assert!(run(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
        // Fresh synthetic keyfiles only. Never inspect an existing wallet.
        // Native Linux/WSL private storage, never a Windows-mounted repository
        // TMPDIR whose chmod may not provide owner-only permissions.
        let dir = Path::new("/tmp").join(format!("phoenix-delegate-check-{}", delegate));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("synthetic.base58");
        let output = new_key(&path).unwrap();
        let generated = public(output["public_key"].as_str().unwrap()).unwrap();
        assert_eq!(local_owner(&path, generated).unwrap().pubkey(), generated);
        assert!(new_key(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(local_owner(&path, generated).is_err());
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&dir).unwrap();
        println!("set/reset owner signer, key binding/private-file/no-overwrite and bad-send-argument controls PASS; synthetic only, no RPC");
    }
}
