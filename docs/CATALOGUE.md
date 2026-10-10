# Payment integration catalogue

Primary references checked 2026-10-10. This is an extension inventory, not a list
of enabled accounts or qualified live payment methods. Inclusion selects no
vendor, currency, offer, recurring rule or deposit policy. The exact built-in
operation names and tested limitations are in [PROVIDERS.md](PROVIDERS.md).

## Implemented and fixture-tested

| Integration | Present capability | Upstream reuse and limits |
|---|---|---|
| Stripe | Intent creation/confirmation, authorization, capture/cancel, refunds, subscription creation/update/cancel, authenticated reads | Maintained async-stripe SDK. A subscription state is not collection. SDK response buffering and lost error fields are documented in [the contract](CONTRACT.md). |
| Mollie | Create/cancel payment, create refund, authenticated reads | Maintained reqwest/serde against the provider API; explicit test mode and bounded responses. No customer/mandate/subscription management adapter. |
| Square | Create/capture/cancel payment, create refund, authenticated reads | Maintained reqwest/serde; explicit API contract date and environment. Partial authorization and cash/external records do not prove full processed collection. |
| GoCardless | Collect against an established mandate, cancel payment, create refund, authenticated reads | Maintained reqwest/serde; caller establishes mandate ownership. Collection can later fail or be charged back. No authorization-hold guarantee. |
| BTCPay Server / CoinGate | Create invoice/order and read its authenticated state | Same payment boundary, including amount checks. No payout, wallet, refund or hold implementation. |
| x402 in crpt | Verify and settle a configured protocol offer; injected receipt observation | Maintained x402 types and optional bounded HTTP transport. Verification is not collection. See the [crypto catalogue](https://git.corbet.ch/corbet-foss/crpt/src/branch/main/docs/CATALOGUE.md). |

All rows require injected account credentials, explicit mode and a bounded
service-fee quote/tariff. None currently discovers an authoritative final
processing fee automatically. See [CREDENTIALS.md](CREDENTIALS.md). Real upstream
SDK/transport fixtures validate our integration; live accounts and provider
sandboxes remain unexecuted.

## Candidates, not implemented

| Candidate | Concrete extension and reconciliation requirement | Credentials, cost and reuse constraints |
|---|---|---|
| PayPal Orders / Payments | Create order, authorize, capture, void and refund. Keep order, authorization, capture and refund IDs distinct; reauthorization replaces the authorization ID. Read authenticated resources after uncertainty. | Host supplies OAuth access token from its app credentials and explicit environment. Preserve PayPal-Request-Id semantics per operation. Authorization expiry/honor periods limit the claim; merchant and method fees require a supplied tariff. Direct official REST integration is a candidate; no Rust SDK has been selected. [Authorize/capture contract](https://developer.paypal.com/v5/checkout/auth-capture/). |
| Adyen Checkout | Payment creation plus capture/cancel/refund, including provider-supported local methods. A cancellation response of received acknowledges a request; authenticated completion is a separate event. Preserve original payment and modification references. | Host supplies merchant account/API key and endpoint/region. POST idempotency is bounded in time and is not shared across regions. No automatic regional retry. Webhook authentication/reconciliation must be implemented before asynchronous completion can prove a fact. Official API plus maintained HTTP/crypto libraries are the proposed route; account tariffs remain explicit. [Cancellation](https://docs.adyen.com/online-payments/cancel/), [idempotency](https://docs.adyen.com/development-resources/api-idempotency). |
| TrueLayer Payments v3 | Bank payment initiation, refunds and provider-supported variable recurring payments. Payment creation/authorization does not establish settlement. Persist only necessary IDs through the host port and inspect the authenticated payment lifecycle. | OAuth token, merchant configuration and request-signing key are distinct. Reuse the provider's Rust signing library; do not implement JWS primitives. Request signatures and idempotency headers bind the write. Provider/bank coverage and fee tariffs require explicit configuration. [API contracts](https://docs.truelayer.com/reference/welcome-api-reference), [signing libraries](https://docs.truelayer.com/docs/sign-your-payments-requests), [payment reads](https://docs.truelayer.com/docs/get-data-about-a-payment). |
| Paddle Billing | Transactions, subscriptions and adjustments; suitable for a provider-managed billing flow when the caller selects it. A subscription lifecycle must remain separate from an authenticated paid transaction. | Server bearer API key with scoped permissions; frontend tokens have a different role. Use official HTTP contracts with maintained transport. Qualify write idempotency and lost-create reconciliation before implementation; no such guarantee is claimed here. Transaction pricing previews describe customer totals, not necessarily the operator's full service fee. [Resources](https://developer.paddle.com/api-reference/), [authentication](https://developer.paddle.com/api-reference/about/authentication/). |
| Mollie recurring lifecycle | Extend the existing adapter with customer/mandate/subscription operations. A first payment and valid mandate establish prerequisites; each subsequent collection has its own outcome. | Reuse the existing transport and credential injection. Require explicit ownership/context for every customer and mandate reference. Provider recurrence and retry behavior must be exposed before enabling those operations. [Recurring contract](https://docs.mollie.com/docs/recurring-payments). |
| Additional Square delayed-capture behavior | Qualify provider-supported duration and delayed action choices with exact amount, method and version fixtures. Capture/cancel are already present; further flows need their own declared capabilities. | Existing access token and version selection. Provider automatic completion/cancellation at the deadline is an external side effect to disclose, not a local fallback rule. [Delayed capture](https://developer.squareup.com/docs/payments-api/take-payments/card-payments/delayed-capture). |
| Lightning and other crypto collection services | Invoice creation and authenticated receipt reconciliation can share this contract; chain/wallet support is not a generic payment guarantee. | Reuse appropriate maintained protocol/RPC clients, with node/service credentials injected and fees bounded. No custody or private wallet key integration. See the [crpt candidate catalogue](https://git.corbet.ch/corbet-foss/crpt/src/branch/main/docs/CATALOGUE.md). |
| Hyperswitch Prism | Potential reuse of provider request/response transformations across many connectors. Each operation needs independent capability qualification; connector presence alone is insufficient. | The evaluated native dependency graph failed the unchanged advisory gate. It is not a dependency. Reassess an upstream-fixed release, including license, diagnostics and dependency footprint; no private fork or advisory exception. Remote orchestration additionally needs an explicit trusted service boundary and retry/logging assessment. [Upstream Rust integration](https://github.com/juspay/hyperswitch-prism/blob/main/sdk/rust/README.md). |

## Qualification contract

A new adapter declares only operations it actually implements. Prefer maintained
upstream SDKs; when none satisfies the boundary, integrate the official schema
through maintained HTTP and serialization crates. Current compatible dependency
ranges and a tested lockfile do not imply a fixed provider API version. Keep our
Rust safe; assess native/unsafe transitive code separately rather than claiming
that crate-level forbid eliminates it.

Before a capability becomes implemented, exercise its real upstream code with
inert fixtures for exact wire requests, authentication, duplicate dispatch,
lost replies, parent ownership, amounts/assets, lifecycle/expiry, sandbox isolation,
private complete output and minimal projection. Refresh the trusted clock at
every transport edge. Pass all required dependency and coverage checks. Live
qualification, where later authorized, is distinct and must remain visibly
unverified until performed.

Require explicit quote provenance, scope, denomination, expiry and a defensible
maximum covering every potentially billed phase, including discovery and reads.
Do not infer free service from an absent fee field, free SDK, sandbox label or
zero payment principal. A provider's retry suggestion does not authorize a
second dispatch: ambiguous effects stop the shared cascade and require host
reconciliation. Preserve authenticated unexpected charges and failed state
commits as described in [CONTRACT.md](CONTRACT.md).
