import { useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import { deletePriceAlert, savePriceAlert, setPriceAlertEnabled } from "../../lib/backend";
import { errorMessage } from "../../lib/errorMessage";
import type { PersistedAppState, PriceAlert } from "../../lib/types";
import { useAppStore } from "../../store/appStore";
import { formatHourlyUsd } from "./spendFormat";

export function describePriceAlert(alert: PriceAlert): string {
  const gpu = alert.gpuQuery.trim() || "Any GPU";
  const where = alert.countryCode.trim() || "anywhere";
  return `${gpu} · ${where} · ≤ ${formatHourlyUsd(alert.maxHourlyUsd)}`;
}

const NO_ALERTS: PriceAlert[] = [];

export function PriceAlertsSettings() {
  // A stable fallback: a fresh [] per render would loop the store selector.
  const alerts = useAppStore((state) => state.appState?.priceAlerts ?? NO_ALERTS);
  const [gpuQuery, setGpuQuery] = useState("");
  const [countryCode, setCountryCode] = useState("");
  const [maxPrice, setMaxPrice] = useState("");
  const [working, setWorking] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  async function run(task: () => Promise<PersistedAppState>, success?: string) {
    setWorking(true);
    setMessage(null);
    try {
      useAppStore.setState({ appState: await task() });
      if (success) {
        setMessage(success);
      }
    } catch (error) {
      setMessage(errorMessage(error));
    } finally {
      setWorking(false);
    }
  }

  function add() {
    const maxHourlyUsd = Number(maxPrice);
    if (!Number.isFinite(maxHourlyUsd) || maxHourlyUsd <= 0) {
      setMessage("Enter a target price above $0.");
      return;
    }
    void run(
      () => savePriceAlert({ gpuQuery, countryCode, maxHourlyUsd }),
      "Price alert saved. Offers are checked every 15 minutes.",
    ).then(() => {
      setGpuQuery("");
      setCountryCode("");
      setMaxPrice("");
    });
  }

  return (
    <Card className="pixel-frame">
      <h2 className="font-display text-[11px] uppercase tracking-[0.12em] text-neon-lime">Price Alerts</h2>
      <p className="mt-2 text-[1.05rem] text-[#a8bed6]">
        Get a notification when a GPU you want is available at or below your target price.
      </p>

      {alerts.length > 0 ? (
        <ul className="mt-4 space-y-2">
          {alerts.map((alert) => (
            <li
              key={alert.id}
              className="flex flex-wrap items-center justify-between gap-2 border border-[#3d426f] bg-[#10152f] px-3 py-2 text-[1rem] text-[#bfd3ee]"
            >
              <label className="flex cursor-pointer items-center gap-3">
                <input
                  type="checkbox"
                  className="h-5 w-5 accent-[#7bff48]"
                  checked={alert.enabled}
                  disabled={working}
                  onChange={(event) => void run(() => setPriceAlertEnabled(alert.id, event.target.checked))}
                />
                <span className="text-white">{describePriceAlert(alert)}</span>
              </label>
              <span className="flex items-center gap-3">
                {alert.lastNotifiedPrice != null ? (
                  <span className="text-[#8db7d8]">last seen {formatHourlyUsd(alert.lastNotifiedPrice)}</span>
                ) : null}
                <Button
                  variant="ghost"
                  aria-label={`Delete price alert ${describePriceAlert(alert)}`}
                  disabled={working}
                  onClick={() => void run(() => deletePriceAlert(alert.id))}
                >
                  Delete
                </Button>
              </span>
            </li>
          ))}
        </ul>
      ) : null}

      <div className="mt-4 grid gap-3 md:grid-cols-[1fr_120px_160px_auto] md:items-end">
        <InputField
          label="GPU (contains)"
          placeholder="e.g. 4090"
          maxLength={40}
          value={gpuQuery}
          onChange={(event) => setGpuQuery(event.target.value)}
        />
        <InputField
          label="Country"
          placeholder="Any"
          maxLength={2}
          value={countryCode}
          onChange={(event) => setCountryCode(event.target.value.toUpperCase())}
        />
        <InputField
          label="Max $/hr"
          type="number"
          min={0}
          step="0.01"
          value={maxPrice}
          onChange={(event) => setMaxPrice(event.target.value)}
        />
        <Button onClick={add} disabled={working || maxPrice.trim() === ""}>
          Add Alert
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
