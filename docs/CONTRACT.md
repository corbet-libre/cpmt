# Payment domain contract

The application supplies authenticated scope/member/action, commercial terms,
requested meaning, account configuration, credentials, time and cost limits.
The domain verifies and transforms payment facts; providers own their protocol
and financial behavior. Provider installation does not activate an offer.

## RAM and caller state

All request, response, credential and session buffers remain in memory. No direct
database, SQL implementation, filesystem cache or hidden SDK persistence belongs
here. `Journal` is a caller-owned atomic state port, not a way to store raw data.
Its minimal record contains opaque operation/context identity, request/receipt
digests, state, mode, fee reservation and necessary continuation. Host persistence,
backups, encryption and retention are outside this library.

`begin` must reserve the operation ID and maximum fee exposure atomically under
the host's configured budget. Repeated IDs return the prior record without a
second reservation. `replace` uses compare-and-swap and atomically reserves receipt
ownership. Pending/uncertain operations retain maximum exposure. A lost final
write is not success. A stable injected key prevents reopening with another key.
`forget` erases continuation without erasing replay markers and refuses unfinished
operations. Losing caller state or its key destroys restart guarantees; never
silently initialize new state to retry an uncertain payment.

## Cost and modes

`preview` accepts a verified provider quote or explicitly configured tariff with
provenance and expiry. It performs no provider I/O. Its binding covers the random
operation ID, account instance, actual mode, action, context, parent, deadline,
commercial terms and evidence. The shared `cfbk` planner owns ordering and budget
arithmetic. Unknown/unbounded costs cannot create a permit. No implicit exchange
rate, floating-point money or assumption that sandbox is free is permitted.

Every dispatch requires a permit. Adapters receive `cost::Dispatch`, check it
against their actual account instance and mode, and make only the declared calls.
Low-level adapter users must perform the same caller-owned atomic reservation
before constructing a Dispatch. The complete action tariff must cover all its
calls, including x402 discovery and verification before settlement. Observations
also use fresh reserved operation IDs and explicit cost authorization.

Actual fees are reported only when established, separately from payment principal.
Missing actual fees remain unknown; the maximum stays reserved. Pending and
uncertain results stop fallback. A provider failure can still incur fees. No
operation, quote request, provider activation or credential acquisition is implied
by planning, registration or simulation.

## Rich and minimal results

`Disclosure::Minimal` is the normal gate path. Explicit `Complete` returns available
upstream payloads in private transient buffers. Native extensions and private
provider diagnostics remain distinguishable from authenticated domain facts.
Malformed payloads and unsuccessful HTTP responses may be returned privately;
they cannot mint a proof. Transport errors without available payloads remain
uncertain. SDK omissions are documented rather than reconstructed or invented.

Only necessary member actions go to the originally authenticated member. Complete
provider results and operator credentials are not member handovers. No revealing
Debug, ordinary errors or response logs. Owned byte buffers zeroize on drop;
upstream/parser copies and host swap/dumps prevent promises of perfect erasure.
Replays cannot reconstruct discarded private responses.

## Financial meaning and gates

An instruction carries trusted financial terms. A paid-access projection requires
an outcome matching that exact instruction and a provider-established financial
condition, plus the caller's required fact, level and interval. Partial or unproven
amounts never establish the condition. Provider validity caps the requested end.
The result contains only the requested level/period and opaque bound gate evidence.
There is no built-in price, level hierarchy, subscription rule or release policy.

Generic Evidence checks scope/member/action/provider and exclusive expiry.
Community gate consumers atomically spend its stable receipt claim; global gate
consumers accept an in-process fact under their own retry protection. A payment
receipt does not identify a unique person. Sandbox facts cannot mint production
evidence. Raw provider JSON cannot construct Outcome or paid-access proofs.

## Maintained integrations

Adapters advertise implemented operations and enforce explicit input/timeout
limits. REST transports also enforce response byte limits; Stripe's maintained
SDK buffers responses internally and exposes no response-size setting. Observe is read-only and never repeats settlement. Lost create responses
without a reference remain uncertain; new IDs are not safe retries. Parent
references must come from the same authenticated account/context. Custody,
connected-account transfers and platform fee/split instructions are unsupported.

Use maintained SDK/protocol/HTTP/cryptographic libraries, current compatible
ranges and first-party main branches; no exact manifest pins or private forks.
Owned code forbids unsafe Rust. Lockfiles record the CI-tested graph. Fixture
validation is distinct from unexecuted live provider qualification.
