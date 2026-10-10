# Service credentials and configuration

The host service supplies credentials. Payment and human-verification libraries
do not discover accounts, inspect the process environment, read a secrets store,
register with a provider or ask the member for operator credentials.

The composition root performs this sequence:

1. Read the externally managed service configuration: enabled adapter instances,
   provider accounts, production/sandbox selection, allowed operations and limits.
2. Resolve each instance's secret reference through the host's secret-management
   facility. A reference is configuration; the secret value is not.
3. Construct the provider SDK/client or the adapter's private credential value.
   Inject the endpoint, credentials, timeout and byte limits explicitly.
4. Register the configured adapter with `Payments`. The caller supplies the
   authenticated member/context and commercial instructions for each operation.
5. Deliver any short-lived provider ceremony/checkout token only to the member
   and flow for which it was issued. Keep operator credentials server-side.

The library boundary stays the same whether the host uses a mounted secret file,
an operating-system credential service, a managed secret store or an encrypted
deployment secret. These are host integration choices, not library dependencies.
The host must not put resolved secrets in command-line arguments, logs, ordinary
configuration snapshots, member requests, journals or gate results.

## Separate credential classes

| Value | Owner and lifetime | How it enters the library |
|---|---|---|
| Provider secret/API key | Operator account; replaceable | Private constructor argument or explicitly configured upstream SDK/client |
| OAuth access token | Provider session; short-lived | Host-managed refreshing credential source or injected SDK with explicit credential configuration |
| OAuth client secret/private signing key | Operator account; server only | Host token/signing facility or an injected provider SDK; never payment `terms` or member `evidence` |
| Public application/merchant identifier | Operator configuration | Explicit provider configuration; it is not proof of authorization |
| Webhook verification key/secret | Receiver service; separate from outbound API authentication | Explicit callback verifier configuration where a callback integration is implemented |
| Payment binding HMAC key | Stable payment-journal identity | `Payments` constructor, separately from provider credentials |
| Member payment-method token or signed authorization | Member interaction; transient | `Instruction::evidence`, bound to trusted terms and authenticated context |
| Provider checkout/3DS/ceremony token | One authenticated member flow; transient | `Observation::member_action`; never journaled or used as operator authentication |

Do not manufacture a new binding key on restart or credential refresh. The
durable journal rejects a changed key. Losing that key or the journal requires
an explicit recovery/migration; silent reinitialization would destroy replay
protection. Provider API-key rotation must not change adapter identity, receipt
namespace, operation IDs or pending provider references.

## Current adapters

| Adapter | Required configuration | Credential handoff |
|---|---|---|
| `cpmt::stripe::Stripe` | Instance ID, timeout and combined input limit; the SDK selects Stripe's HTTPS API | `Private` containing the server-side Stripe secret key, passed to `Stripe::new` |
| `crpt::http::HttpFacilitator` | Explicit HTTPS facilitator URL, timeout and response-size bound | Explicit authentication `HeaderMap` passed to `HttpFacilitator::new`; mark secret header values sensitive |
| `crpt::x402::X402` with another facilitator | Explicit upstream `Facilitator` implementation plus adapter limits | Inject the configured implementation; use its maintained authentication/signing support when credentials vary per request |
| `chmn` provider adapters | Public app/audience/issuer configuration, explicit endpoints and limits | Host-injected provider credentials or SDK/client, as specified in that adapter's contract; raw biometric data never enters the leaf |

Header names, API-key prefixes and token refresh protocols are provider-specific.
Do not invent a universal bearer-token convention. A static header map is suitable
only for credentials valid for its lifetime; it cannot implement expiring signed
requests. Supply a dynamic upstream implementation for those protocols.

## Replacement and failure

The host refreshes expiring tokens through the provider's maintained OAuth/SDK
support. It may rebuild an adapter with replacement credentials while retaining
the same journal, stable identity and binding key. Drain or coordinate in-flight
operations before discarding the old instance. Keep pending operations observable
under the provider account that created them; switching merchant accounts is not
a credential rotation.

Missing or rejected credentials produce a private failure. They never trigger
fallback to another account, provider, network or sandbox. A failed or ambiguous
payment response remains uncertain: refreshing credentials does not authorize a
new payment ID or another dispatch. Use read-only reconciliation where supported.

The service chooses least-privilege permissions for the operations it enables.
An installed adapter or optional Cargo feature does not provision a merchant
account, obtain contractual access, configure production billing or enable a
payment method. External rules remain responsible for the offered product.

## Examples and verification

Examples use inert values and constructor signatures, never environment discovery:

```rust,ignore
// Obtained by the host's composition root, outside the payment library.
let secret: cpmt::payments::Private = resolved_provider_secret;
let adapter = cpmt::stripe::Stripe::new(
    configured_instance_id,
    secret,
    configured_timeout,
    configured_input_limit,
)?;
```

For x402, the host constructs a `HeaderValue`, calls `set_sensitive(true)` for
each credential header, inserts it under the provider's documented header name,
then injects the map. Never place credentials in URL user-info or query strings.

Fixture tests inject inert credentials and inspect local requests. Production
credentials, live charges, verification ceremonies and provider registrations
are outside those tests. Owned secret buffers are zeroized where possible;
third-party SDK, HTTP and parser copies may exist transiently. Do not claim
complete process-memory erasure.
