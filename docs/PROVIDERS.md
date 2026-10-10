# Provider adapters

Enable `cpmt`'s `providers` feature for `providers::Rest`. Every instance selects
one provider, account and environment. Registration makes an implementation
available; the external policy layer decides whether to offer it and supplies
commercial terms. There is no automatic provider fallback. The core `Adapter`
trait remains open; this list only describes built-in implementations.

| Provider | Implemented operation names | Authentication injected by the host |
|---|---|---|
| Mollie | `mollie.payment.create`, `mollie.payment.cancel`, `mollie.refund.create` | Profile API key or OAuth bearer token; OAuth test mode is explicit adapter configuration |
| Square | `square.payment.create`, `square.payment.capture`, `square.payment.cancel`, `square.refund.create` | Account access token, explicit sandbox flag, API version date |
| GoCardless | `gocardless.payment.create`, `gocardless.payment.cancel`, `gocardless.refund.create` | Account access token and explicit sandbox flag |
| BTCPay Server | `btcpay.invoice.create` | Explicit HTTPS instance, operator store ID, store-scoped Greenfield API key |
| CoinGate | `coingate.order.create` | API application token and explicit sandbox flag |

Stripe intents, holds, capture, refunds and subscriptions remain available through
`stripe`. `crpt` supplies the x402 v2 protocol adapter. The hosted BTCPay/CoinGate
adapters implement the same `cpmt::payments::Adapter`; they feed the same journal
and cgts/cgbl bridges. Currencies and available methods come from the provider and
caller terms, not an application-wide enum. An adapter's available operations do
not promise that every payment method/account supports every operation.

## Construction and credentials

The host constructs `Provider`, `Limits` and `Rest::new(id, provider, credential,
limits)`, then registers the adapter in `Registry`. See [CREDENTIALS.md](CREDENTIALS.md)
for secret references, OAuth refresh, rotation and the separate durable journal
binding key. Provider clients do not read environment variables or secret files.

- `Provider::Mollie { test_mode: None }` for profile API keys. For OAuth credentials,
  set `Some(true)` or `Some(false)` at the composition root. Requests cannot
  override it. It follows child operations and read-only reconciliation.
- `Provider::Square { sandbox, version }` requires an explicit API contract date;
  the current documented version when added was `2026-09-16`. This is a wire
  contract selection, separate from semver library dependency updates.
- `Provider::GoCardless { sandbox }` uses the provider's `2015-07-06` API contract
  header. The credential must belong to the operator account receiving payment.
- `Provider::Btcpay { base, store }` uses the current Greenfield invoice API:
  create under `/api/v1/stores/{store}/invoices`, retrieve under
  `/api/v1/invoices/{id}`, and verify the returned store ID. The host owns instance
  compatibility; older servers with different routes need an adapter for their
  published schema. An installation URL prefix is preserved.
- `Provider::CoinGate { sandbox }` uses its v2 JSON API and `Token` authentication.

Credentials and provider accounts are distinct. Rotating an access token keeps
the adapter ID, account, environment, journal and binding key stable. Changing
account, instance, store or test mode requires a separate configured identity.
Use least-privilege collection/refund permissions; no wallet keys or payout
credentials are needed by these implemented operations.

## Terms and member handoff

`Instruction.terms` contains the provider-native JSON **object**, without a
GoCardless/Square response envelope. The caller supplies amounts, currencies,
methods, timing, descriptions, callback URLs and any necessary billing/KYC data.
References for child operations come only from the journaled parent in the same
member/service/action context. Caller-supplied `id`, `payment_id` or
`idempotency_key` fields are refused. Connected-account routing and application
fee/split fields are refused. These integrations collect for the configured
operator; they do not implement member wallets or transfers between members.

For example, Mollie payment terms include `amount: {value: "12.50", currency:
"CHF"}`, `description` and `redirectUrl`. Square uses `amount_money` in minor
units and a `source_id`; `autocomplete:false` requests a hold. GoCardless uses an
integer `amount`, `currency` and a previously established operator-authorized
`links.mandate`. Its refunds take `amount` and `total_amount_confirmation` (the
existing refunded amount plus this refund); currency comes from the parent.
BTCPay uses a fixed `amount` and `currency`; top-up invoices are not implemented.
CoinGate uses `price_amount`, `price_currency` and its required order description
fields. The adapter supplies a stable opaque order reference.

Mollie and Square payment creation optionally accept a JSON string containing a
member-issued payment token in `Instruction.evidence`. It fills only `cardToken`
or `source_id` and cannot replace a term already supplied by the trusted caller.
Other operations refuse member input. Billing information, bank mandate
ownership and provider session enrollment must be established inside the
appropriate authenticated integration, not copied from arbitrary member JSON.

Hosted checkout links are returned privately as a JSON string in `member_action`.
The host hands them only to the bound member through its authenticated UI. They
are not logged, persisted or included in a gate proof. The UI must treat a link
as a URL, not HTML or script. No webhook payload or browser redirect establishes
a payment fact; observe fetches the authenticated provider resource.

## Facts, uncertainty and retention

- These adapters perform one write request per executor operation and never retry
  HTTP requests or follow redirects. Mollie/GoCardless receive an idempotency
  header; Square receives a 43-character key in its body. These are supplementary
  to the durable pre-dispatch journal, not permission to repeat a write after a
  provider's idempotency window expires. CoinGate order references and BTCPay
  invoice metadata are **not** claimed to provide write idempotency.
- Observation always uses GET. Known same-resource updates preserve their ID on
  a lost reply. A failed create/refund reply cannot substitute the parent's ID
  for the unknown new resource. A create without a surviving ID requires
  provider-side reconciliation; repeating with a new operation is unsafe.
- A response with a validated resource ID but an unproven amount/status retains
  the ID as Unknown, allowing observation or cancellation. Positive facts check
  currency and expected amount. Square cash/external accounting records do not
  establish processed collection; partial authorizations remain Unknown for the
  requested full amount. They can still be cancelled through their reference.
- Known Square/Mollie authorization deadlines limit evidence validity. A malformed
  deadline cannot produce Authorized evidence. A cancellation reports Cancelled;
  it is not automatically proof that a hold was released.
- Mollie paid status with a partial captured amount or unresolved manual capture
  is insufficient for full collection. GoCardless confirmation is a current
  collection fact, not an irreversible guarantee; later failures/chargebacks
  withdraw positive evidence on observation. BTCPay manual marking, partial/late
  payment and unknown additional statuses do not establish settlement. Confirmed
  settlement also checks the actual paid amount. CoinGate's `paid` is its
  authenticated settlement decision; conversion and accepted-payment rules belong
  to that provider/account, not to a local chain-confirmation calculation.
- The journal retains only resource ID/type, the parent payment ID when needed,
  and the expected amount/currency needed to validate later reads. The keyed
  receipt digest contains no billing/KYC data. Raw bodies, member tokens, links,
  headers and provider errors are discarded. `forget` removes continuation data
  without rearming execution. SDK/JSON/HTTP temporary allocations are not a claim
  of process-wide secure erasure; owned input/body buffers are zeroized where
  possible, and request tracing is suppressed for each network future.

Amounts use exact decimal comparison, never floating-point arithmetic. The
implemented representation supports ordinary provider amounts within the decimal
library's range. Unsupported/malformed values fail closed; this does not define a
closed list of currencies for the shared adapter contract.

## Library choices and further integrations

These small JSON integrations reuse current `reqwest`, `serde`, `rust_decimal`
and `time` releases. Version ranges live in Cargo.toml; Cargo.lock records the CI
snapshot. They avoid a custom HTTP/TLS stack, SDK retries and hidden diagnostics.

Hyperswitch Prism was evaluated for much broader native connector reuse. Its
request/response transformers avoid network orchestration, but the current
native dependency graph failed the unchanged advisory gate on 2026-10-10
(cpmt Crow run 30): vulnerable old hyper/h2, protobuf, quick-xml, TLS and time
branches, plus unmaintained packages. `cargo update` cannot cross upstream major
version constraints. Its server also has transport retry/event logging that
requires separate assessment. Prism is **not a dependency or an implemented
adapter**. Reassess it after upstream fixes rather than maintaining a private
fork or ignoring advisories.

Good subsequent candidates include PayPal, Adyen (including local methods such
as TWINT), Core Lightning via its maintained `cln-rpc`, and current Coinbase
Business Checkout. They are not advertised as implemented. In particular,
BTCPay refund creation produces a pull-payment claim, not a completed refund;
that needs its own payout lifecycle before it can report Refunded. Add verified
provider operations through the same adapter boundary and fixture suite.

Primary contracts: [Mollie](https://docs.mollie.com/reference/create-payment),
[Square](https://developer.squareup.com/reference/square/payments-api/create-payment),
[GoCardless](https://docs.gocardless.com/docs/api-reference/payment),
[BTCPay Greenfield](https://docs.btcpayserver.org/API/Greenfield/v1/swagger.json),
[CoinGate OpenAPI](https://github.com/coingate/openapi/blob/main/reference/v2.json),
[Prism native integration](https://github.com/juspay/hyperswitch-prism/blob/main/sdk/rust/README.md).
