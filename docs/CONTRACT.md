# Payment adapter contract

The composition root authenticates the member and supplies `Instruction`:
nonzero random operation ID, optional parent ID, adapter, advertised operation,
service/member/action binding, exclusive start deadline, trusted commercial terms
and transient evidence. Never copy commercial terms from an unauthenticated
client. There is no automatic provider selection or payment policy.

Adapters are trusted library code, not client plugins. They must:

- Advertise only implemented operations and a nonzero combined input limit; use
  current upstream libraries. The executor applies the limit before hashing or
  writing an operation marker.
- Bind member evidence to the caller's amount, currency, recipient and resource.
- Use the provided stable key for upstream idempotency. Never hide retries.
- Return `Unknown` for ambiguous side effects; `observe` is read-only.
- Distinguish verification, holds, collection, arrangements and refunds.
- Emit a stable, protocol-namespaced receipt identity for positive gate facts.
  Repeated observation of the same fact must produce the same identity, even
  through different configured adapters. Include enough detail to distinguish
  facts such as authorization and collection, without inventing a new receipt.
- Retain only references required for later operations. Return member actions
  separately, for delivery to the originally authenticated member only.
- Discard provider responses and diagnostics; never return billing/KYC records.

The executor persists a keyed instruction digest and `Unknown` before dispatch.
Concurrent calls or process cancellation cannot dispatch that ID twice. Journal
writes use compare-and-swap revisions. The namespace is atomically bound to the
injected key fingerprint; reopening with a different key fails before dispatch. Observation requires a retained reference and never revises an unfinished
dispatch. `forget` refuses an unfinished dispatch so a late reply cannot silently
undo acknowledged erasure. A failed final write leaves an uncertain
result and is never success. This is at-most-once dispatch, not a guarantee that
a provider collected money. Reconciliation without a provider reference may
remain `Unknown`; no generic library can infer an unreported external outcome.

Journal records remain inside the payment boundary. They contain opaque operation
and context identifiers, state, optional provider validity, keyed receipt and
instruction digests, a revision, and optional necessary provider continuation.
No financial ledger, raw request history, payment-method details or KYC records
are stored. `forget` erases the continuation but retains duplicate-execution
protection. Do not expose serialized `Record` values as application APIs.

A gate consumes `Evidence` and rechecks its service/member/action/provider and
exclusive expiry. Community gates (`cgts`) atomically spend the receipt claim
with their result. Global gates (`cgbl`) accept a fact for a bound check using
existing retry protection; it is neither a one-use purchase nor person uniqueness.
The payment journal prevents a receipt moving between authenticated contexts.
Policy chooses which fact is acceptable and how long it remains valid; provider
expiry caps that choice. Evidence and outcomes are snapshots; provider changes require
observation and the host's existing invalidation policy.

## Stripe adapter

`terms` is a Stripe parameter object. It preserves provider-specific currency,
amount and subscription behavior. Optional `evidence` is a JSON string containing
a provider-issued payment-method token; it cannot override commercial parameters.
Never send raw card data. Parent operations supply object references for capture,
cancellation, refunds or subscription updates. No arbitrary object IDs, connected
account headers, transfers or application fees are accepted. Provider errors are
uniform and ambiguous outcomes remain `Unknown`. Lost create responses without
an object ID require provider-side reconciliation; a fresh ID is not a safe retry.

Upstream: [async-stripe](https://github.com/arlyon/async-stripe), permissive
MIT/Apache-2.0 licensing. The current 1.0 release-candidate SDK line is used
because 0.41 retains affected, obsolete TLS/HTTP dependencies; CI checks the
complete feature graph against current RustSec advisories. Protocol encoding, authentication and API version belong
to the SDK. `crlt` owns storage; `ed25519-dalek` owns assertion signatures; `hmac`,
`sha2` and `zeroize` provide maintained cryptographic and memory primitives.
Use semver ranges and current `main` for first-party libraries, never exact
version or commit requirements. The lockfile records the dependency snapshot
validated by CI; updating it is part of dependency maintenance.
