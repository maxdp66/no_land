import { useState, type ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "../../components/ui/Button";
import { InputField } from "../../components/ui/InputField";
import { errorMessage } from "../../lib/errorMessage";

interface Props {
  providerName: string;
  description: ReactNode;
  keyPageUrl: string;
  currentKey: string;
  busy: boolean;
  testId: string;
  onSave: (apiKey: string) => Promise<void>;
}

/** Optional API key for an extra GPU provider, verified on save. */
export function ProviderKeySettings({
  providerName,
  description,
  keyPageUrl,
  currentKey,
  busy,
  testId,
  onSave,
}: Props) {
  const [apiKey, setApiKey] = useState(currentKey);
  const [message, setMessage] = useState<string | null>(null);
  const configured = currentKey.trim().length > 0;

  async function save(nextKey: string) {
    setMessage(null);
    try {
      await onSave(nextKey);
      setApiKey(nextKey);
      setMessage(nextKey ? `${providerName} key verified and saved.` : `${providerName} key removed.`);
    } catch (error) {
      setMessage(errorMessage(error));
    }
  }

  async function openKeyPage() {
    try {
      await openUrl(keyPageUrl);
    } catch {
      window.open(keyPageUrl, "_blank", "noopener,noreferrer");
    }
  }

  return (
    <div className="mt-6 border-t border-[#3e4270] pt-4" data-testid={testId}>
      <h3 className="font-display text-[11px] uppercase tracking-[0.12em] text-white">{providerName} (optional)</h3>
      <p className="mt-1 text-[1rem] text-[#8fb4d4]">{description}</p>
      <div className="mt-3 grid gap-3">
        <InputField
          label={`${providerName} API Key`}
          value={apiKey}
          type="password"
          onChange={(event) => setApiKey(event.target.value)}
        />
        <div className="flex flex-wrap items-center gap-3">
          <Button disabled={busy || apiKey.trim().length < 16} onClick={() => void save(apiKey.trim())}>
            Save {providerName} Key
          </Button>
          {configured ? (
            <Button variant="ghost" disabled={busy} onClick={() => void save("")}>
              Remove Key
            </Button>
          ) : null}
          <Button variant="secondary" disabled={busy} onClick={() => void openKeyPage()}>
            Open {providerName} API Page
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
