# Prebuilt VM agents

Release builds place CI-built Linux binaries for the agents that run on the
remote VM here (`x86_64-unknown-linux-gnu/` plus a `SHA256SUMS` manifest),
produced by `scripts/ci/build-vm-agents.sh`. The app verifies the manifest and
uploads these binaries during provisioning instead of compiling the agents on
every new instance.

Development builds ship no binaries, and the VM falls back to building each
agent from the bundled source. To test the prebuilt path locally, run the
build script (on x86_64 Linux) or point `NOLAND_VM_AGENTS_DIR` at a directory
containing the binaries and `SHA256SUMS`.
