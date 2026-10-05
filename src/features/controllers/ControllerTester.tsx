import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { buttonLabel, snapshotGamepads, snapshotsEqual, type ControllerSnapshot } from "./controllerSnapshot";

type RumbleActuator = {
  playEffect: (type: string, params: Record<string, number>) => Promise<unknown>;
};

function readPads(): ControllerSnapshot[] {
  if (typeof navigator === "undefined" || typeof navigator.getGamepads !== "function") {
    return [];
  }
  return snapshotGamepads(navigator.getGamepads());
}

export function ControllerTester() {
  const [pads, setPads] = useState<ControllerSnapshot[]>(readPads);
  const supported = typeof navigator !== "undefined" && typeof navigator.getGamepads === "function";

  useEffect(() => {
    if (!supported) {
      return;
    }
    let frame = 0;
    let previous: ControllerSnapshot[] = [];
    const poll = () => {
      const next = readPads();
      if (!snapshotsEqual(previous, next)) {
        previous = next;
        setPads(next);
      }
      frame = window.requestAnimationFrame(poll);
    };
    frame = window.requestAnimationFrame(poll);
    return () => window.cancelAnimationFrame(frame);
  }, [supported]);

  function rumble(index: number) {
    const pad = navigator.getGamepads()[index] as (Gamepad & { vibrationActuator?: RumbleActuator }) | null;
    void pad?.vibrationActuator
      ?.playEffect("dual-rumble", { duration: 400, strongMagnitude: 0.8, weakMagnitude: 0.5 })
      .catch(() => undefined);
  }

  return (
    <div className="mt-6 border-t border-[#3e4270] pt-4" data-testid="controller-tester">
      <h3 className="font-display text-[11px] uppercase tracking-[0.12em] text-white">Controller Check</h3>
      <p className="mt-1 text-[1rem] text-[#8fb4d4]">
        Connect controllers and press buttons to confirm they are detected. Up to 16 controllers are forwarded to the
        cloud PC during a stream, so couch co-op works with each controller as its own player.
      </p>
      {!supported ? (
        <p className="mt-3 text-[1rem] text-[#ffe0a3]">This system's web view does not expose controllers here.</p>
      ) : pads.length === 0 ? (
        <p className="mt-3 text-[1rem] text-[#9ec4df]">No controllers detected. Press a button to wake a connected one.</p>
      ) : (
        <ul className="mt-3 space-y-2">
          {pads.map((pad) => (
            <li key={pad.index} className="border border-[#3a4068] bg-[#10152f]/60 px-3 py-2 text-[1rem] text-[#bfd3ee]">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="text-white">
                  Player {pad.index + 1}: {pad.name}
                  {pad.standardMapping ? "" : " (non-standard layout)"}
                </span>
                {pad.canRumble ? (
                  <Button variant="ghost" onClick={() => rumble(pad.index)}>
                    Test Rumble
                  </Button>
                ) : null}
              </div>
              <p className="mt-1">
                Pressed:{" "}
                {pad.pressedButtons.length > 0
                  ? pad.pressedButtons.map((index) => buttonLabel(index, pad.standardMapping)).join(" ")
                  : "none"}
              </p>
              <p className="mt-1 font-mono text-[0.9rem] text-[#8db7d8]">
                Sticks: {pad.axes.map((value) => value.toFixed(2)).join("  ")}
              </p>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
