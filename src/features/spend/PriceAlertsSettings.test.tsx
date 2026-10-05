import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PersistedAppState, PriceAlert } from "../../lib/types";
import { useAppStore } from "../../store/appStore";

const savePriceAlert = vi.fn(async (_input: unknown) => ({ priceAlerts: [] }) as unknown as PersistedAppState);

vi.mock("../../lib/backend", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../lib/backend")>()),
  savePriceAlert: (input: unknown) => savePriceAlert(input),
  deletePriceAlert: vi.fn(),
  setPriceAlertEnabled: vi.fn(),
}));

import { describePriceAlert, PriceAlertsSettings } from "./PriceAlertsSettings";

const alert: PriceAlert = {
  id: "a1",
  gpuQuery: "4090",
  countryCode: "US",
  maxHourlyUsd: 0.45,
  enabled: true,
  lastNotifiedAt: null,
  lastNotifiedPrice: 0.39,
};

describe("PriceAlertsSettings", () => {
  beforeEach(() => {
    savePriceAlert.mockClear();
    useAppStore.setState({ appState: { priceAlerts: [alert] } as unknown as PersistedAppState });
  });

  it("describes alerts", () => {
    expect(describePriceAlert(alert)).toBe("4090 · US · ≤ $0.450/hr");
    expect(describePriceAlert({ ...alert, gpuQuery: "", countryCode: "" })).toBe("Any GPU · anywhere · ≤ $0.450/hr");
  });

  it("lists alerts and adds a new one", async () => {
    render(<PriceAlertsSettings />);
    expect(screen.getByText("4090 · US · ≤ $0.450/hr")).toBeTruthy();
    expect(screen.getByText("last seen $0.390/hr")).toBeTruthy();

    fireEvent.change(screen.getByPlaceholderText("e.g. 4090"), { target: { value: "A6000" } });
    fireEvent.change(screen.getByLabelText("Max $/hr"), { target: { value: "0.6" } });
    fireEvent.click(screen.getByText("Add Alert"));
    await waitFor(() =>
      expect(savePriceAlert).toHaveBeenCalledWith({ gpuQuery: "A6000", countryCode: "", maxHourlyUsd: 0.6 }),
    );
  });
});
