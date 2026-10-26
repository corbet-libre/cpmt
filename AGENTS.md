# cpmt agent instructions

Write code comments and documentation in English. Owned code forbids unsafe
Rust (`#![forbid(unsafe_code)]`, `src/lib.rs:6`).

## Build and test (lane: `~/agents/scratch/cbuild <lane> ...`)

- `fmt -p cpmt -- --check`
- `test -p cpmt --all-features`
  (lib 70 + doc 0, exit 0; `--locked` needs CI's resolve refresh)
- `clippy -p cpmt --all-features --all-targets -- -D warnings`
- `check -p cpmt --target wasm32-unknown-unknown`
- wasm exec: `test -p cpmt --target wasm32-unknown-unknown --lib` with
  `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner`
  (wasm subset 5, headless Chrome, exit 0)
- CI-only (Crow `.ci/verify.sh`): `cargo deny check advisories`,
  provider-regression mutations (`.ci/test-provider-regressions.py`),
  nightly `llvm-cov` gate (`docs/COVERAGE.md`)

## Hard invariants (each with its enforcing test)

- Every dispatch needs a caller-owned atomic reservation and a cost permit;
  unknown costs never mean free (`docs/CONTRACT.md:35-40`):
  `payments::tests::actual_cost_never_exceeds_reserved_units_and_fallback_keeps_uncertainty`,
  `run::tests::every_alternative_is_checked_before_any_charge`.
- Uncertain outcomes never authorize another charge and never trigger fallback
  (`docs/CONTRACT.md:44-45,107-110`):
  `run::tests::ambiguous_pending_refused_expired_and_domain_failures_never_fallback`,
  `payments::tests::uncertain_execution_never_becomes_a_second_charge`.
- Provider payloads are untrusted assertions, never proofs; sandbox facts never
  mint production evidence (`docs/CONTRACT.md:53-55,74-76`):
  `stripe::tests::provider_data_is_reduced_and_unrecognized_statuses_never_prove_payment`,
  `access::tests::simulation_uses_the_real_level_interval_transformation_but_produces_no_evidence`.
- Paid access needs an outcome matching the exact instruction plus a
  provider-established condition (`docs/CONTRACT.md:65-70`):
  `tests::roundtrip_and_expiry`,
  `providers::tests::reduction_checks_identity_parent_account_and_discards_private_fields`.
- Results are RAM-only; diagnostics redact; owned byte buffers zeroize on drop
  (`docs/CONTRACT.md:57-61`, `src/cost.rs`, `src/payments/journal.rs`):
  `payments::tests::raw_input_can_be_dropped_before_reconciliation_and_forgetting`.
- Simulation, sandbox and production are distinct; no spending or live calls
  (`src/tests/support.rs` fixtures).
- LGPL-3.0-only WITH linking exception; no GPL/AGPL-only code (`LICENSE.md`).

Pointers: `docs/CONTRACT.md`, `docs/COVERAGE.md`, `docs/CATALOGUE.md`,
`docs/PROVIDERS.md`, `docs/CREDENTIALS.md`.
