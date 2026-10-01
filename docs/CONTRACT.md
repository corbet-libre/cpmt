# Payment entitlement contract

`cpmt` is the LGPL entitlement-only recurring-payment gate used by cvld. It
checks a single signed intermediary assertion; ordering, fallback and minimum
provider requirements belong to `cfbk` under `crbk` settings.

The independent intermediary must own customer/payment records and recurring
renewals, failures, cancellation and support. The operator receives only an
opaque scoped commitment, eligibility expiry and a one-use assertion identifier.
A webhook filter controlled by the operator, expandable API access, customer
exports or support lookup do not meet that boundary. GNU Taler, wallet funding
and coins are outside this fiat-payment contract.

`verify` authenticates the existing `cpmt.assertion.v1` JSON-array bytes using
the configured Ed25519 public key, checks nonempty bounded fields and strict
expiry, and rejects obvious provider-ID prefixes with a uniform error. This is
a syntactic guard, not proof that a provider enforces the privacy contract.
The trusted caller must compare the signed commitment to its authenticated
member/community commitment and atomically consume the replay key until expiry
before authorizing admission. `verify` has no storage or subject argument and
cannot enforce those caller obligations. Durable storage belongs to the host
through `crlt`; this library stores no customer data or request/login history.

`cpmt.commitment.v1`, `cpmt.replay.v1` and `cpmt.attestation.v1` preserve the
existing wire domains. Cryptography is delegated to Ed25519-Dalek and RustCrypto
SHA-256; no local primitive is implemented. Qualified provider integration
remains open. Tests use real cryptographic fixtures and execute natively and
on Wasm, without charges, account registration, outreach or registry publication.
