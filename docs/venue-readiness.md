# Solana perps venue readiness (25 September 2026)

Based on public first-party documentation read on 25 September 2026.
"Documented" means the venue describes the capability; it does not mean UpScalp
has exercised it. This compares delivery fit, not liquidity, safety, cost or
profitability.

| Venue | Documented interface | Access and testing | Decision |
|---|---|---|---|
| **Phoenix perpetuals (Rise)** | [Rise SDK](https://docs.phoenix.trade/sdk/rise): TypeScript `@ellipsis-labs/rise` and Rust `phoenix-rise`; REST/WebSocket clients, market metadata, instruction builders and trader-state reconciliation. Pinned at [`rise-public@b5a8009f`](https://github.com/Ellipsis-Labs/rise-public/tree/b5a8009f71a4b3bac486b5da9844b2e240e3dea7). | [LiteSVM fixtures](https://docs.phoenix.trade/sdk/litesvm-testing) need the protocol program binaries. Documentation uses mainnet endpoints. [API authentication](https://docs.phoenix.trade/sdk/auth) is separate from transaction signing. | **First venue.** Offline setup conversion first, then local VM execution, then a controlled mainnet path. |
| **Imperial** | The [application](https://www.imperial.space/) serves a perps interface and refers to Phoenix activation and trading. No versioned integration API or SDK was found. | No qualified sandbox, API access or signing contract identified. | Follow-on. Needs Imperial's supported integration surface and access terms first. |
| **Jupiter Perps** | [Position-request docs](https://developers.jup.ag/docs/perps/position-request-account) separate requests from execution. The [CLI perps docs](https://github.com/jup-ag/cli/blob/d8d7386691535469f7d17705ebfc5f432b96feba/docs/perps.md) cover SOL/BTC/ETH, order preview and TP/SL. | CLI trading needs an active key; dry-run uses API simulation. Limit entry cannot be combined with TP/SL in that command. | Credible alternative, not first. Must solve limit entry plus attached protection without dropping protection. |
| **Flash Trade V2** | [V2 SDK](https://docs.flash.trade/flash-trade/flash-trade-protocol/build-on-flash/flash-sdk-v2) `@flash_trade/flash-sdk-v2` (npm 1.1.2). Uses Solana RPC plus Flash `ER_RPC`; positions live in a basket model. | Docs name mainnet and devnet configurations; availability not verified. | Follow-on candidate. Use V2, not the deprecated SDK. |

**Why Phoenix first:** the Rise SDK documents the instruction, account and
status surfaces needed for a narrow setup-to-execution path, and Phoenix
supports attached stop-loss and take-profit orders that track the parent
position. Its [conditional-order rules](https://docs.phoenix.trade/phoenix/matching-engine/take-profit-stop-loss)
matter here: triggers use the mark price, market-style conditionals execute
immediate-or-cancel / all-or-none, limit conditionals can rest unfilled, and a position flip invalidates active
TP/SL. The integration reconciles these states rather than assuming a submitted
stop means a protected position.
