import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { OfferCandidate, PersistedAppState, ServerPreset } from "../../lib/types";

const applyServerPreset = vi.fn(async (_id: string) => ({ serverPresets: [] }) as unknown as PersistedAppState);
const saveServerPreset = vi.fn(async (_name: string) => ({ serverPresets: [] }) as unknown as PersistedAppState);
const deleteServerPreset = vi.fn(async (_id: string) => ({ serverPresets: [] }) as unknown as PersistedAppState);

vi.mock("../../lib/backend", () => ({
  applyServerPreset: (id: string) => applyServerPreset(id),
  saveServerPreset: (name: string) => saveServerPreset(name),
  deleteServerPreset: (id: string) => deleteServerPreset(id),
}));

import { PresetsCard } from "./PresetsCard";
import { presetSummary } from "./presetSummary";

const preset = {
  id: "p1",
  name: "4090 Europe",
  createdAt: "2026-10-05T00:00:00Z",
  serverPreferences: {
    storageGb: 150,
    maxHourlyPrice: 0.8,
    minGpuRamGb: 24,
    geolocationCountryCode: "de",
  },
  stream: { bitrate: 50000, fps: 120, width: 2560, height: 1440 },
} as unknown as ServerPreset;

describe("PresetsCard", () => {
  beforeEach(() => {
    applyServerPreset.mockClear();
    saveServerPreset.mockClear();
  });

  it("summarizes a preset", () => {
    expect(presetSummary(preset)).toBe("24GB+ VRAM · DE · ≤ $0.80/hr · 150GB disk · 2560×1440@120 · 50 Mbps");
  });

  it("quick start applies the preset and rents the best offer", async () => {
    const offer = { id: 9 } as OfferCandidate;
    const onRentOffer = vi.fn(async () => {});
    const onStateChange = vi.fn();
    render(
      <PresetsCard
        presets={[preset]}
        busy={false}
        onStateChange={onStateChange}
        onSearchOffers={async () => [offer]}
        onRentOffer={onRentOffer}
      />,
    );
    fireEvent.click(screen.getByText("Quick Start"));
    await waitFor(() => expect(onRentOffer).toHaveBeenCalledWith(offer, 150));
    expect(applyServerPreset).toHaveBeenCalledWith("p1");
    expect(onStateChange).toHaveBeenCalled();
  });

  it("explains when no offers match", async () => {
    const onRentOffer = vi.fn(async () => {});
    render(
      <PresetsCard
        presets={[preset]}
        busy={false}
        onStateChange={() => {}}
        onSearchOffers={async () => []}
        onRentOffer={onRentOffer}
      />,
    );
    fireEvent.click(screen.getByText("Quick Start"));
    expect(await screen.findByText(/No offers match “4090 Europe”/)).toBeTruthy();
    expect(onRentOffer).not.toHaveBeenCalled();
  });

  it("saves the current settings under a name", async () => {
    render(
      <PresetsCard
        presets={[]}
        busy={false}
        onStateChange={() => {}}
        onSearchOffers={async () => []}
        onRentOffer={async () => {}}
      />,
    );
    fireEvent.change(screen.getByPlaceholderText(/4090/), { target: { value: "Couch co-op" } });
    fireEvent.click(screen.getByText("Save Setup"));
    await waitFor(() => expect(saveServerPreset).toHaveBeenCalledWith("Couch co-op"));
  });
});
