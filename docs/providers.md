# GPU Providers

No Land can rent machines from more than one GPU cloud. Offers from every
configured provider are merged and ranked together in the server picker.

| Provider | Status | Machine type | Notes |
| --- | --- | --- | --- |
| Vast.ai | Supported | KVM VM from the No Land template | Original provider. |
| TensorDock | Supported (new) | KVM VM, `ubuntu2404` image, dedicated IP | See the caveats below. |
| Shadeform | Supported (new) | VM or bare metal from the cloud Shadeform resells, public IP | One API for Lambda, Massed Compute, Hyperstack, DataCrunch and others. See below. |
| RunPod | Not supported | Docker container | Pods expose TCP only, with no inbound UDP, which WireGuard and GameStream need. They also lack systemd and a configurable X server. |

## Architecture

- `src-tauri/src/models/provider.rs` — `CloudProviderKind` and local ids.
  The app keys instances by `u64`. Vast ids are used as-is; foreign string
  ids (TensorDock and Shadeform UUIDs) map to a stable FNV-1a id in
  `[2^52, 2^53)`, which never collides with Vast ids and is exact in JS.
  The mapping is persisted in `state.providerInstanceRefs`.
- `src-tauri/src/services/cloud_provider.rs` — `CloudClient`, the only type
  call sites use. It routes search/create/get/list/stop/destroy and SSH-key
  sync to the right provider. `list_instances` fails if any configured
  provider fails, so reconciliation never deletes local records because of an
  outage; `list_instances_partial` is for display.
- `src-tauri/src/services/tensordock_api.rs` — TensorDock v2 client and
  tolerant parsers (snake/camel case, GPU lists as arrays or maps).
- `src-tauri/src/services/shadeform_api.rs` — Shadeform v1 client.
- `state-agent/crates/noland-lifecycle-agent` — the in-VM auto-shutdown agent
  understands `providerKind: "tensordock"` and `"shadeform"` and calls each
  provider's stop/delete endpoints (Shadeform: delete only, with its
  `X-API-KEY` header). For Vast, `providerKind` and `remoteInstanceId`
  are left out of the JSON so older agents, which reject unknown fields, keep
  working.

## TensorDock flow

1. Offers come from `GET /api/v2/locations`: one per location and GPU model,
   priced for 1 GPU, 8 vCPU, 32 GB RAM and the requested storage (minimum
   100 GB). GPUs that are sold out, or have no dedicated IP, are skipped.
2. `POST /api/v2/instances` with `useDedicatedIp: true` and the managed SSH
   public key.
3. Once SSH answers, the orchestrator logs in as TensorDock's default user
   (`user`) and runs an idempotent bootstrap. It gives the managed key root
   access, sets the desktop user's password to the app password (Vast's
   template does this from `USER`/`PASS`), and allows key-only root login.
4. Provisioning then continues exactly as for Vast.

Credentials: `credentials.tensordockApiKey` is stored in the OS keyring like
the Vast key. Settings → Profile verifies a key before saving it.

## Shadeform flow

1. Offers come from `GET /instances/types?available=true`: one per
   instance type and available region. Prices are in cents per hour. Offers
   are skipped when the GPU has no NVENC encoder (A100, H100, H200, GH200,
   B200, AMD and Gaudi), when the type has more than one GPU, or when the
   underlying cloud is a container platform or one the app integrates
   directly (RunPod, Vast.ai, TensorDock). Shadeform reports no coordinates,
   so offers use their country's centroid for distance ranking.
2. The managed SSH key is added to the Shadeform account once
   (`POST /sshkeys/add`) and reused by id for every `POST /instances/create`,
   which also picks an Ubuntu 24.04 or 22.04 image when the type offers one.
3. Once SSH answers, the orchestrator logs in as the instance's `ssh_user`
   (usually `shadeform`) and runs the same access bootstrap as TensorDock.
4. Provisioning then continues exactly as for Vast.

Shadeform cannot stop instances. Stopping a Shadeform server from the app
returns an error asking you to destroy it instead, and the lifecycle agent
only accepts the destroy action for it. Credentials:
`credentials.shadeformApiKey`, stored in the OS keyring and verified in
Settings → Profile before saving.

Not yet checked against a live account: the request and response shapes
come from SkyPilot's Shadeform provider. Before relying on it, confirm that
the underlying cloud's firewall lets inbound UDP reach WireGuard (51820) and
the network probe (6201); some clouds only open SSH by default.

## Troubleshooting "no offers"

Both TensorDock and Shadeform log one line per search with how many offers
were found and how many were skipped and why (sold out, no dedicated IP,
storage, no price, no NVENC, multi-GPU). When a search finds nothing, the
log line also includes the response's shape (keys and types only, no values)
so a mismatch with the live API is easy to spot.

Offers from providers other than Vast report no bandwidth and may report no
VRAM. Those unknown values no longer fail the minimum bandwidth or VRAM
filters in server preferences; offers are also grouped by ISO country code
in the server picker, which earlier left every TensorDock offer out of the
per-country counts.

## Caveats

TensorDock's documentation site is not reachable from the environment this
integration was built in. The request and response shapes come from
TensorDock's public v2 API description and a third-party OpenAPI profile, and
have not been exercised against a live account. Before relying on it:

- check the `gpus` shape in `create_instance_payload` (map keyed by
  `v0Name`) against a real deployment;
- confirm the default SSH user on the `ubuntu2404` image is `user`
  (`TENSORDOCK_DEFAULT_SSH_USER`);
- confirm stopped instances report `stopped` and that storage keeps billing
  (the spend tracker assumes storage cost is `0` because the instance
  payload does not include it).
