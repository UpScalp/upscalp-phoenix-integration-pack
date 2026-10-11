# UpScalp × Phoenix Perpetuals Integration

Open-source integration layer that lets UpScalp route a protected perpetuals
trade to [Phoenix](https://phoenix.trade) on Solana, using Ellipsis Labs'
[Rise SDK](https://docs.phoenix.trade/sdk/rise), without UpScalp ever holding the
user's wallet key.

Built under UpScalp's Solana Foundation grant (Superteam Ireland), whose first
venue is Phoenix. This repository is the code part of the grant's public
integration pack. UpScalp's scanner, scoring, detectors and strategy rules are
private and are not part of this repository.

> **Status: pre-release.** Everything here has been exercised inside
> a local Solana VM (LiteSVM) loaded with Phoenix's mainnet program binaries.
> No mainnet trade has been executed through this code yet. See
> [What has been proven](#what-has-been-proven).

## What it does

1. **Public setup format** (`upscalp.solana_setup.v1`): a versioned,
   allowlisted description of a trade setup (market, direction, limit entry,
   stop-loss, take-profits, deadline) that reveals nothing about how the signal
   was produced. See [docs/setup-format.md](docs/setup-format.md).
2. **Native runtime** (`engine/phoenix_native.rs`): a small Rust program built on
   the Rise SDK. It receives one JSON request on stdin and can:
   - read a delegated trader's account, position and conditional orders;
   - prepare a protected entry: a market order plus a full-size IOC stop-loss
     and limit take-profit legs, in **one atomic transaction**, with an
     explicit stop execution band rather than the SDK's 10% default;
   - prepare a scoped, reduce-only close of an owned position;
   - replace a stop-loss atomically (cancel the old stop and place the new one
     in one transaction, refusing to loosen it or act on a drifted position);
   - report executed fill price, size and fee only from the finalized
     transaction's Phoenix trade events, otherwise `unavailable` (never a
     planned or ticker price);
   - submit a prepared transaction once, then report status using the original
     signature only (no automatic resend).
3. **Python adapter** (`engine/phoenix_adapter.py`): calls the native runtime
   as a subprocess. It pins the executable by SHA-256, pins the RPC endpoint and
   genesis hash, and keeps key material out of process arguments and error text.
4. **Delegation tool** (`scripts/phoenix_delegate.rs`): runs on the user's own
   machine. It creates a dedicated trading key and uses Phoenix's
   `DelegateTrader` instruction to give that key *position authority* only.
   It simulates by default; `--send` is the only state-changing option. It can
   revoke delegation and read the current authority.

## Custody model

| Key | Held by | Can | Cannot |
|---|---|---|---|
| Owner wallet key | User, on their own machine | Deposit, withdraw, delegate, revoke | (n/a) |
| Delegated trading key | UpScalp runtime | Open, protect and close positions; move collateral between the same owner's subaccounts; pay its own SOL transaction fees | Withdraw funds (to any address, including the owner's); transfer collateral to another trader |

The "cannot" column is enforced by Phoenix's program, not by UpScalp. The
custody fixture (`tests/phoenix_delegate_custody_fixture.rs`) checks it on a
local VM: each delegated outflow is rejected with `IncorrectAuthority`, the
guarded accounts stay byte-identical, and the owner's own withdrawal succeeds.
This covers the instructions tested; it is not a proof over every program
entrypoint. The trading key's SOL fee balance is spendable by UpScalp; keep
only a small fee float on it.

Non-custodial does not mean risk-free. Collateral on Phoenix still carries
smart-contract, oracle, liquidation and execution risk.

## What has been proven

| Check | Where | Level |
|---|---|---|
| Adapter-built instructions execute; stop-loss fires and flattens the position; a one-byte tamper is refused before any send | `tests/upscalp_returned_fixture_tests.rs` | Local VM, Phoenix mainnet programs |
| Protected entry, reduce-only close and atomic stop replacement, LONG and SHORT; request validation; status bound to the original plan | `tests/phoenix_first_party_fixture.rs` | Local VM |
| Delegated custody boundary | `tests/phoenix_delegate_custody_fixture.rs` | Local VM |
| Mainnet delegation, entry, close | none yet | **Not yet exercised** |

## Building and testing

The Rust files compile inside a checkout of
[`Ellipsis-Labs/rise-public`](https://github.com/Ellipsis-Labs/rise-public)
pinned at `b5a8009f71a4b3bac486b5da9844b2e240e3dea7`, using its unchanged
`test_harness`.

| This repo | Copy into the Rise checkout as |
|---|---|
| `engine/phoenix_native.rs` | `rust/sdk/examples/upscalp_phoenix_native.rs` |
| `scripts/phoenix_delegate.rs` | `rust/sdk/examples/phoenix_delegate.rs` |
| `tests/phoenix_unsigned_sdk_fixture.rs` | `rust/sdk/examples/phoenix_unsigned_sdk_fixture.rs` |
| `tests/phoenix_first_party_fixture.rs` | `rust/sdk/tests/phoenix_first_party_fixture.rs` |
| `tests/phoenix_delegate_custody_fixture.rs` | `rust/sdk/tests/phoenix_delegate_custody_fixture.rs` |
| `tests/upscalp_returned_fixture_tests.rs` | `rust/sdk/tests/upscalp_returned_fixture_tests.rs` |

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p phoenix-rise \
  --test phoenix_first_party_fixture
```

The VM tests need Phoenix's program binaries, which this repository does not
redistribute. Point the SDK loader at locally obtained copies with
`RISE_SDK_LOCALNET_REQUIRE_PROGRAMS=1` and `RISE_SDK_LOCALNET_ETERNAL_SO`,
`RISE_SDK_LOCALNET_EMBER_SO`, `RISE_SDK_LOCALNET_HAWKEYE_SO` and
`RISE_SDK_LOCALNET_FLICKER_SO`. With `REQUIRE_PROGRAMS=1`, a missing binary is
a failure, not a skip.

`engine/phoenix_adapter.py` needs only Python 3.12 and the standard library.
Its unit tests run inside UpScalp's private codebase and are not included here.

## Provenance

The Phoenix integration was built between 25 September and 11 October 2026,
during the Colosseum Crypto World's Fair. The commits in this repository date
from 9 to 11 October; earlier work (an offline setup validator) depends on
UpScalp's private codebase and is not included. Development was
AI-assisted: the founder directed Codex coding agents, with independent agent
QA and review gates. Each commit carries a `Source-Commit:` trailer naming
the private commit it reproduces.

## License

MIT. See [LICENSE](LICENSE).

`tests/upscalp_returned_fixture_tests.rs` is adapted from Ellipsis Labs'
MIT-licensed Rise SDK test suite; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
