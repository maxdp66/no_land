import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";

export interface AppUpdateInfo {
  currentVersion: string;
  latestVersion: string;
  releaseName: string;
  releaseNotes: string;
  publishedAt: string | null;
}

export interface AppUpdateProgress {
  phase: "downloading" | "installing" | "restarting";
  downloadedBytes: number;
  totalBytes: number | null;
  percent: number | null;
}

const SKIPPED_VERSION_STORAGE_KEY = "noland.skippedUpdateVersion";

let pendingUpdate: Update | null = null;

export function getSkippedAppUpdateVersion(): string | null {
  try {
    return window.localStorage.getItem(SKIPPED_VERSION_STORAGE_KEY);
  } catch {
    return null;
  }
}

export function skipAppUpdateVersion(version: string): void {
  try {
    window.localStorage.setItem(SKIPPED_VERSION_STORAGE_KEY, version);
  } catch {
    // Storage unavailable: the prompt will come back on the next launch.
  }
}

export async function checkForAppUpdate(): Promise<AppUpdateInfo | null> {
  if (!("__TAURI_INTERNALS__" in window)) return null;

  // Re-query when the cached update was skipped so a newer release still surfaces.
  if (!pendingUpdate || pendingUpdate.version === getSkippedAppUpdateVersion()) {
    pendingUpdate = await check({ timeout: 30_000 });
  }
  if (!pendingUpdate || pendingUpdate.version === getSkippedAppUpdateVersion()) return null;

  return {
    currentVersion: pendingUpdate.currentVersion,
    latestVersion: pendingUpdate.version,
    releaseName: `Noland Connect ${pendingUpdate.version}`,
    releaseNotes: pendingUpdate.body?.trim() || "Performance improvements and bug fixes.",
    publishedAt: pendingUpdate.date ?? null,
  };
}

export async function installPendingAppUpdate(
  onProgress: (progress: AppUpdateProgress) => void,
): Promise<void> {
  const update = pendingUpdate;
  if (!update) throw new Error("The update is no longer available. Check again.");

  let downloadedBytes = 0;
  let totalBytes: number | null = null;
  const report = (event: DownloadEvent) => {
    if (event.event === "Started") {
      totalBytes = event.data.contentLength ?? null;
    } else if (event.event === "Progress") {
      downloadedBytes += event.data.chunkLength;
    }
    onProgress({
      phase: event.event === "Finished" ? "installing" : "downloading",
      downloadedBytes,
      totalBytes,
      percent: totalBytes && totalBytes > 0
        ? Math.min(100, Math.round((downloadedBytes / totalBytes) * 100))
        : null,
    });
  };

  await update.downloadAndInstall(report);
  pendingUpdate = null;
  onProgress({ phase: "restarting", downloadedBytes, totalBytes, percent: 100 });
  await relaunch();
}
