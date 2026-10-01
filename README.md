# cpmt

**Entitlement-only recurring-payment assertion for cvld admission.**

The intermediary owns customer records and billing support. `cpmt` verifies
a member-bound entitlement is currently valid, with expiry and replay guard.
No customer/payment ids flow to the operator.

## Scope

`cpmt` performs one local check of an intermediary's signed, expiring payment
entitlement. The intermediary owns recurring billing, renewal failures,
cancellation and billing support. The host binds the signed commitment to the
authenticated member and community, then atomically consumes the assertion's
replay key through its storage owner before admission. `verify` alone does not
perform either host operation.

Provider selection and fallback belong to `cfbk`, configured by `crbk`. This
library is referenced by global and community gatekeeping; the definitive
single home is still a composition decision. There is no live billing adapter,
customer lookup, login history, HTTP request logging or payment credential
storage. A provider dashboard, API, export or support channel that lets the
operator recover customer identities fails the entitlement-only boundary.
No live billing service has been qualified by the fixture tests.

## Dependency choices

The implementation reuses Ed25519-Dalek for signature verification, RustCrypto
`sha2` for domain-separated SHA-256 commitments and replay keys, and Serde JSON
for the existing versioned array encoding. Those established libraries cover
the required operations without another cryptographic implementation or a
payment-provider SDK carrying customer records. Existing wire domains remain
unchanged. Native and actual Node Wasm tests use real signatures, expiry,
field mutations and malformed input; [the contract](docs/CONTRACT.md) defines
the remaining host and provider obligations.

## License

Copyright 2026 Julian Y. Richard Corbet. Licensed under
[LGPL-3.0-only](LICENSES/LGPL-3.0-only.txt)
[WITH LGPL-3.0-linking-exception](LICENSES/LGPL-3.0-linking-exception.txt),
with the incorporated [GPL version 3](LICENSES/GPL-3.0-only.txt).
Combined works may link statically or dynamically without relinking duties;
library modifications stay LGPL.
See [licensing notes](LICENSE.md).

## Continuous verification

Dependency updates follow main and are tested against one CI-resolved lockfile.
Line and branch coverage target 100%; failures remain blocking. See
[the coverage contract](docs/COVERAGE.md) for measurement and exclusions.
