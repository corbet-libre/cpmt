# Agent instructions

Write code comments and documentation in English.

- Implement the adapter and information boundary in `docs/CONTRACT.md`.
  Callers own payment policy; all provider input/results are RAM-only; caller-owned atomic state receives
  only explicitly necessary operational references and fee reservations. No custody or member-to-member transfers.
- `crpt` reuses this contract. Do not add a closed currency/platform registry.
- Reuse maintained libraries; semver ranges and first-party `branch = "main"`.
  Lockfiles record tested dependency resolution; no exact manifest pins.
- LGPL-3.0-only WITH LGPL-3.0-linking-exception. No GPL/AGPL-only dependencies.
- Fixtures only: no live billing, registrations, spending or outreach.
- Workstations edit and format; Crow runs compilation, tests and coverage.
  Candidate commits enable remote validation; never claim completion until
  required checks pass. Forgejo is primary. No registry publication.
- Test context binding, expiry, retries, replay protection, ambiguous outcomes,
  caller-owned atomic state, cost authorization and redacted outputs.
- Explicit callers may request complete transient upstream results; cvld gets only
  requested minimal facts. No direct database/SQL/cache implementation in this crate.
- Simulation, sandbox and production are distinct. No spending or live calls.
- Owned Rust code uses #![forbid(unsafe_code)]. Preserve the coverage gate.
