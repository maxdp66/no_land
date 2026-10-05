import { describe, expect, it, vi } from "vitest";
import {
  getNotificationPreferences,
  isNotificationEnabled,
  setNotificationPreference,
} from "./notificationPreferences";

const STORAGE_KEY = "noland.notificationPreferences";

describe("notificationPreferences", () => {
  it("defaults every kind to enabled when nothing is stored", () => {
    expect(getNotificationPreferences()).toEqual({
      network: true,
      storage: true,
      instances: true,
      provisioning: true,
      budget: true,
    });
    expect(isNotificationEnabled("network")).toBe(true);
  });

  it("persists a preference and reads it back", () => {
    const next = setNotificationPreference("storage", false);
    expect(next.storage).toBe(false);
    expect(JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "{}")).toMatchObject({
      storage: false,
    });
    expect(getNotificationPreferences()).toEqual({
      network: true,
      storage: false,
      instances: true,
      provisioning: true,
      budget: true,
    });
    expect(isNotificationEnabled("storage")).toBe(false);
  });

  it("keeps other kinds when updating one", () => {
    setNotificationPreference("network", false);
    setNotificationPreference("instances", false);
    expect(getNotificationPreferences()).toEqual({
      network: false,
      storage: true,
      instances: false,
      provisioning: true,
      budget: true,
    });
  });

  it("fills missing keys from defaults for partial stored values", () => {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify({ provisioning: false }));
    expect(getNotificationPreferences()).toEqual({
      network: true,
      storage: true,
      instances: true,
      provisioning: false,
      budget: true,
    });
  });

  it("falls back to defaults when storage holds corrupt JSON", () => {
    window.localStorage.setItem(STORAGE_KEY, "{not json");
    expect(getNotificationPreferences()).toEqual({
      network: true,
      storage: true,
      instances: true,
      provisioning: true,
      budget: true,
    });
  });

  it("falls back to defaults when storage holds a non-object", () => {
    window.localStorage.setItem(STORAGE_KEY, "42");
    expect(getNotificationPreferences().network).toBe(true);
  });

  it("still returns the updated preferences when storage writes throw", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota exceeded");
    });
    const next = setNotificationPreference("network", false);
    expect(next.network).toBe(false);
  });
});
