#!/usr/bin/env bash
# Build the agents that run on the remote Vast VM once, so provisioning can
# upload binaries instead of installing Rust and compiling on every instance.
#
# Run inside an ubuntu:22.04 container (oldest supported VM release), so the
# binaries only need glibc 2.35 and run on Ubuntu 22.04+ and Debian trixie.
# Output: src-tauri/vm-agents/x86_64-unknown-linux-gnu/ with a SHA256SUMS file
# that the app verifies before shipping the files to a VM.
set -Eeuo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TARGET="x86_64-unknown-linux-gnu"
OUT="$ROOT/src-tauri/vm-agents/$TARGET"

if [[ "$(uname -m)" != "x86_64" ]]; then
  echo "build-vm-agents.sh must run on x86_64" >&2
  exit 1
fi

if [[ "${NOLAND_SKIP_APT:-0}" != "1" ]]; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends \
    build-essential ca-certificates clang curl git libelf-dev llvm pkg-config zlib1g-dev \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
fi

export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export PATH="$CARGO_HOME/bin:$PATH"
if ! command -v rustup >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
fi

# Each workspace pins its toolchain in rust-toolchain.toml (network-agent
# uses stable), exactly as the on-VM source build does.
(cd "$ROOT/state-agent" && cargo build --release --locked \
  -p noland-state-agent -p noland-lifecycle-agent)
(cd "$ROOT/network-agent" && cargo build --release --locked)
(cd "$ROOT/vm-cloud-mic-agent" && cargo build --release --locked)

bpf_object="$(find "$ROOT/state-agent/target/release/build" \
  -path '*/out/noland_observer.bpf.o' -type f -printf '%T@ %p\n' \
  | sort -nr | head -n 1 | cut -d' ' -f2-)"
if [[ -z "$bpf_object" ]]; then
  echo "noland_observer.bpf.o was not produced" >&2
  exit 1
fi

rm -rf "$OUT"
install -d "$OUT"
install -m 0755 "$ROOT/state-agent/target/release/noland-state-agent" "$OUT/"
install -m 0755 "$ROOT/state-agent/target/release/noland-lifecycle-agent" "$OUT/"
install -m 0644 "$bpf_object" "$OUT/noland_observer.bpf.o"
install -m 0755 "$ROOT/network-agent/target/release/noland-network-agent" "$OUT/"
install -m 0755 "$ROOT/vm-cloud-mic-agent/target/release/noland-mic-receiver" "$OUT/"
strip "$OUT"/noland-state-agent "$OUT"/noland-lifecycle-agent \
  "$OUT"/noland-network-agent "$OUT"/noland-mic-receiver

# Guard against building on a newer distro by mistake: every supported VM
# release must be able to load these binaries.
MAX_GLIBC="${NOLAND_VM_AGENTS_MAX_GLIBC:-2.35}"
for binary in "$OUT"/noland-*; do
  needed="$(objdump -T "$binary" | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' | sed 's/GLIBC_//' | sort -Vu | tail -n 1)"
  if [[ -n "$needed" && "$(printf '%s\n%s\n' "$needed" "$MAX_GLIBC" | sort -V | tail -n 1)" != "$MAX_GLIBC" ]]; then
    echo "$(basename "$binary") needs glibc $needed, newer than the $MAX_GLIBC supported VMs ship" >&2
    exit 1
  fi
done

(cd "$OUT" && sha256sum -- * > SHA256SUMS)
cat "$OUT/SHA256SUMS"
ls -l "$OUT"
