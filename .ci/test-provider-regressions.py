#!/usr/bin/env python3
"""Require actual regression assertions against each unsafe provider projection."""
from pathlib import Path
import re
import subprocess

path = Path('src/providers/projection.rs')
original = path.read_bytes()
controls = [
    ('coingate_late_settlement_blocks_fallback_and_reconciles_without_another_charge',
     '"expired" | "invalid" | "canceled" => State::Unknown,',
     '"expired" | "invalid" | "canceled" => State::Failed,'),
    ('gocardless_automatic_retry_blocks_fallback_until_read_only_reconciliation',
     '                (false, "failed") if value["retry_if_possible"] != false => State::Unknown,\n',
     ''),
    ('square_hold_evidence_requires_actual_bank_authorization_and_keeps_reconciliation',
     'if matches!(self, Self::Square { .. }) && state == State::Authorized {',
     'if false {'),
]
try:
    for name, before, after in controls:
        source = original.decode()
        if source.count(before) != 1:
            raise SystemExit(f'provider control source drift: {name}')
        path.write_text(source.replace(before, after))
        test = f'providers::tests::safety::{name}'
        result = subprocess.run(
            ['cargo', 'test', '--locked', '--all-features', '--lib', test,
             '--', '--exact', '--nocapture'],
            capture_output=True, text=True,
        )
        output = result.stdout + result.stderr
        # A compile error, empty selection, or a different failed test proves nothing.
        if (result.returncode != 101 or 'test result: FAILED. 0 passed; 1 failed;' not in output
                or 'assertion' not in output or 'error[E' in output
                or not re.search(r'^test ' + re.escape(test) + r' \.\.\. FAILED$', output, re.M)):
            print(output)
            raise SystemExit(f'provider control did not fail its assertion: {name}')
        print(f'Unsafe provider projection rejected by assertion: {name}')
        path.write_bytes(original)
finally:
    path.write_bytes(original)
