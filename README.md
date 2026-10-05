<div align="center">

# No Land

### Turn on-demand GPU cloud machines into personal remote gaming PCs.

[![Rust](https://img.shields.io/badge/Rust-Tauri-000000?logo=rust)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)](https://tauri.app/)
[![React](https://img.shields.io/badge/React-TypeScript-20232a?logo=react)](https://react.dev/)
[![Linux](https://img.shields.io/badge/Linux-Ubuntu-FCC624?logo=linux&logoColor=black)](https://ubuntu.com/)
[![WireGuard](https://img.shields.io/badge/WireGuard-Networking-88171A?logo=wireguard)](https://www.wireguard.com/)

**GPU orchestration · automated provisioning · low-latency streaming · networking · remote storage**

[Website](https://no-land.net) · [Architecture](docs/architecture.md) · [Flows](docs/flows.md) · [Configuration](docs/configuration.md) · [Discord](https://discord.gg/yafxvA6EBR)

</div>

---

## Why No Land exists

Cloud gaming solves the cost of owning high-end hardware, but many services still restrict users to a predefined game catalog.

No Land explores a different model: rent a real GPU-powered Linux machine, provision it automatically, connect it securely, and use it as your own remote gaming PC.

The project focuses less on game catalog management and more on the systems underneath remote computing: **GPU infrastructure, Linux provisioning, networking, streaming, audio, storage, and client orchestration**.

## What it does

From one desktop app, No Land can:

1. authenticate locally and connect to a user's Vast.ai account;
2. discover and rank GPU offers by location, reliability, price, storage, and VRAM;
3. create or reuse a rented GPU instance;
4. wait for the machine to become SSH-ready;
5. configure NVIDIA headless graphics, Sunshine, audio, WireGuard, and supporting services;
6. patch the local Moonlight configuration and guide pairing;
7. persist provisioning checkpoints so completed steps can be safely skipped on retry;
8. expose a native streaming path built around Moonlight/GameStream components.
9. rent from Vast.ai, TensorDock or Shadeform, with offers from every provider ranked together;
10. track estimated spend against a monthly budget, with warnings and optional auto-stop;
11. save server + stream setups and start a matching server in one click;
12. watch prices, flag preempted interruptible instances, and learn from each session's stream quality to rank future offers.

## System flow

```text
Desktop app
   │
   ▼
Vast.ai offer discovery
   │
   ▼
GPU instance creation / reuse
   │
   ▼
SSH provisioning orchestrator
   ├── NVIDIA / display
   ├── Sunshine
   ├── PipeWire / WirePlumber
   ├── WireGuard
   └── runtime validation
   │
   ▼
Moonlight pairing + native client
   │
   ▼
Low-latency remote gaming session
```

## Engineering highlights

- **Rust orchestration layer** — Tauri backend built with `tokio`, `reqwest`, `serde`, and structured tracing.
- **Cloud offer ranking** — selects machines using location, reliability, price, storage, and VRAM signals.
- **Automated SSH provisioning** — turns a generic rented GPU machine into a usable remote gaming environment.
- **Idempotent recovery** — stores per-server provisioning checkpoints and resumes only incomplete steps.
- **Secure networking** — integrates WireGuard and an embedded userspace tunnel path.
- **Low-latency Linux audio** — configures PipeWire/WirePlumber profiles for Sunshine streaming and includes fallback profiles for underruns/crackling.
- **Native streaming work** — documents frame pipelines, queues, timing domains, packet sizing, reconnect behaviour, and latency optimization experiments.
- **Cross-platform release pipeline** — GitHub Actions builds, signs, and scans desktop binaries for macOS, Windows, and Linux on both x64 and arm64.

## Stack

| Area | Technology |
| --- | --- |
| Desktop | Tauri 2, Rust |
| UI | React, TypeScript, Vite, Tailwind CSS, Zustand |
| Async / HTTP | Tokio, Reqwest |
| Cloud compute | Vast.ai GPU instances |
| Streaming | Sunshine, Moonlight, moonlight-common-c |
| Networking | WireGuard, GotaTun, UDP |
| Media | GStreamer, PipeWire, WirePlumber |
| Platform | Linux / NVIDIA |
| CI/CD | GitHub Actions |

## Architecture and documentation

Project documentation lives in `docs/`:

- [`docs/README.md`](docs/README.md) — documentation entry point
- [`docs/architecture.md`](docs/architecture.md) — high-level system architecture
- [`docs/flows.md`](docs/flows.md) — user and provisioning flows
- [`docs/schemas.md`](docs/schemas.md) — persisted/runtime data shapes
- [`docs/api-reference.md`](docs/api-reference.md) — API notes
- [`docs/configuration.md`](docs/configuration.md) — runtime configuration
- [`docs/operations.md`](docs/operations.md) — operational guidance
- [`docs/providers.md`](docs/providers.md) — GPU provider abstraction, TensorDock and Shadeform
- [`docs/spend-tracking.md`](docs/spend-tracking.md) — spend ledger and monthly budget
- [`docs/stream-quality-history.md`](docs/stream-quality-history.md) — session quality history and offer ranking

### Streaming implementation notes

- [`docs/moonlight-client-pipeline.md`](docs/moonlight-client-pipeline.md) — native frame pipeline, queues, timing domains, and ownership map
- [`docs/moonlight-client-optimizations.md`](docs/moonlight-client-optimizations.md) — latency feature flags, source precedents, platform limits, and validation matrix
- [`docs/moonlight-adaptive-packet-size.md`](docs/moonlight-adaptive-packet-size.md) — adaptive GameStream packet sizing, path hints, cache, scoring, and controlled reconnect

## Project layout

```text
src/                      React + TypeScript desktop UI
  features/               onboarding, dashboard, servers, provisioning, settings,
                          launch-library, moonlight, shared-storage, ...
  store/                  Zustand state
src-tauri/                Tauri 2 / Rust desktop backend
  src/commands/           Tauri commands exposed to the UI
  src/services/           provisioning, Vast.ai, SSH, Sunshine, Moonlight,
                          WireGuard, NVIDIA headless, shared storage, ...
  src/moonlight/          native streaming client integration
network-agent/            network quality probe and telemetry agent
network-contracts/        versioned network control/state/probe contracts
state-agent/              app-state tracking for the disposable VM (shared storage)
mic-sidecar/              desktop microphone media sidecar
vm-cloud-mic-agent/       PipeWire virtual microphone source on the cloud VM
scripts/                  build, packaging, and release helpers
docs/                     architecture, flows, and operations docs
```

## Run locally

Install dependencies:

```bash
npm install
```

Frontend development:

```bash
npm run dev
```

Full desktop app:

```bash
npm run tauri:dev
```

Production build:

```bash
npm run tauri:build
```

Tests:

```bash
npm test                 # frontend (Vitest)
npm run test:unit        # network-contracts and network-agent
cargo test --manifest-path src-tauri/Cargo.toml
```

## Desktop releases

Releases are produced by GitHub Actions (`.github/workflows/release.yml`) on every push to `main`:

1. validation, security scanning (CodeQL, Trivy, Gitleaks, dependency audits), and the full test suite;
2. the next version is computed by bumping the patch number of the latest `vX.Y.Z` tag (or the version in `src-tauri/tauri.conf.json` if that is newer) — tags that are not plain `vX.Y.Z` are ignored;
3. signed builds for six targets: macOS, Windows, and Linux, each on x64 and arm64;
4. package validation: every platform artifact must be present, packaged filesystems are scanned for high/critical vulnerabilities, and checksums plus a CycloneDX SBOM are generated;
5. the version tag is created and a GitHub release is published with the validated assets.

Artifacts include `.dmg` / app archives on macOS, installers on Windows, and AppImage / `.deb` packages on Linux.

A nightly workflow (`nightly.yml`) runs the same pipeline as a rehearsal without tagging or publishing anything. Pull requests run `ci.yml`.

Release builds always require `TAURI_SIGNING_PRIVATE_KEY` for updater signatures. Apple Developer ID signing/notarization and Azure Authenticode signing are used when their secrets are configured; without them macOS builds are ad-hoc signed and Windows installers are unsigned, and the privileged network helper is then only trusted when it is unsigned in the same way as the app.

## Provisioning state and recovery

No Land persists runtime state through a `StateStore` abstraction. Provisioned servers keep step-level completion state plus the runtime artifacts needed to resume safely.

That allows a failed or restarted provisioning run to continue from the remaining steps rather than rebuilding the machine from scratch.

## Low-latency audio

During host provisioning, No Land configures PipeWire and WirePlumber for remote streaming and validates the result with system-level tooling.

The provisioning logic can apply multiple fallback profiles when aggressive low-latency settings cause underruns on a particular machine.

## Upstream projects

No Land builds on excellent open-source work including:

- [Sunshine](https://github.com/LizardByte/Sunshine)
- [Moonlight Qt](https://github.com/moonlight-stream/moonlight-qt)
- [moonlight-common-c](https://github.com/moonlight-stream/moonlight-common-c)
- [WireGuard](https://github.com/WireGuard/wireguard-tools)
- [GotaTun](https://github.com/mullvad/gotatun)
- [GStreamer](https://github.com/GStreamer/gstreamer)
- [PipeWire](https://github.com/PipeWire/pipewire)
- [WirePlumber](https://github.com/PipeWire/wireplumber)

Vast.ai is the current cloud-provider integration used for GPU instance discovery and provisioning.

## Current focus

The project is actively evolving around:

- resilience on unstable networks;
- streaming latency and packet delivery;
- adaptive networking and reconnect behaviour;
- cross-platform tunnel integration;
- stronger provisioning recovery;
- remote application restore and shared storage workflows.

---

<div align="center">

**Cloud gaming without giving up the freedom of a real PC.**

</div>
