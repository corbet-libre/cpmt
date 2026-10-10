#!/usr/bin/env bash
# Execute the manifest's selected checks with content-keyed result reuse.
set -euo pipefail
exec 2>&1
: "${CCID_BIN:?Missing verified ccid job runtime}"
: "${CI_COMMIT_SHA:?Missing verified source revision}"
: "${CI_REPOSITORY_URL:?Missing canonical repository identity}"
: "${CHECKS:?Missing declared check selection}"
if [ "$CHECKS" != "verify" ]; then
  echo "CHECKS must be exactly 'verify', got: $CHECKS" >&2
  exit 2
fi
test -f Cargo.lock || { echo 'Cargo.lock must be present (committed snapshot)' >&2; exit 2; }
cache_root=${CI_CACHE_ROOT:-${CARGO_HOME:-}}
[[ $cache_root = /* ]] || { echo 'An absolute persistent CI_CACHE_ROOT or CARGO_HOME is required' >&2; exit 2; }
namespace=$(printf '%s' "$CI_REPOSITORY_URL" | sha256sum | cut -c1-24)
state="$cache_root/ccid-cached/$namespace"
toolchains=${CI_TOOLCHAIN_ROOT:-$cache_root/ccid-cached/toolchains}
[[ $toolchains = /* ]] || { echo 'CI_TOOLCHAIN_ROOT must be absolute' >&2; exit 2; }
mkdir -p "$state" "$toolchains"
exec 9>"$state/lock"
flock -w "${CI_TIMEOUT:-2700}" 9
mkdir -p "$state/moon-cache/hashes" "$state/moon-cache/outputs" "$state/moon-home"
mkdir -p .moon/cache
tool_source=github:NixOS/nixpkgs/b7c2ada94fe99c15b0dbcf4d11fd7850b957a436
tool_paths=$(nix build --no-link --print-out-paths "$tool_source#moon" "$tool_source#proto" "$tool_source#python3" "$tool_source#nodejs_24" "$tool_source#curl" "$tool_source#cargo-llvm-cov" "$tool_source#cargo-deny")
for path in $tool_paths; do PATH="$path/bin:$PATH"; done
export PROTO_HOME="$toolchains/proto" RUSTUP_HOME="$toolchains/rustup"
export CARGO_HOME="$toolchains/cargo" MOON_HOME="$state/moon-home"
export PROTO_AUTO_CLEAN=false PROTO_TELEMETRY=false PROTO_YES=true
export RUSTUP_INIT_SKIP_PATH_CHECK=yes RUSTUP_TOOLCHAIN=stable
export PATH="$(dirname "$CCID_BIN"):$CARGO_HOME/bin:$PROTO_HOME/shims:$PROTO_HOME/bin:$PATH"
export CI_JOBS="${CI_JOBS:-2}" CARGO_BUILD_JOBS="${CI_JOBS:-2}"
export RUST_TEST_THREADS="${CI_TEST_THREADS:-2}"
export CARGO_NET_GIT_FETCH_WITH_CLI=true
# Append to any git config the verified runtime already supplies through the environment.
git_config_base=${GIT_CONFIG_COUNT:-0}
export GIT_CONFIG_COUNT=$((git_config_base + 3))
export "GIT_CONFIG_KEY_$((git_config_base + 0))=url.https://git.corbet.ch/corbet-libs/.insteadOf"
export "GIT_CONFIG_VALUE_$((git_config_base + 0))=https://github.com/corbet-libs/"
export "GIT_CONFIG_KEY_$((git_config_base + 1))=url.https://git.corbet.ch/corbet-foss/.insteadOf"
export "GIT_CONFIG_VALUE_$((git_config_base + 1))=https://github.com/corbet-foss/"
export "GIT_CONFIG_KEY_$((git_config_base + 2))=url.https://git.corbet.ch/cmtymeet/.insteadOf"
export "GIT_CONFIG_VALUE_$((git_config_base + 2))=https://github.com/cmtymeet/"
# Shared-first toolchain lock: production base helper verbatim.
TOOLCHAIN_LOCK_FILE="$toolchains/install.lock"
TOOLCHAIN_LOCK_TIMEOUT="${CI_TIMEOUT:-2700}"
TOOLCHAIN_WANT_NIGHTLY=1
TOOLCHAIN_WASM_TARGETS="stable:wasm32-unknown-unknown"
# shellcheck source=toolchain-lock.sh
source "$(dirname "${BASH_SOURCE[0]}")/toolchain-lock.sh"
toolchain_ensure
cargo_bin=$("$CARGO_HOME/bin/rustup" which --toolchain stable cargo)
test -x "$cargo_bin"
export PATH="$(dirname "$cargo_bin"):$PATH"
export CARGO_INSTALL_ROOT="$state/install-bin"
mkdir -p "$CARGO_INSTALL_ROOT/bin"
export PATH="$CARGO_INSTALL_ROOT/bin:$PATH"
export ARTIFACT_ROOT="${ARTIFACT_ROOT:-$state/release-artifacts}"
mkdir -p "$ARTIFACT_ROOT"
[[ "$ARTIFACT_ROOT" = /* ]] || { echo "ARTIFACT_ROOT must be absolute" >&2; exit 2; }
export SOURCE_SHA256="$(git archive --format=tar HEAD | sha256sum | cut -d ' ' -f 1)"
export CCID_SOURCE_REVISION="$CI_COMMIT_SHA"
"$CCID_BIN" render --repo "$PWD" --check
proto --version
proto_version="$(proto --version)"
grep -q '0\.59\.0' <<<"$proto_version" || { echo "proto drifted: $proto_version, want 0.59.0" >&2; exit 2; }
moon --version
moon_version="$(moon --version)"
grep -q '2\.4\.6' <<<"$moon_version" || { echo "moon drifted: $moon_version, want 2.4.6" >&2; exit 2; }
rustc -vV
cargo --version
python3 --version
node --version
curl --version
rustup run nightly rustc -vV
rustup run nightly cargo llvm-cov --version
"$CCID_BIN" cached --repo "$PWD" --check verify
test -s ".ccid/results/verify.jsonl"
