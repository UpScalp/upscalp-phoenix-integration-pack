# UpScalp × Phoenix Perpetuals Integration

Open-source integration layer that lets UpScalp route a protected perpetuals
trade to [Phoenix](https://phoenix.trade) on Solana, using Ellipsis Labs'
[Rise SDK](https://docs.phoenix.trade/sdk/rise), without UpScalp ever holding the
user's wallet key.

Built under UpScalp's Solana Foundation grant (Superteam Ireland), whose first
venue is Phoenix. This repository is the code part of the grant's public
integration pack. UpScalp's scanner, scoring, detectors and strategy rules are
private and are not part of this repository.

## Connect and trade in UpScalp Auto

Phoenix connects through the same account setup flow as the other venues:

1. In Telegram, open **Connect account**, choose **Phoenix**, and follow the
   secure setup link.
2. On the setup page, tap **Connect Phantom and approve**. Connect the wallet
   that owns your Phoenix account. One approval delegates trading authority and,
   if the new trading key has less than 0.003 SOL, transfers 0.003 SOL to it for
   transaction fees. UpScalp creates that dedicated key; your owner key stays
   in your wallet.
   There are no private keys to paste.
3. The page checks the delegation and completes the connection. Return to
   Telegram, open **Accounts**, and select **Use Phoenix**.
4. Run **Test connection** to check the selected account and its trading
   authority.
5. Choose **Test Trade** and confirm the **SOL perpetual** trade on Solana
   mainnet. The demo Test Trade is a plain market order without TP/SL;
   ordinary protected entries use the separate atomic bracket path.
6. Inspect the demo position in Phoenix. Manage and close this unprotected
   demo position in the Phoenix app; the native close fix for this specific
   test path is outside this snapshot.

The Phoenix account needs eligible collateral. The owner wallet must cover
the delegation transaction fee plus the conditional 0.003 SOL transfer.
The trading key pays order fees from its fee float. Reconnecting the same
wallet supports an owner-approved replacement delegate; an already matching
delegate is recognized without another delegation.

The setup page sends an unsigned **serialized transaction**, not just its
message, to Phantom. The exact envelope conversion is documented in
[the wallet payload section](docs/setup-format.md#wallet-approval-payload).

This is the UpScalp product flow. This public repository contains its
standalone Phoenix integration components and fixtures, not the hosted Telegram
bot or setup service. The Python adapter imports only the standard library;
the native code builds against the public Rise SDK. No private UpScalp modules,
scanner or strategy code are required to build these components.

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
   - prepare a scoped, reduce-only close of a protected owned position;
   - prepare the SOL-only unprotected demo entry with an explicit 400,000 CU
     budget; the caller supplies its IOC price allowance (the private executor
     sizing and price policy are not included here);
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
   Static refusal codes are preserved so callers can explain a rejected request.
4. **Operator delegation tool** (`scripts/phoenix_delegate.rs`): an alternative
   to the in-app wallet flow, run on the user's own machine. It creates a
   dedicated trading key and uses Phoenix's
   `DelegateTrader` instruction to give that key *position authority* only.
   It simulates by default; `--send` is the only state-changing option. It can
   revoke delegation and read the current authority.

## Custody model

| Key | Held by | Can | Cannot |
|---|---|---|---|
| Owner wallet key | User, on their own machine | Deposit, withdraw, delegate, revoke | (n/a) |
| Delegated trading key | UpScalp runtime | Open, protect and close positions; move collateral between the same owner's subaccounts; pay its own SOL transaction fees | Withdraw funds (to any address, including the owner's); transfer collateral to another trader |

Phoenix documents the distinction between the collateral-owning `authority`
and trading `position_authority` in its
[Accounts guide](https://docs.phoenix.trade/phoenix/collateral-and-accounts/accounts).
The delegated key places orders as `position_authority`; withdrawals remain
authority-only. The
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
| SOL Test Trade opened a 0.45 SOL LONG at 13:53 UTC on 12 October 2026 | Recorded UpScalp demo; team-reported Phoenix API readback | Mainnet observation, not a full lifecycle or protection test; private identifiers omitted |

## Building and testing

The Rust files compile inside a checkout of
[`Ellipsis-Labs/rise-public`](https://github.com/Ellipsis-Labs/rise-public)
pinned at `b5a8009f71a4b3bac486b5da9844b2e240e3dea7`, using Rust/Cargo 1.96
and its unchanged `test_harness`. Enable the SDK's `events` feature for
finalized execution-event decoding.

| This repo | Copy into the Rise checkout as |
|---|---|
| `engine/phoenix_native.rs` | `rust/sdk/examples/upscalp_phoenix_native.rs` |
| `scripts/phoenix_delegate.rs` | `rust/sdk/examples/phoenix_delegate.rs` |
| `tests/phoenix_unsigned_sdk_fixture.rs` | `rust/sdk/examples/phoenix_unsigned_sdk_fixture.rs` |
| `tests/phoenix_first_party_fixture.rs` | `rust/sdk/tests/phoenix_first_party_fixture.rs` |
| `tests/phoenix_delegate_custody_fixture.rs` | `rust/sdk/tests/phoenix_delegate_custody_fixture.rs` |
| `tests/upscalp_returned_fixture_tests.rs` | `rust/sdk/tests/upscalp_returned_fixture_tests.rs` |

```sh
cargo build --release --locked --manifest-path rust/Cargo.toml -p phoenix-rise \
  --features events --example upscalp_phoenix_native --example phoenix_delegate
cargo test --release --locked --manifest-path rust/Cargo.toml -p phoenix-rise --features events \
  --test phoenix_first_party_fixture
```

The VM tests need Phoenix's program binaries, which this repository does not
redistribute. Point the SDK loader at locally obtained copies with
`RISE_SDK_LOCALNET_REQUIRE_PROGRAMS=1` and `RISE_SDK_LOCALNET_ETERNAL_SO`,
`RISE_SDK_LOCALNET_EMBER_SO`, `RISE_SDK_LOCALNET_HAWKEYE_SO`,
`RISE_SDK_LOCALNET_FLIGHT_SO` and
`RISE_SDK_LOCALNET_FLICKER_SO`. With `REQUIRE_PROGRAMS=1`, a missing binary is
a failure, not a skip.

`engine/phoenix_adapter.py` needs Python 3.12 and the standard library on Linux,
with executable sealed memfd support and `/proc/self/fd` available. It executes
the verified immutable bytes instead of reopening an executable pathname.
Its unit tests run inside UpScalp's private codebase and are not included here.

## Provenance

The Phoenix integration was developed during the Colosseum Crypto World's
Fair. Earlier work on an offline setup validator depends on UpScalp's private
codebase and is outside this standalone package. Development was AI-assisted:
the founder directed Codex coding agents, with independent agent QA and review
gates. This snapshot follows the deployed source as of 12 October 2026:

| Component | Source SHA-256 |
|---|---|
| Native runtime (source revision `46acaee4`) | `6c7a2b2becc7c3b20e6bd847e6301d3d9e364641976e4686197fe6307271c774` |
| Python adapter | `84859a7773191d9c0ea06302667fe80301521c59f2a3cc64203075add74db5ff` |
| PO-local delegation CLI | `2417d05c1b331472514bf8f4a1a5eeced2b4981e6da1f02403c076a47c1edf01` |

Public fixture adaptations remain separate from the hosted product. The wallet
envelope snippet matches the released setup-page conversion. Newer local
close work is excluded. No hosted bot, credential store or private executor
is exported by this repository.

## License

MIT. See [LICENSE](LICENSE).

`tests/upscalp_returned_fixture_tests.rs` is adapted from Ellipsis Labs'
MIT-licensed Rise SDK test suite; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
