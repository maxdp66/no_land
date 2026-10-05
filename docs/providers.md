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
   100 GB). Sold-out GPUs are skipped. GPUs with a dedicated IP are deployed
   with `useDedicatedIp: true`; GPUs without one but with
   `port_forwarding_available` are offered as "(port-forwarded)" (and count
   as no static IP) and deployed with `port_forwards` for SSH (22),
   WireGuard (51820) and the network probe (6201), each with
   `external_port: 0` so TensorDock assigns the public port. GPUs with
   neither are skipped. Streaming runs inside the WireGuard tunnel, so no
   other ports are needed.
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
   instance type and region with an available `on_demand` entry (types that
   offer spot list each region again as `spot`; those are ignored). Prices are
   in cents per hour. Offers are skipped when the type's `deployment_type` is
   `container`, when the GPU has no NVENC encoder (A100, H100, H200, GH200,
   B200, AMD and Gaudi), when the type has more than one GPU, or when the
   underlying cloud is a container platform or one the app integrates
   directly (RunPod, Vast.ai, TensorDock). Shadeform reports no coordinates,
   so offers use their country's centroid for distance ranking.
2. The managed SSH key is added to the Shadeform account once
   (`POST /sshkeys/add`) and reused by id for every `POST /instances/create`,
   which sends `shade_cloud: true` and `rental_type: on_demand` and picks an
   Ubuntu 24.04 or 22.04 image (newest CUDA) when the type offers one.
3. Once SSH answers, the orchestrator logs in as the instance's `ssh_user`
   (usually `shadeform`) and runs the same access bootstrap as TensorDock.
4. Provisioning then continues exactly as for Vast.

Shadeform cannot stop instances. Stopping a Shadeform server from the app
returns an error asking you to destroy it instead, and the lifecycle agent
only accepts the destroy action for it. Credentials:
`credentials.shadeformApiKey`, stored in the OS keyring and verified in
Settings → Profile before saving.

The request and response shapes were checked against Shadeform's official
OpenAPI spec (`openapi.yaml` in github.com/shadeform/docs) but not against a
live account. Shadeform applies no firewall rules of its own; its images ship
UFW, which provisioning configures. When a cloud puts the instance behind NAT
it reports `port_mappings`; then WireGuard (51820) and the network probe
(6201) are only used if they are mapped.

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

The TensorDock client was checked against TensorDock's official v2 API docs
(dashboard.tensordock.com/api/docs: locations, instance creation and instance
management). The locations response, the create request (including `gpus` as
an object keyed by model) and the instance shapes match; the parsers also
accept the flat `GET /api/v2/instances/{id}` response the docs show. It has
not been exercised against a live account.

`GET /api/v2/locations` is public. When it returns an empty `locations`
list, TensorDock has no location-based capacity at that moment and the app
correctly shows no TensorDock offers. Hostnode-based deployment
(`GET /api/v2/hostnodes`, authenticated) is not used yet; it needs explicit
port forwards instead of a dedicated IP.

Still unconfirmed because the docs do not say:

- whether `port_forwards` carries UDP. The request has no protocol field.
  If WireGuard's port is not forwarded for UDP, provisioning stops at the
  WireGuard step (no handshake), and dedicated-IP offers should be used;

- whether `rateHourly` on a stopped instance reports the storage charge.
  TensorDock's help site (docs.tensordock.com, Spot Instances) says storage
  is billed at the standard rate even when a workload is not running, but
  the spend tracker assumes TensorDock storage cost is `0` because the
  instance payload has no separate storage price.

Confirmed by docs.tensordock.com: the default login user on Ubuntu images
is `user` (`ssh user@ip`, matching `TENSORDOCK_DEFAULT_SSH_USER`), and
Linux VMs start with all ports open and UFW disabled; provisioning enables
UFW with rules for SSH, WireGuard and the network probe.
