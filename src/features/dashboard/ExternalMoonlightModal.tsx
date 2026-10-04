import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "../../components/ui/Button";
import { ModalBody, ModalFrame } from "../../components/ui/ModalFrame";
import {
  externalMoonlightGetConnectionInfo,
  externalMoonlightLaunch,
  externalMoonlightPair,
  externalMoonlightSetExecutablePath,
  externalMoonlightSubmitPin,
} from "../../lib/backend";
import { errorMessage } from "../../lib/errorMessage";
import type {
  ExternalMoonlightConnectionInfo,
  RentedInstanceSummary,
} from "../../lib/types";

interface Props {
  instance: RentedInstanceSummary;
  onClose: () => void;
}

type Busy = null | "path" | "pair" | "launch" | "pin";

function DetailRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between gap-3 rounded border border-[#283252] bg-[#0d132b] px-3 py-2">
      <span className="text-xs uppercase tracking-wide text-[#8fa7c6]">{label}</span>
      <span className="flex items-center gap-2">
        <code className="select-all break-all text-sm text-[#d7e6f7]">{value}</code>
        <Button
          variant="ghost"
          className="px-2 py-1 text-xs"
          onClick={() => void navigator.clipboard?.writeText(value)}
        >
          Copy
        </Button>
      </span>
    </div>
  );
}

export function ExternalMoonlightModal({ instance, onClose }: Props) {
  const [info, setInfo] = useState<ExternalMoonlightConnectionInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [pin, setPin] = useState("");
  const [showPassword, setShowPassword] = useState(false);

  async function refresh() {
    try {
      setInfo(await externalMoonlightGetConnectionInfo());
      setError(null);
    } catch (nextError) {
      setError(errorMessage(nextError));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function run(kind: Busy, action: () => Promise<void>, success: string) {
    setBusy(kind);
    setError(null);
    setNotice(null);
    try {
      await action();
      setNotice(success);
    } catch (nextError) {
      setError(errorMessage(nextError));
    } finally {
      setBusy(null);
    }
  }

  async function choosePath() {
    const selection = await open({
      title: "Choose your Moonlight app",
      multiple: false,
      directory: false,
    });
    if (!selection || Array.isArray(selection)) return;
    await run(
      "path",
      async () => {
        await externalMoonlightSetExecutablePath(selection);
        await refresh();
      },
      "Saved. Noland will use this Moonlight install.",
    );
  }

  async function useBundled() {
    await run(
      "path",
      async () => {
        await externalMoonlightSetExecutablePath(null);
        await refresh();
      },
      "Cleared the custom path.",
    );
  }

  const isOtherInstance =
    info?.activeInstanceId != null && info.activeInstanceId !== instance.instanceId;
  const effectivePath =
    info?.configuredExecutablePath ?? info?.detectedExecutablePath ?? null;
  const disabled = busy !== null;

  return (
    <ModalFrame
      labelledBy="external-moonlight-title"
      panelClassName="pixel-frame max-w-2xl bg-[#090d20] text-white"
    >
      <div className="flex items-start justify-between gap-4 border-b border-[#283252] p-5">
        <div>
          <p className="font-display text-[10px] uppercase tracking-[0.16em] text-neon-cyan">
            Use your own Moonlight
          </p>
          <h2 id="external-moonlight-title" className="mt-1 text-xl font-semibold">
            {instance.label}
          </h2>
        </div>
        <Button variant="ghost" disabled={disabled} onClick={onClose}>
          Close
        </Button>
      </div>

      <ModalBody className="space-y-5 p-5">
        {loading || !info ? (
          <p className="text-[#a8bed6]">Reading connection details…</p>
        ) : (
          <>
            <div
              className={`rounded border px-3 py-2 text-sm ${
                info.tunnelReachable
                  ? "border-neon-cyan/40 bg-neon-cyan/10 text-neon-cyan"
                  : "border-[#9a6536]/70 bg-[#3a2518]/70 text-[#ffd3a3]"
              }`}
            >
              {info.tunnelReachable
                ? "Secure tunnel is connected. Any Moonlight on this computer can reach the instance."
                : "Secure tunnel is not connected. Start the instance with Play once so Noland connects the tunnel, then come back."}
              {isOtherInstance ? (
                <span className="mt-1 block text-xs">
                  The tunnel currently points at instance {info.activeInstanceId}, not this one.
                </span>
              ) : null}
            </div>

            <section className="space-y-2">
              <h3 className="text-sm font-semibold text-[#d7e6f7]">Moonlight app</h3>
              <p className="text-xs leading-5 text-[#8fa7c6]">
                The built-in player stays the default for Play. Pick your own Moonlight
                install here to pair and stream with it instead.
              </p>
              <div className="rounded border border-[#283252] bg-[#0d132b] px-3 py-2 text-sm">
                {effectivePath ? (
                  <>
                    <code className="break-all text-[#d7e6f7]">{effectivePath}</code>
                    {!info.configuredExecutablePath ? (
                      <span className="ml-2 text-xs text-[#8fa7c6]">(found automatically)</span>
                    ) : null}
                  </>
                ) : (
                  <span className="text-[#8fa7c6]">No Moonlight install found yet.</span>
                )}
              </div>
              <div className="flex flex-wrap gap-2">
                <Button variant="ghost" disabled={disabled} onClick={() => void choosePath()}>
                  Choose Moonlight…
                </Button>
                {info.configuredExecutablePath ? (
                  <Button variant="ghost" disabled={disabled} onClick={() => void useBundled()}>
                    Clear custom path
                  </Button>
                ) : null}
                <Button
                  loading={busy === "pair"}
                  loadingText="Pairing…"
                  disabled={disabled || !effectivePath || !info.tunnelReachable}
                  onClick={() =>
                    void run(
                      "pair",
                      externalMoonlightPair,
                      "Moonlight is paired. You can start streaming.",
                    )
                  }
                >
                  Pair automatically
                </Button>
                <Button
                  loading={busy === "launch"}
                  loadingText="Starting…"
                  disabled={disabled || !effectivePath || !info.tunnelReachable}
                  onClick={() =>
                    void run(
                      "launch",
                      () => externalMoonlightLaunch("Desktop"),
                      "Moonlight is starting the Desktop stream.",
                    )
                  }
                >
                  Stream Desktop
                </Button>
              </div>
            </section>

            <section className="space-y-2">
              <h3 className="text-sm font-semibold text-[#d7e6f7]">Connect manually</h3>
              <p className="text-xs leading-5 text-[#8fa7c6]">
                In any Moonlight client on this computer, choose Add PC manually and enter the
                address below. When Moonlight shows a 4-digit PIN, enter it here to approve it.
              </p>
              <DetailRow label="Address" value={info.host} />
              <div className="flex items-center gap-2">
                <input
                  className="w-28 rounded border border-[#354269] bg-[#080d1f] px-3 py-2 text-center font-mono tracking-[0.3em] text-white outline-none focus:border-neon-cyan"
                  inputMode="numeric"
                  maxLength={4}
                  placeholder="PIN"
                  value={pin}
                  disabled={disabled}
                  onChange={(event) => setPin(event.target.value.replace(/\D/g, ""))}
                />
                <Button
                  loading={busy === "pin"}
                  loadingText="Approving…"
                  disabled={disabled || pin.length !== 4 || !info.tunnelReachable}
                  onClick={() =>
                    void run(
                      "pin",
                      async () => {
                        await externalMoonlightSubmitPin(pin);
                        setPin("");
                      },
                      "PIN approved. Moonlight should now show the instance as paired.",
                    )
                  }
                >
                  Approve PIN
                </Button>
              </div>

              <details className="rounded border border-[#283252] bg-[#0d132b] px-3 py-2 text-sm">
                <summary className="cursor-pointer text-[#d7e6f7]">
                  Ports and Sunshine web UI
                </summary>
                <div className="mt-3 space-y-2">
                  <ul className="grid gap-1 text-xs text-[#bfd3ee] sm:grid-cols-2">
                    {info.ports.map((port) => (
                      <li key={`${port.protocol}-${port.port}`}>
                        <code>
                          {port.port}/{port.protocol}
                        </code>{" "}
                        {port.purpose}
                      </li>
                    ))}
                  </ul>
                  <DetailRow label="Web UI" value={info.webUiUrl} />
                  <DetailRow label="Username" value={info.sunshineUsername} />
                  <div className="flex items-center justify-between gap-3 rounded border border-[#283252] bg-[#0d132b] px-3 py-2">
                    <span className="text-xs uppercase tracking-wide text-[#8fa7c6]">Password</span>
                    <span className="flex items-center gap-2">
                      <code className="select-all break-all text-sm text-[#d7e6f7]">
                        {showPassword ? info.sunshinePassword : "••••••••"}
                      </code>
                      <Button
                        variant="ghost"
                        className="px-2 py-1 text-xs"
                        onClick={() => setShowPassword((value) => !value)}
                      >
                        {showPassword ? "Hide" : "Show"}
                      </Button>
                    </span>
                  </div>
                  <Button
                    variant="ghost"
                    className="text-xs"
                    onClick={() => void openUrl(info.webUiUrl)}
                  >
                    Open Sunshine web UI
                  </Button>
                  <p className="text-xs leading-5 text-[#8fa7c6]">
                    These ports are only reachable through Noland&apos;s secure tunnel, so manual
                    connections work from this computer while Noland keeps the tunnel up. Other
                    devices on your network cannot reach the instance this way.
                  </p>
                </div>
              </details>
            </section>
          </>
        )}

        {notice ? (
          <div className="rounded border border-neon-cyan/40 bg-neon-cyan/10 p-3 text-sm text-neon-cyan">
            {notice}
          </div>
        ) : null}
        {error ? (
          <div className="rounded border border-red-400/50 bg-red-950/50 p-3 text-sm text-red-200">
            {error}
          </div>
        ) : null}
      </ModalBody>
    </ModalFrame>
  );
}
