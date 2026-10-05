import { describe, expect, it } from "vitest";
import { buttonLabel, snapshotGamepads } from "./controllerSnapshot";

function pad(index: number, overrides: Partial<Gamepad> = {}): Gamepad {
  return {
    index,
    id: "Xbox Wireless Controller (STANDARD GAMEPAD Vendor: 045e Product: 0b13)",
    connected: true,
    mapping: "standard",
    axes: [0.02, -0.5, 0, 1],
    buttons: [
      { pressed: true, touched: true, value: 1 },
      { pressed: false, touched: false, value: 0 },
      { pressed: false, touched: false, value: 0.7 },
    ],
    timestamp: 0,
    hapticActuators: [],
    vibrationActuator: null,
    ...overrides,
  } as unknown as Gamepad;
}

describe("snapshotGamepads", () => {
  it("lists connected pads with pressed buttons and dead-zoned axes", () => {
    const [first] = snapshotGamepads([pad(0), null, pad(2, { connected: false })]);
    expect(first.name).toBe("Xbox Wireless Controller");
    expect(first.pressedButtons).toEqual([0, 2]);
    expect(first.axes).toEqual([0, -0.5, 0, 1]);
    expect(first.standardMapping).toBe(true);
    expect(first.canRumble).toBe(false);
  });

  it("supports several controllers for co-op", () => {
    expect(snapshotGamepads([pad(0), pad(1), pad(2)]).map((snapshot) => snapshot.index)).toEqual([0, 1, 2]);
  });

  it("labels standard and non-standard buttons", () => {
    expect(buttonLabel(0, true)).toBe("A");
    expect(buttonLabel(16, true)).toBe("Guide");
    expect(buttonLabel(3, false)).toBe("B3");
  });
});
