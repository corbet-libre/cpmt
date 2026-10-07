#!/usr/bin/env bash
set -euo pipefail
exec 2>&1
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-2}"
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
: "${CI_COMMIT_SHA:?verify requires the exact source revision}"
: "${ARTIFACT_ROOT:?verify requires an absolute ARTIFACT_ROOT}"
[[ "$ARTIFACT_ROOT" = /* ]] || { echo "ARTIFACT_ROOT must be absolute" >&2; exit 2; }
export RUNNER_TEMP="${RUNNER_TEMP:-$(mktemp -d)}"
export GITHUB_PATH="${GITHUB_PATH:-$RUNNER_TEMP/github_path}"
touch "$GITHUB_PATH"
python3 .github/check-first-party.py
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo check --locked --target wasm32-unknown-unknown
version=$(python3 -c "import tomllib; print(next(p['version'] for p in tomllib.load(open('Cargo.lock','rb'))['package'] if p['name']=='wasm-bindgen'))")
archive="wasm-bindgen-${version}-x86_64-unknown-linux-musl.tar.gz"
base="https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${version}"
mkdir -p "$RUNNER_TEMP/cpmt-wasm-runner"
(cd "$RUNNER_TEMP/cpmt-wasm-runner" && curl --user-agent dependency-ci --fail --location --retry 3 --remote-name "$base/$archive" && curl --user-agent dependency-ci --fail --location --retry 3 --remote-name "$base/$archive.sha256sum" && checksum=$(cut -d ' ' -f1 "$archive.sha256sum") && printf '%s  %s\n' "$checksum" "$archive" | sha256sum --check && tar -xzf "$archive" --strip-components=1)
export PATH="$RUNNER_TEMP/cpmt-wasm-runner:$PATH"
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
cargo test --locked --target wasm32-unknown-unknown --lib
rustup run nightly rustc --version
NIGHTLY_CARGO="$(rustup which --toolchain nightly cargo)"
NIGHTLY_RUSTC="$(rustup which --toolchain nightly rustc)"
export RUSTC="$NIGHTLY_RUSTC" CARGO="$NIGHTLY_CARGO" RUSTUP_TOOLCHAIN=nightly
export PATH="$(dirname "$NIGHTLY_RUSTC"):$PATH"
"$NIGHTLY_CARGO" llvm-cov --version
# Bug 2 isolation: coverage data must not reuse the shared cargo target
# across jobs (stale checkout paths fail the next gate); scope the
# coverage target to this job only. Native gates keep the shared warm
# target; outer traps and services are untouched.
(
coverage_target="$(mktemp -d)"
case "$coverage_target" in ""|"$PWD"/*) echo 'unsafe coverage target' >&2; exit 2;; esac
trap 'rm -rf -- "$coverage_target"' EXIT
export CARGO_TARGET_DIR="$coverage_target"
"$NIGHTLY_CARGO" llvm-cov --locked --workspace --all-features --branch --no-cfg-coverage --no-cfg-coverage-nightly --ignore-filename-regex '/tests/' --json --output-path coverage.json
"$NIGHTLY_CARGO" llvm-cov report --locked --branch --ignore-filename-regex '/tests/' --lcov --output-path coverage.lcov
"$NIGHTLY_CARGO" llvm-cov report --locked --branch --ignore-filename-regex '/tests/' --text --output-path coverage.txt
)
python3 .github/test-source-coverage.py
python3 .github/check-source-coverage.py coverage.lcov coverage.json coverage.txt
