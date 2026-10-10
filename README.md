# cpmt

Reusable payment expertise over maintained provider integrations. Callers choose
financial terms, providers and the meaning of a result. `crpt` shares this
contract. There is no custody, wallet key, account ledger or member-to-member
transfer.

`Payments::preview` binds an explicit provider quote or configured tariff to the
account instance, mode and exact instruction. Local planning never calls a
provider. The shared `cfbk::cost::Plan` checks capabilities and the whole-cascade
budget and creates a move-only permit. `Payments::dispatch` requires that permit;
there is no uncosted execution path. Payment principal is separate from fees.
Unknown fees never mean free. Installing an adapter enables no account or offer.

The optional `stripe` feature uses the maintained async-stripe SDK. The optional
`providers` feature implements [Mollie, Square, GoCardless, BTCPay and
CoinGate](docs/PROVIDERS.md). No provider or currency is selected by default.

All library data stays in RAM. The caller implements the atomic `Journal` port
and chooses persistence outside this crate. It reserves operation identity and
maximum fee exposure before dispatch, retaining only minimal operational state.
There is no database integration, SQL schema or disk cache in this library.
The volatile `MemoryJournal` is for disposable use and conformance fixtures;
production restart guarantees require a durable caller-owned implementation.
A stable injected key protects context and receipt identities across restarts.

Results normally contain normalized facts and necessary member actions only.
Explicit `Disclosure::Complete` additionally returns complete available upstream
responses in private transient buffers, including extensions. Provider payloads
are untrusted assertions, separate from verified facts. They never enter the
journal, ordinary diagnostics or gate evidence. SDKs/parsers may make temporary
copies; host swap and crash dumps are outside the library's control.

`Payments::paid_access` transforms established financial terms into a requested
level and period. It rechecks the exact instruction and caps validity to the
provider's bound. Amounts, currencies and object IDs remain inside the payment
boundary. Missing/partial amount evidence cannot establish paid access.
`Outcome::evidence` remains the generic minimal in-process gate fact; sandbox
facts cannot create production evidence. Neither API accepts arbitrary JSON
as proof. An active subscription or verified authorization is not collection.

Reconciliation is a separate costed instruction. `Payments::observation` prepares
it from an authenticated parent and fresh operation ID without provider I/O.
Dispatch never retries automatically. Uncertain outcomes retain exposure and
cannot authorize another charge. `forget` removes continuation through the
caller port while preserving duplicate-dispatch protection.

See [the contract](docs/CONTRACT.md), [credentials](docs/CREDENTIALS.md), and
[coverage requirements](docs/COVERAGE.md). Tests use inert local fixtures;
production and provider-sandbox calls are not executed or authorized by this work.

`Payments::run` uses the shared planner for explicit configured or cost-ordered
alternatives. It checks all eligible tasks' bound terms, context and requested
fact before the first dispatch, and stops on success, pending, refusal or uncertain
effects. A provider-confirmed safe failure may still cost money. Unknown costs
remain reserved, including across replays in caller-owned state.

`simulation::simulate` uses the same planner and outcome classification with dummy
responses. `access::simulate_access` applies the same level/period transformation
but returns a distinct non-proof type. Neither accepts a provider or credential.
Real adapters refuse Simulation mode; Sandbox is separate and still costed.

Stripe success payloads preserve available SDK objects, including extensions;
typed Stripe API errors preserve the complete fields exposed by the SDK. Its
transport discards malformed response bytes and unknown error fields before they
reach this adapter, and does not expose response headers. REST adapters preserve
bounded response bytes, status and available headers through private output.
Stripe's maintained SDK buffers its response internally; its limit here is a
request timeout and explicit input bound, not a configurable response byte cap.

The host injects a live `cost::Clock` into `Payments::new` (closures implement the
trait). Dispatch time is refreshed after waits and before each transport phase.
Caller `Journal::begin` receives the effective deadline for its atomic decision.
An unexpected fee remains visible in `cost.violation` and cascade
`charge_violation`; it does not disappear into a smaller original reservation.
Failed final state writes return `commit_error`, Unknown and explicit private
reconciliation data, with no proof or automatic retry.
