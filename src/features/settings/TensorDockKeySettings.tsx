import { useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "../../components/ui/Button";
import { InputField } from "../../components/ui/InputField";
import { TENSORDOCK_API_KEY_URL } from "../../lib/constants";
import { errorMessage } from "../../lib/errorMessage";
import { useAppStore } from "../../store/appStore";

interface Props {
  currentKey: string;
  busy: boolean;
}

export function TensorDockKeySettings({ currentKey, busy }: Props) {
  const saveTensordockApiKey = useAppStore((state) => state.saveTensordockApiKey);
  const [apiKey, setApiKey] = useState(currentKey);
  const [message, setMessage] = useState<string | null>(null);
  const configured = currentKey.trim().length > 0;

  async function save(nextKey: string) {
    setMessage(null);
    try {
      await saveTensordockApiKey(nextKey);
      setApiKey(nextKey);
      setMessage(nextKey ? "TensorDock key verified and saved." : "TensorDock key removed.");
    } catch (error) {
      setMessage(errorMessage(error));
    }
  }

  async function openKeyPage() {
    try {
      await openUrl(TENSORDOCK_API_KEY_URL);
    } catch {
      window.open(TENSORDOCK_API_KEY_URL, "_blank", "noopener,noreferrer");
    }
  }

  return (
    <div className="mt-6 border-t border-[#3e4270] pt-4" data-testid="tensordock-key-settings">
      <h3 className="font-display text-[11px] uppercase tracking-[0.12em] text-white">TensorDock (optional)</h3>
      <p className="mt-1 text-[1rem] text-[#8fb4d4]">
        Add a TensorDock API key to include TensorDock virtual machines in server search. Offers from both providers
        are ranked together.
      </p>
      <div className="mt-3 grid gap-3">
        <InputField
          label="TensorDock API Key"
          value={apiKey}
          type="password"
          onChange={(event) => setApiKey(event.target.value)}
        />
        <div className="flex flex-wrap items-center gap-3">
          <Button disabled={busy || apiKey.trim().length < 16} onClick={() => void save(apiKey.trim())}>
            Save TensorDock Key
          </Button>
          {configured ? (
            <Button variant="ghost" disabled={busy} onClick={() => void save("")}>
              Remove Key
            </Button>
          ) : null}
          <Button variant="secondary" disabled={busy} onClick={() => void openKeyPage()}>
            Open TensorDock API Page
          </Button>
        </div>
        {message ? (
          <p className="text-[1rem] text-[#9ec4df]" aria-live="polite">
            {message}
          </p>
        ) : null}
      </div>
    </div>
  );
}
