import { describe, expect, it } from "vitest";
import { budgetStatusLabel, formatHourlyUsd, formatSessionDuration, formatUsd } from "./spendFormat";

describe("spendFormat", () => {
  it("formats currency and guards against non-finite values", () => {
    expect(formatUsd(1.234)).toBe("$1.23");
    expect(formatUsd(Number.NaN)).toBe("$0.00");
    expect(formatHourlyUsd(0.4021)).toBe("$0.402/hr");
  });

  it("formats session durations", () => {
    const now = new Date("2026-10-05T12:30:00Z");
    expect(formatSessionDuration("2026-10-05T12:05:00Z", now)).toBe("25m");
    expect(formatSessionDuration("2026-10-05T10:00:00Z", now)).toBe("2h 30m");
    expect(formatSessionDuration(null, now)).toBe("--");
    expect(formatSessionDuration("not a date", now)).toBe("--");
  });

  it("labels budget states", () => {
    expect(budgetStatusLabel("exceeded")).toBe("Budget reached");
    expect(budgetStatusLabel("disabled")).toBe("No budget set");
  });
});
