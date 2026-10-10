# Agent instructions

Write code comments and documentation in English.

- Implement the adapter and information boundary in `docs/CONTRACT.md`.
  Callers own payment policy; all provider input is transient except explicitly
  necessary operational references. No custody or member-to-member transfers.
- `crpt` reuses this contract. Do not add a closed currency/platform registry.
- Reuse maintained libraries; semver ranges and first-party `branch = "main"`.
  Lockfiles record tested dependency resolution; no exact manifest pins.
- LGPL-3.0-only WITH LGPL-3.0-linking-exception. No GPL/AGPL-only dependencies.
- Fixtures only: no live billing, registrations, spending or outreach.
- Workstations edit and format; Crow runs compilation, tests and coverage.
  Candidate commits enable remote validation; never claim completion until
  required checks pass. Forgejo is primary. No registry publication.
- Test context binding, expiry, retries, replay protection, ambiguous outcomes,
  persistent journals and redacted outputs. Preserve the coverage gate.
