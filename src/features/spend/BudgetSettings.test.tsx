import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SpendSummary } from "../../lib/types";

const summary: SpendSummary = {
  month: "2026-10",
  monthToDateUsd: 42.5,
  currentBurnUsdPerHour: 0.6,
  projectedMonthUsd: 100,
  budget: { monthlyBudgetUsd: 50, warnAtPercent: 80, autoStopAtBudget: false },
  budgetStatus: "warning",
  budgetUsedPercent: 85,
  instances: [
    {
      provider: "vast",
      instanceId: 7,
      label: "Gaming rig",
      gpuName: "RTX 4090",
      running: true,
      currentHourlyUsd: 0.6,
      sessionUsd: 1.2,
      sessionStartedAt: new Date(Date.now() - 2 * 60 * 60 * 1000).toISOString(),
      monthToDateUsd: 42.5,
    },
  ],
  months: [
    { month: "2026-10", totalUsd: 42.5, runningHours: 70 },
    { month: "2026-09", totalUsd: 12, runningHours: 20 },
  ],
};

const getSpendSummary = vi.fn(async () => summary);
const updateBudgetSettings = vi.fn(async (settings: SpendSummary["budget"]) => ({
  ...summary,
  budget: settings,
}));

vi.mock("../../lib/backend", () => ({
  getSpendSummary: () => getSpendSummary(),
  updateBudgetSettings: (settings: SpendSummary["budget"]) => updateBudgetSettings(settings),
  subscribeSpendUpdates: vi.fn(async () => () => {}),
}));

import { BudgetSettings } from "./BudgetSettings";

describe("BudgetSettings", () => {
  beforeEach(() => {
    updateBudgetSettings.mockClear();
  });

  it("loads the current budget and shows spend", async () => {
    render(<BudgetSettings />);

    expect(await screen.findByDisplayValue("50")).toBeTruthy();
    expect(screen.getByText("Approaching budget")).toBeTruthy();
    expect(screen.getByText("$42.50 / $50.00")).toBeTruthy();
    expect(screen.getByText(/Session \$1\.20 · 2h 0m/)).toBeTruthy();
  });

  it("saves budget with auto-stop", async () => {
    render(<BudgetSettings />);
    const budgetInput = await screen.findByDisplayValue("50");
    fireEvent.change(budgetInput, { target: { value: "75" } });
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByText("Save Budget"));

    await waitFor(() =>
      expect(updateBudgetSettings).toHaveBeenCalledWith({
        monthlyBudgetUsd: 75,
        warnAtPercent: 80,
        autoStopAtBudget: true,
      }),
    );
    expect(await screen.findByText("Budget saved.")).toBeTruthy();
  });

  it("rejects an out-of-range warning threshold", async () => {
    render(<BudgetSettings />);
    await screen.findByDisplayValue("50");
    fireEvent.change(screen.getByDisplayValue("80"), { target: { value: "150" } });
    fireEvent.click(screen.getByText("Save Budget"));

    expect(await screen.findByText(/between 1 and 100 percent/)).toBeTruthy();
    expect(updateBudgetSettings).not.toHaveBeenCalled();
  });
});
