# Noland Connect Documentation

This folder is the source of truth for how Noland Connect works end-to-end.

## Start Here

- `docs/architecture.md`: system layout, module boundaries, and runtime model
- `docs/flows.md`: onboarding, provisioning, post-WireGuard setup, reboot, lifecycle, backup, mic
- `docs/shared-storage-high-level.md`: high-level shared storage flow, diagrams, and tools/components used
- `docs/schemas.md`: persisted state schema, event schema, and key data contracts
- `docs/api-reference.md`: frontend/backend command surface and grouping
- `docs/configuration.md`: environment variables, defaults, and tuning knobs
- `docs/operations.md`: build/release workflows, release artifacts, and operational runbook
- `docs/providers.md`: GPU provider abstraction, TensorDock integration, and why RunPod is not supported
- `docs/stream-quality-history.md`: per-session quality records and how they shape offer ranking
- `docs/spend-tracking.md`: local spend ledger, monthly budget, alerts, and budget auto-stop
- `docs/automatic-backup-shutdown.md`: remote lifecycle architecture, safety rules, deployment paths, and E2E procedure
- `docs/noland-connect-system.md`: full production-grade deep system documentation

## Notes and plans

- `docs/PROVISIONING_STEPS.md`: provisioning checklist notes
- `docs/KVM_GOLDEN_IMAGE_SETUP.md`: VM image preparation notes
- `docs/plans/`: shared storage optimization and restore implementation plans
- `docs/networking/windows-adapter-troubleshooting.md`: clearing stale Windows WireGuard adapter state
- `docs/examples/sample-state.json`: example persisted app state

## Related project docs outside this folder

- `README.md`: repo setup, stack, and top-level project overview

If a behavior changes in code, update the relevant file in this folder in the same PR.
