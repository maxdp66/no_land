export interface ControllerSnapshot {
  index: number;
  name: string;
  standardMapping: boolean;
  pressedButtons: number[];
  axes: number[];
  canRumble: boolean;
}

const AXIS_DEAD_ZONE = 0.08;

/** Plain, render-friendly view of the connected gamepads. */
export function snapshotGamepads(pads: ArrayLike<Gamepad | null>): ControllerSnapshot[] {
  const snapshots: ControllerSnapshot[] = [];
  for (let i = 0; i < pads.length; i += 1) {
    const pad = pads[i];
    if (!pad || !pad.connected) {
      continue;
    }
    snapshots.push({
      index: pad.index,
      name: pad.id.replace(/\s*\(.*?Vendor:.*?\)\s*/i, " ").trim() || `Controller ${pad.index + 1}`,
      standardMapping: pad.mapping === "standard",
      pressedButtons: pad.buttons.flatMap((button, buttonIndex) =>
        button.pressed || button.value > 0.5 ? [buttonIndex] : [],
      ),
      axes: pad.axes.map((value) => (Math.abs(value) < AXIS_DEAD_ZONE ? 0 : Number(value.toFixed(2)))),
      canRumble: Boolean((pad as Gamepad & { vibrationActuator?: unknown }).vibrationActuator),
    });
  }
  return snapshots;
}

/** Labels for the W3C "standard" gamepad layout. */
export const STANDARD_BUTTON_LABELS = [
  "A", "B", "X", "Y", "LB", "RB", "LT", "RT", "Back", "Start", "LS", "RS",
  "Up", "Down", "Left", "Right", "Guide",
];

export function buttonLabel(index: number, standardMapping: boolean): string {
  return standardMapping && index < STANDARD_BUTTON_LABELS.length
    ? STANDARD_BUTTON_LABELS[index]
    : `B${index}`;
}

export function snapshotsEqual(left: ControllerSnapshot[], right: ControllerSnapshot[]): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}
