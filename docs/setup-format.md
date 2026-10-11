# Public setup format: `upscalp.solana_setup.v1`

An allowlisted **data contract** for describing a trade setup. It is not a
signed order, and it does not reveal how the setup was generated. Only a setup
that has already been authorized privately is exported; publication or venue
support never makes a setup eligible.

The example is wholly synthetic. Its numbers show units and validation only;
they set no strategy, risk level, market minimum or position size. `SYNTH` has no
live instrument mapping, so the example cannot become an executable order.

```json
{
  "schema_version": "upscalp.solana_setup.v1",
  "grant_setup_ref": "example-setup-001",
  "source": "upscalp",
  "created_at": "2026-09-25T00:00:00Z",
  "valid_until": "2026-09-25T01:00:00Z",
  "market": {
    "venue": "phoenix",
    "product": "perpetual",
    "instrument": "SYNTH",
    "base_asset": "SYNTH",
    "price_currency": "USD",
    "collateral_asset": "USDC"
  },
  "direction": "LONG",
  "entry": {"type": "LIMIT", "limit_price_usd": "100.00"},
  "risk": {
    "max_notional_usd": "10.00",
    "max_leverage": "1.00",
    "stop_loss": {"trigger_price_usd": "95.00", "trigger_basis": "MARK"},
    "take_profits": [{"trigger_price_usd": "110.00", "trigger_basis": "MARK", "close_fraction": "1.00"}]
  },
  "skip_conditions": ["EXPIRED", "MISSING_AUTHORITY", "UNSUPPORTED_INSTRUMENT", "INVALID_GEOMETRY", "UNSUPPORTED_PROTECTION", "RISK_LIMIT_FAILED", "USER_DECLINED"],
  "user_action": "REVIEW_ONLY",
  "status": "CREATED",
  "outcome": {"state": "NOT_EVALUATED", "basis": "NONE", "observed_at": null}
}
```

## Rules

- **Identity.** `grant_setup_ref` is opaque. A private crosswalk binds it to the
  original signal, creation time and payload. Conflicting identity or geometry
  is an error. New IDs, retries or format revisions cannot create new setups.
- **Venue.** `market.venue` names the intended destination, not the venue where
  the signal was observed. Destination market and price checks happen in the
  adapter.
- **Time.** UTC timestamps with an explicit timezone, and `valid_until` later
  than `created_at`. Expired setups stay inspectable but cannot be executed.
- **Units.** Prices are USD per base unit, notional is USD, leverage is a ratio,
  fractions are dimensionless. Values are decimal strings that must parse to
  finite positive numbers. For LONG, the stop is below entry and every target
  above; SHORT is the reverse. Take-profit fractions sum to exactly 1.
- **Size.** Quantity and margin are not implied by the format. The adapter
  derives them from authorized sizing plus the venue's tick, lot and margin
  metadata, and checks again after rounding. Leverage or risk is never raised
  to meet a venue minimum.
- **Triggers.** `MARK` is the trigger reference, not a guaranteed fill price.
- **Skips.** `skip_conditions` lists reason categories, not private thresholds.
  Unknown fields, unsupported versions, duplicate keys and missing risk or
  authority all fail validation; nothing defaults to permissive.
- **Authority.** `user_action` starts as `REVIEW_ONLY`. A published setup never
  grants signing or account authority. `status` and `outcome` are projections of
  the recorded lifecycle, not editable evidence.

The format does not expose cohort membership, attempts, identities or feedback.
