import { beforeEach, describe, expect, it, vi } from "vitest";

const check = vi.fn();
vi.mock("@tauri-apps/plugin-updater", () => ({ check: (...args: unknown[]) => check(...args) }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn() }));

function fakeUpdate(version: string) {
  return { version, currentVersion: "1.0.0", body: "notes", date: null };
}

describe("updateChecker skip version", () => {
  beforeEach(() => {
    vi.resetModules();
    check.mockReset();
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
  });

  it("hides a skipped version and surfaces a newer one", async () => {
    const { checkForAppUpdate, skipAppUpdateVersion } = await import("./updateChecker");
    check.mockResolvedValue(fakeUpdate("1.1.0"));
    expect((await checkForAppUpdate())?.latestVersion).toBe("1.1.0");

    skipAppUpdateVersion("1.1.0");
    expect(await checkForAppUpdate()).toBeNull();

    check.mockResolvedValue(fakeUpdate("1.2.0"));
    expect((await checkForAppUpdate())?.latestVersion).toBe("1.2.0");
  });
});
