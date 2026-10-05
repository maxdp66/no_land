import { describe, expect, it } from "vitest";
import { providerKeyErrors } from "./OnboardingScreen";

describe("providerKeyErrors", () => {
  it("requires at least one provider key", () => {
    expect(providerKeyErrors("", "").vastApiKey).toMatch(/Vast\.ai API key/);
  });

  it("accepts a TensorDock-only setup", () => {
    expect(providerKeyErrors("", "td_0123456789abcdef")).toEqual({ vastApiKey: "", tensordockApiKey: "" });
  });

  it("flags short keys for whichever provider was filled in", () => {
    expect(providerKeyErrors("short", "")).toEqual({ vastApiKey: "API key seems too short", tensordockApiKey: "" });
    expect(providerKeyErrors("vast_0123456789abcdef", "tiny").tensordockApiKey).toBe("API key seems too short");
  });
});
