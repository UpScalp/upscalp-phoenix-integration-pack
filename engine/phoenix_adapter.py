"""Phoenix native SDK boundary; no import-time connection or default wallet."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
import subprocess
from typing import Mapping
from urllib.parse import urlsplit


PHOENIX_PLATFORM_ID = "phoenix"
PHOENIX_PROGRAM_ID = "EtrnLzgbS7nMMy5fbD42kXiUzGg8XQzJ972Xtk1cjWih"


def phoenix_no_order_probe(
    credentials: Mapping[str, str], *, executable: str, executable_sha256: str,
    rpc_url: str, genesis_hash: str,
) -> dict[str, object]:
    """Validate one delegated primary account; never sign or submit a transaction."""
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
    request = {
        "operation": "validate_connection",
        "wallet_address": credentials.get("wallet_address", ""),
        "private_key": credentials.get("private_key", ""),
        "rpc_url": rpc_url,
        "genesis_hash": genesis_hash,
    }
    try:
        completed = subprocess.run(
            [str(path)], input=json.dumps(request), text=True, capture_output=True,
            timeout=40, check=False,
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
        or result.get("wallet_address") != request["wallet_address"]
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
    # Connection validation is separate from funds, execution activation and custody approval.
    return {
        "delegation_verified": True, "scope": "primary_cross_margin",
        "finalized_slot": result["finalized_slot"], "execution_ready": False,
    }
