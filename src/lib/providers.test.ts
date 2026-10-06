import { describe, expect, it } from "vitest";
import { providerDisplayName } from "./providers";

describe("providerDisplayName", () => {
  it("names each provider and defaults to Vast.ai", () => {
    expect(providerDisplayName("tensordock")).toBe("TensorDock");
    expect(providerDisplayName("Shadeform")).toBe("Shadeform");
    expect(providerDisplayName("vast")).toBe("Vast.ai");
    expect(providerDisplayName(undefined)).toBe("Vast.ai");
  });
});
