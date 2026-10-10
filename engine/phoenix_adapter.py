"""Phoenix native SDK boundary; no import-time connection or default wallet."""
from __future__ import annotations

import base64
import hashlib
import json
import math
from pathlib import Path
import re
import subprocess
from typing import Mapping, TypedDict
from urllib.parse import urlsplit


PHOENIX_PLATFORM_ID = "phoenix"
PHOENIX_PROGRAM_ID = "EtrnLzgbS7nMMy5fbD42kXiUzGg8XQzJ972Xtk1cjWih"


class PhoenixPreparedTransaction(TypedDict):
    transaction_base64: str
    transaction_sha256: str
    signature: str
    submitted: bool
    fee_lamports: int
    payer_balance_lamports: int
    simulation_slot: int
    last_valid_block_height: int
    blockhash: str
    binding_signature: str


class PhoenixNativeResult(TypedDict, total=False):
    status: str
    operation: str
    wallet_address: str
    delegate_address: str
    trader_account: str
    finalized_slot: int
    state_sha256: str
    state: dict[str, object]
    prepared: PhoenixPreparedTransaction
    signature: str
    transaction_sha256: str
    submission_state: str
    confirmation_slot: int | None
    send_attempted: bool
    transport_uncertain: bool
    execution: dict[str, object]
    replacement: dict[str, object]


def phoenix_account_operation(
    credentials: Mapping[str, str], *, executable: str, executable_sha256: str,
    rpc_url: str, genesis_hash: str, request: Mapping[str, object], api_url: str = "",
) -> PhoenixNativeResult:
    """Bound native account operation; activation is gated by the account caller."""
    operation = request.get("operation")
    keys = {"validate_connection": {"operation"}, "read_state": {"operation", "symbol"},
            "prepare_entry": {"operation", "entry", "expected_state_sha256"},
            "prepare_close": {"operation", "close", "expected_state_sha256"},
            "prepare_replace_stop": {"operation", "replace_stop", "expected_state_sha256"}}
    if operation in ("read_submission", "submit_prepared"):
        keys[operation] = {"operation", "prepared", "expected_state_sha256",
                           "entry" if "entry" in request else "close" if "close" in request else "replace_stop"}
    if not isinstance(operation, str) or operation not in keys or set(request) != keys[operation]:
        raise ValueError("Phoenix operation inputs are incomplete or unsupported")
    if operation != "validate_connection":
        endpoint = urlsplit(api_url)
        if endpoint.scheme != "https" or not endpoint.hostname or endpoint.username or endpoint.password or endpoint.fragment:
            raise ValueError("Phoenix metadata endpoint is incomplete or invalid")
    path = Path(executable)
    endpoint = urlsplit(rpc_url)
    if (
        not executable or not path.is_absolute() or path.is_symlink()
        or not re.fullmatch(r"[0-9a-f]{64}", executable_sha256)
        or endpoint.scheme != "https" or not endpoint.hostname or endpoint.fragment
        or endpoint.username or endpoint.password
        or not re.fullmatch(r"[1-9A-HJ-NP-Za-km-z]{32,44}", genesis_hash)
    ):
        raise ValueError("Phoenix native configuration is incomplete or invalid")
    try:
        with path.open("rb") as handle:
            digest = hashlib.sha256()
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        if digest.hexdigest() != executable_sha256:
            raise ValueError("Phoenix native executable hash mismatch")
    except OSError:
        raise RuntimeError("Phoenix native executable is unavailable") from None
    payload = {
        **request,
        "wallet_address": credentials.get("wallet_address", ""),
        "private_key": credentials.get("private_key", ""),
        "rpc_url": rpc_url,
        "genesis_hash": genesis_hash,
    }
    if operation != "validate_connection":
        payload["api_url"] = api_url
    try:
        completed = subprocess.run(
            [str(path)], input=json.dumps(payload, allow_nan=False), text=True, capture_output=True,
            timeout=120 if operation != "validate_connection" else 40, check=False,
        )
    except (OSError, subprocess.SubprocessError):
        # Timeout exceptions retain input: never forward their repr/message.
        raise RuntimeError("Phoenix native probe unavailable or timed out") from None
    try:
        result = json.loads(completed.stdout)
    except (TypeError, ValueError):
        raise RuntimeError("Phoenix native probe returned an invalid response") from None
    if completed.returncode != 0 or not isinstance(result, dict) or result.get("status") != "ok":
        # Do not expose process stderr, endpoint details, or arbitrary response text.
        raise RuntimeError("Phoenix account or delegation could not be verified")
    if (
        result.get("platform") != PHOENIX_PLATFORM_ID
        or result.get("wallet_address") != payload["wallet_address"]
        or result.get("genesis_hash") != genesis_hash
        or result.get("program_id") != PHOENIX_PROGRAM_ID
        or result.get("scope") != "primary_cross_margin"
        or result.get("delegation_verified") is not True
        or result.get("execution_ready") is not False
        or type(result.get("finalized_slot")) is not int or result["finalized_slot"] <= 0
        or result.get("delegate_address") == result.get("wallet_address")
        or any(not isinstance(result.get(key), str) or not re.fullmatch(r"[1-9A-HJ-NP-Za-km-z]{32,44}", result[key])
               for key in ("delegate_address", "trader_account"))
    ):
        raise RuntimeError("Phoenix native account response binding failed")
    if operation != "validate_connection":
        state = result.get("state")
        plan = request.get("entry", request.get("close", request.get("replace_stop", {})))
        symbol = request.get("symbol") if operation == "read_state" else plan.get("symbol") if isinstance(plan, Mapping) else None
        if (result.get("operation") != operation or not isinstance(state, dict) or not symbol
            or state.get("symbol") != symbol or type(state.get("signed_base_lots")) is not int
            or not isinstance(state.get("conditionals"), list)
            or not isinstance(result.get("state_sha256"), str) or not re.fullmatch(r"[0-9a-f]{64}", result["state_sha256"])):
            raise RuntimeError("Phoenix native state binding failed")
        # Native fraction and raw micro-fee are different units. A missing last
        # trade is explicit; never substitute a midpoint or invented mark price.
        if (type(state.get("taker_fee")) not in (int, float) or not math.isfinite(state["taker_fee"]) or state["taker_fee"] < 0
            or type(state.get("default_taker_fee_micro")) is not int or not 0 <= state["default_taker_fee_micro"] <= 2**32 - 1
            or state.get("price_source") != "phoenix_orderbook_last_trade"):
            raise RuntimeError("Phoenix native market units invalid")
        last = [state.get(k) for k in ("last_trade_price", "last_trade_slot", "last_trade_timestamp")]
        if any(v is not None for v in last) and not (
            type(last[0]) in (int, float) and math.isfinite(last[0]) and last[0] > 0
            and type(last[1]) is int and 0 < last[1] <= result["finalized_slot"]
            and type(last[2]) is int and 0 < last[2] <= 2**63 - 1
        ):
            raise RuntimeError("Phoenix native last trade invalid")
        if operation.startswith("prepare_"):
            prepared = result.get("prepared")
            if not isinstance(prepared, dict) or result["state_sha256"] != request["expected_state_sha256"]:
                raise RuntimeError("Phoenix prepared state binding failed")
            try:
                raw = base64.b64decode(prepared["transaction_base64"], validate=True)
                valid = (0 < len(raw) <= 1232 and hashlib.sha256(raw).hexdigest() == prepared["transaction_sha256"]
                         and prepared["submitted"] is False and re.fullmatch(r"[1-9A-HJ-NP-Za-km-z]{64,88}", prepared["signature"])
                         and isinstance(prepared.get("binding_signature"), str)
                         and re.fullmatch(r"[1-9A-HJ-NP-Za-km-z]{64,88}", prepared["binding_signature"])
                         and all(type(prepared.get(k)) is int and prepared[k] >= 0 for k in ("fee_lamports", "payer_balance_lamports"))
                         and prepared["payer_balance_lamports"] >= prepared["fee_lamports"]
                         and all(type(prepared.get(k)) is int and prepared[k] > 0 for k in ("simulation_slot", "last_valid_block_height")))
            except (KeyError, TypeError, ValueError):
                valid = False
            if not valid:
                raise RuntimeError("Phoenix prepared bytes binding failed")
            if "replace_stop" in request and prepared.get("replace_before") != state:
                raise RuntimeError("Phoenix replacement preimage missing")
        elif operation in ("read_submission", "submit_prepared"):
            prepared = request.get("prepared")
            state_name = result.get("submission_state")
            slot = result.get("confirmation_slot")
            if (not isinstance(prepared, Mapping) or not prepared.get("signature")
                or result.get("signature") != prepared.get("signature")
                or result.get("transaction_sha256") != prepared.get("transaction_sha256")
                or state_name not in {"pending", "finalized", "failed"}
                or type(result.get("send_attempted")) is not bool
                or (operation == "read_submission" and result["send_attempted"])
                or (result["send_attempted"] and (state_name != "pending" or type(result.get("transport_uncertain")) is not bool))
                or (state_name == "pending" and slot is not None)
                or (state_name != "pending" and (type(slot) is not int or not 0 < slot <= result["finalized_slot"]))):
                raise RuntimeError("Phoenix signature status binding failed")
            execution = result.get("execution")
            if "entry" in request:
                if not isinstance(execution, dict) or execution.get("status") not in {"verified", "unavailable"}:
                    raise RuntimeError("Phoenix execution evidence missing")
                if execution["status"] == "unavailable":
                    if set(execution) != {"status", "reason"} or not isinstance(execution.get("reason"), str) or not re.fullmatch(r"[a-z_]{1,64}", execution["reason"]):
                        raise RuntimeError("Phoenix execution evidence invalid")
                elif (state_name != "finalized" or execution.get("source") != "phoenix_finalized_transaction_events"
                    or execution.get("signature") != prepared["signature"]
                    or type(execution.get("slot")) is not int or execution["slot"] != slot
                    or execution.get("client_order_id") != str(plan.get("client_order_id"))
                    or any(type(execution.get(k)) is not int or not 0 <= execution[k] <= 2**64 - 1
                           for k in ("base_lots_filled", "quote_lots_filled", "fee_quote_lots", "fee_lamports"))
                    or execution["base_lots_filled"] != plan.get("base_lots") or execution["base_lots_filled"] == 0
                    or execution["quote_lots_filled"] == 0 or execution["quote_lots_filled"] > plan.get("max_quote_lots", 0)
                    or type(execution.get("signed_base_lots")) is not int
                    or execution["signed_base_lots"] != execution["base_lots_filled"] * (1 if plan.get("direction") == "LONG" else -1)
                    or any(type(execution.get(k)) not in (int, float) or not math.isfinite(execution[k]) or execution[k] < 0
                           for k in ("average_price", "fee_quote")) or execution["average_price"] == 0):
                    raise RuntimeError("Phoenix execution evidence binding failed")
            replacement = result.get("replacement")
            if "replace_stop" in request:
                if not isinstance(replacement, dict) or replacement.get("status") not in {"verified", "unverified"}:
                    raise RuntimeError("Phoenix replacement evidence missing")
                if replacement["status"] == "unverified":
                    if set(replacement) != {"status"}:
                        raise RuntimeError("Phoenix replacement evidence invalid")
                elif (state_name != "finalized" or replacement.get("old_stop") != plan.get("old_stop")
                    or not isinstance(replacement.get("new_stop"), list) or len(replacement["new_stop"]) != 2
                    or any(type(v) is not int or v < 0 for v in replacement["new_stop"])
                    or replacement["new_stop"][0] > 255 or replacement["new_stop"][1] > 2**64 - 1
                    or replacement["new_stop"] == replacement["old_stop"]
                    or any(type(replacement.get(k)) is not int or not 0 < replacement[k] <= 2**64 - 1
                           for k in ("trigger_ticks", "execution_ticks", "remaining_base_lots"))
                    or replacement["remaining_base_lots"] != abs(plan.get("signed_base_lots", 0))):
                    raise RuntimeError("Phoenix replacement evidence binding failed")
    return result


def phoenix_no_order_probe(
    credentials: Mapping[str, str], *, executable: str, executable_sha256: str,
    rpc_url: str, genesis_hash: str,
) -> dict[str, object]:
    """Validate one delegated primary account; never sign or submit a transaction."""
    result = phoenix_account_operation(credentials, executable=executable, executable_sha256=executable_sha256,
        rpc_url=rpc_url, genesis_hash=genesis_hash, request={"operation": "validate_connection"})
    # Connection validation is separate from funds, execution activation and custody approval.
    return {
        "delegation_verified": True, "scope": "primary_cross_margin",
        "finalized_slot": result["finalized_slot"], "execution_ready": False,
    }
