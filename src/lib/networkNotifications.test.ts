import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: vi.fn(),
  requestPermission: vi.fn(),
  sendNotification: vi.fn(),
}));

import { networkWarningBody, type NetworkStatusEvent } from "./networkNotifications";

function event(overrides: Partial<NetworkStatusEvent>): NetworkStatusEvent {
  return {
    current: "BAD",
    reasons: [],
    alertEligible: true,
    ...overrides,
  };
}

describe("networkWarningBody", () => {
  it("reports a lost connection first, even with other reasons", () => {
    expect(
      networkWarningBody(
        event({
          reasons: ["PACKET_LOSS", "CONNECTION_LOST"],
          keyMetrics: { lossPercent: 50 },
        }),
      ),
    ).toBe("The connection to your gaming PC appears to be lost.");
  });

  it("formats packet loss with one decimal", () => {
    expect(
      networkWarningBody(event({ reasons: ["PACKET_LOSS"], keyMetrics: { lossPercent: 3.456 } })),
    ).toBe("Packet loss has reached 3.5%. Streaming may stutter.");
  });

  it("treats missing packet-loss metric as 0", () => {
    expect(networkWarningBody(event({ reasons: ["PACKET_LOSS"], keyMetrics: {} }))).toBe(
      "Packet loss has reached 0.0%. Streaming may stutter.",
    );
  });

  it("formats jitter", () => {
    expect(
      networkWarningBody(event({ reasons: ["HIGH_JITTER"], keyMetrics: { jitterMs: 12 } })),
    ).toBe("Network jitter has reached 12.0 ms. Streaming may feel inconsistent.");
  });

  it("formats latency when the median RTT is known", () => {
    expect(
      networkWarningBody(event({ reasons: ["HIGH_LATENCY"], keyMetrics: { medianRttMs: 87.25 } })),
    ).toBe("Network latency is 87.3 ms. Input may feel delayed.");
  });

  it("falls back to the generic message when metrics are missing", () => {
    const generic = "High latency variation or packet loss may affect streaming.";
    expect(networkWarningBody(event({ reasons: ["PACKET_LOSS"] }))).toBe(generic);
    expect(
      networkWarningBody(event({ reasons: ["HIGH_LATENCY"], keyMetrics: { medianRttMs: null } })),
    ).toBe(generic);
    expect(networkWarningBody(event({ reasons: [] }))).toBe(generic);
  });
});
