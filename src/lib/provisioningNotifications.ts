import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isNotificationEnabled } from "./notificationPreferences";
import type { OrchestrationState, ProvisioningEvent } from "./types";

/**
 * States that block on the user doing something in the setup modal:
 * - WireGuardConfigGenerated: the user must click "Setup Tunnel".
 * - MoonlightSunshineReadyToSetup: the tunnel is up; the user must click
 *   "Continue" to start streaming setup.
 * - WireGuardWaitingForActivation: tunnel activation/verification failed and
 *   must be retried.
 * - AwaitingPairPin: Moonlight pairing needs a PIN.
 * WireGuardConnected is a success milestone (emitted more than once per run)
 * and is intentionally not included.
 */
const ATTENTION_STATES = new Set<OrchestrationState>([
  "WireGuardConfigGenerated",
  "MoonlightSunshineReadyToSetup",
  "WireGuardWaitingForActivation",
  "AwaitingPairPin",
]);

/** States emitted only while a fresh orchestration run is underway. */
const RUN_START_STATES = new Set<OrchestrationState>([
  "SelectingServer",
  "ServerSelected",
  "GeneratingSshKey",
  "UploadingSshKeyToVast",
  "CreatingInstance",
  "WaitingForInstance",
  "VerifyingReservation",
  "ConnectingSsh",
  "ConfiguringRemote",
  "ConfiguringWireGuard",
  "ConfiguringSunshine",
  "ConfiguringNvidiaHeadless",
]);

const ERROR_REPEAT_WINDOW_MS = 30 * 60 * 1000;
const ERROR_MIN_INTERVAL_MS = 60 * 1000;

let runInstanceKey: string | null = null;
const notifiedRunStates = new Set<string>();
const errorNotifiedAt = new Map<string, number>();
let lastErrorNotificationAt = 0;

function resetRun(instanceKey: string | null) {
  runInstanceKey = instanceKey;
  notifiedRunStates.clear();
}

async function mainWindowFocused(): Promise<boolean> {
  try {
    return await getCurrentWindow().isFocused();
  } catch {
    return typeof document !== "undefined" && document.hasFocus();
  }
}

/**
 * Decide whether a provisioning event deserves an OS notification and send it.
 * Each attention state notifies at most once per provisioning run (keyed by
 * instance), "complete" at most once per run, and identical error messages
 * are rate-limited. Notifications are skipped while the app window is
 * focused, since the setup modal is already in front of the user.
 */
export function handleProvisioningEventNotification(
  event: ProvisioningEvent,
  instanceId: number | null | undefined,
): void {
  const instanceKey = instanceId == null ? null : String(instanceId);
  if (instanceKey !== null && instanceKey !== runInstanceKey) {
    resetRun(instanceKey);
  } else if (RUN_START_STATES.has(event.state) && notifiedRunStates.size > 0) {
    resetRun(instanceKey ?? runInstanceKey);
  }

  if (event.isError) {
    const now = Date.now();
    const errorKey = event.message.trim();
    const previous = errorNotifiedAt.get(errorKey);
    if (
      (previous !== undefined && now - previous < ERROR_REPEAT_WINDOW_MS) ||
      now - lastErrorNotificationAt < ERROR_MIN_INTERVAL_MS
    ) {
      return;
    }
    errorNotifiedAt.set(errorKey, now);
    lastErrorNotificationAt = now;
    void notifyProvisioningUpdate("attention", event.message, event.details);
    return;
  }

  if (event.state === "Ready") {
    if (notifiedRunStates.has("Ready")) {
      return;
    }
    // Ready ends the run: forget attention states but remember that the
    // completion was announced so repeated Ready events stay silent.
    notifiedRunStates.clear();
    notifiedRunStates.add("Ready");
    void notifyProvisioningUpdate("complete", "Your instance is ready to use.");
    return;
  }

  if (!ATTENTION_STATES.has(event.state) || notifiedRunStates.has(event.state)) {
    return;
  }
  // Leaving Ready for an interactive state means setup is being redone.
  notifiedRunStates.delete("Ready");
  notifiedRunStates.add(event.state);
  void notifyProvisioningUpdate("attention", event.message, event.details);
}

export async function notifyProvisioningUpdate(
  kind: "attention" | "complete",
  message: string,
  details?: string,
): Promise<void> {
  if (!isNotificationEnabled("provisioning")) {
    return;
  }

  try {
    if (await mainWindowFocused()) {
      return;
    }
    let granted = await isPermissionGranted();
    if (!granted) {
      granted = (await requestPermission()) === "granted";
    }
    if (!granted) {
      return;
    }

    await sendNotification({
      title: kind === "complete"
        ? "No Land — Provisioning complete"
        : "No Land — Provisioning needs attention",
      body: details ? `${message} ${details}` : message,
      icon: "icons/icon.png",
      silent: false,
    });
  } catch (error) {
    console.warn("[provisioning] native notification failed", error);
  }
}
