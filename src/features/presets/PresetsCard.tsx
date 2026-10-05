import { useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import { applyServerPreset, deleteServerPreset, saveServerPreset } from "../../lib/backend";
import { errorMessage } from "../../lib/errorMessage";
import type { OfferCandidate, PersistedAppState, ServerPreset } from "../../lib/types";
import { presetSummary } from "./presetSummary";

interface Props {
  presets: ServerPreset[];
  busy: boolean;
  onStateChange: (state: PersistedAppState) => void;
  /** Search offers with the current preferences and return the first page. */
  onSearchOffers: () => Promise<OfferCandidate[]>;
  /** Select an offer and start provisioning it. */
  onRentOffer: (offer: OfferCandidate, storageGb: number) => Promise<void>;
}

export function PresetsCard({ presets, busy, onStateChange, onSearchOffers, onRentOffer }: Props) {
  const [name, setName] = useState("");
  const [working, setWorking] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  async function run(key: string, task: () => Promise<void>) {
    setWorking(key);
    setMessage(null);
    try {
      await task();
    } catch (error) {
      setMessage(errorMessage(error));
    } finally {
      setWorking(null);
    }
  }

  const saveCurrent = () =>
    run("save", async () => {
      onStateChange(await saveServerPreset(name));
      setMessage(`Saved “${name.trim()}”.`);
      setName("");
    });

  const apply = (preset: ServerPreset) =>
    run(`apply:${preset.id}`, async () => {
      onStateChange(await applyServerPreset(preset.id));
      setMessage(`Applied “${preset.name}”. Search for servers to see matching offers.`);
    });

  const quickStart = (preset: ServerPreset) =>
    run(`start:${preset.id}`, async () => {
      onStateChange(await applyServerPreset(preset.id));
      const offers = await onSearchOffers();
      const best = offers[0];
      if (!best) {
        setMessage(`No offers match “${preset.name}” right now. Try again later or relax its filters.`);
        return;
      }
      await onRentOffer(best, preset.serverPreferences.storageGb);
    });

  const remove = (preset: ServerPreset) =>
    run(`delete:${preset.id}`, async () => {
      onStateChange(await deleteServerPreset(preset.id));
    });

  const disabled = busy || working !== null;

  return (
    <Card className="pixel-frame">
      <h3 className="font-display text-sm uppercase tracking-[0.12em] text-white">Saved Setups</h3>
      <p className="mt-1 text-[1rem] text-[#8db7d8]">
        Save your current server filters and stream quality, then start a matching server in one click.
      </p>

      {presets.length > 0 ? (
        <ul className="mt-3 space-y-2">
          {presets.map((preset) => (
            <li
              key={preset.id}
              className="flex flex-wrap items-center justify-between gap-2 border border-[#3a4068] bg-[#10152f]/60 px-3 py-2"
            >
              <div className="min-w-0">
                <p className="font-display text-[11px] uppercase tracking-[0.08em] text-white">{preset.name}</p>
                <p className="text-[0.95rem] text-[#9ec4df]">{presetSummary(preset)}</p>
              </div>
              <div className="flex flex-wrap gap-2">
                <Button
                  onClick={() => void quickStart(preset)}
                  disabled={disabled}
                  loading={working === `start:${preset.id}`}
                  loadingText="Starting..."
                >
                  Quick Start
                </Button>
                <Button variant="secondary" onClick={() => void apply(preset)} disabled={disabled}>
                  Apply
                </Button>
                <Button
                  variant="ghost"
                  aria-label={`Delete preset ${preset.name}`}
                  onClick={() => void remove(preset)}
                  disabled={disabled}
                >
                  Delete
                </Button>
              </div>
            </li>
          ))}
        </ul>
      ) : null}

      <div className="mt-3 flex flex-wrap items-end gap-3">
        <div className="min-w-[200px] flex-1">
          <InputField
            label="Save current settings as"
            placeholder="e.g. 4090 · Europe · 1440p"
            maxLength={40}
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
        </div>
        <Button variant="secondary" onClick={() => void saveCurrent()} disabled={disabled || !name.trim()}>
          Save Setup
        </Button>
      </div>
      {message ? (
        <p className="mt-2 text-[1rem] text-[#9ec4df]" aria-live="polite">
          {message}
        </p>
      ) : null}
    </Card>
  );
}
