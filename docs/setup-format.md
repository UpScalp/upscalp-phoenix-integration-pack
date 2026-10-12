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

## Wallet approval payload

The hosted setup flow generates a dedicated trading key server-side and builds
an unsigned owner-paid DelegateTrader message. This is a separate account
setup operation from the trade format above. The owner signs in Phantom; the
owner key is never provided to the server. The native `message_base64` field
is a legacy **message**, whereas Phantom's `signAndSendTransaction` request
requires a base58 **transaction**. Prepend the required signature count and
64 zero bytes per signer before encoding. Passing the message directly was
the released version-50 deserialization defect.

The following is the shipped conversion, with no library or private imports:

```javascript
  function messageBase58(encoded) {
    const message = Uint8Array.from(atob(encoded), c => c.charCodeAt(0));
    const signatures = message[0];
    if (message.length < 3 || !signatures || signatures >= 128 ||
        1 + 64 * signatures + message.length > 1232) throw new Error('message_size');
    const bytes = new Uint8Array(1 + 64 * signatures + message.length);
    bytes[0] = signatures;
    bytes.set(message, 1 + 64 * signatures);
    const alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
    let number = 0n, text = '', zeros = 0;
    for (const byte of bytes) number = number * 256n + BigInt(byte);
    while (number) { text = alphabet[Number(number % 58n)] + text; number /= 58n; }
    while (zeros < bytes.length && bytes[zeros] === 0) zeros++;
    return '1'.repeat(zeros) + text;
  }
```

The page passes its result as `params.message` to the injected Phantom
provider's `request({method: 'signAndSendTransaction', params: ...})`.
The existing request options are `skipPreflight: false`,
`preflightCommitment: 'finalized'`, and `maxRetries: 0`. Do not treat a returned
signature as proof of delegation: the hosted flow verifies the on-chain
position authority against the generated key before credential registration.
This snippet performs serialization only; it neither signs nor sends.
