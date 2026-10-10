# cpmt

Payment execution adapters and minimal gate evidence. `crpt` uses this same
contract for crypto payments. Callers choose providers, amounts, currencies,
recurrence, holds, capture, release, refunds and eligibility policy. The library
executes the requested operation and reports a fact; it owns no member balances,
wallets or transfers between members.

`payments::Adapter` declares operations by name. `Payments` registers trusted
adapters, journals an attempt before dispatch, and uses a stable idempotency key.
A repeated operation never dispatches again. Changed instructions under the same
ID fail. Uncertain results stay `Unknown` and must be reconciled, never interpreted
as permission to charge again. Follow-up operations reference a journaled parent
in the same authenticated member/service/action context.

| Feature | Implementation |
|---|---|
| Core (default) | Registry, execution contract, context-bound facts, volatile test journal, signed assertion verifier |
| `storage` | Durable atomic journal through `crlt`; append `storage::SCHEMA` to the application's migrations |
| `stripe` | Maintained `async-stripe` SDK: payment intents, confirmation, capture/cancellation, refunds, subscription creation/update/cancellation |

Provider selection is configuration, not an endorsement or automatic fallback.
No provider account, currency or commercial model is enabled by default.

## Information boundary

Input and provider details stay inside the payment library. `Private` has no
serialization and redacts debug output; owned buffers are zeroized on drop.
SDKs and JSON parsers may allocate temporary copies, so this is not a promise of
complete process-memory erasure. Adapters do not log responses or include them
in errors. Only required provider references are journaled; raw member evidence,
commercial input and member-action responses are not. `observe` needs only an
operation ID and authenticated context. `forget` removes retained continuation
without rearming execution. The storage owner must apply retention to database
backups as well as the live database.

`Outcome::evidence` checks a caller-selected fact and validity bound. `Evidence`
is an opaque in-process value: it cannot be deserialized from a client. The gate
host checks its context and provider. Community gates atomically spend its
receipt-derived claim; global gates accept a reusable fact for their bound check.
Both record minimal gate metadata. No receipt means no gate evidence.
An active subscription or a verified authorization does not prove collection.

Production requires a durable journal, a stable injected random binding key,
authenticated instructions and a trusted clock. The journal rejects a changed
binding key. Losing the journal can destroy retry/replay guarantees. Keep
operation markers for the lifetime of their IDs. No live payment tests or credential discovery.

See [the contract](docs/CONTRACT.md) for adapter and integration obligations.
See [service credentials](docs/CREDENTIALS.md) for injection, token refresh,
account boundaries and the stable journal key.

Additional built-in collection adapters: [Mollie, Square, GoCardless, BTCPay and CoinGate](docs/PROVIDERS.md)
under the optional `providers` feature. The external policy layer selects what
is offered; credentials and provider accounts are supplied by the host.
