import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RentedInstanceSummary } from "../../lib/types";

const backend = vi.hoisted(() => ({
  openRemoteTerminal: vi.fn(),
  closeRemoteTerminal: vi.fn(),
  resizeRemoteTerminal: vi.fn(),
  writeRemoteTerminal: vi.fn(),
}));

vi.mock("../../lib/backend", () => backend);

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => undefined)),
}));

vi.mock("@xterm/xterm", () => ({
  Terminal: vi.fn(function Terminal(this: Record<string, unknown>) {
    this.rows = 24;
    this.cols = 80;
    this.loadAddon = vi.fn();
    this.open = vi.fn();
    this.writeln = vi.fn();
    this.write = vi.fn();
    this.focus = vi.fn();
    this.dispose = vi.fn();
    this.onData = vi.fn(() => ({ dispose: vi.fn() }));
  }),
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: vi.fn(function FitAddon(this: Record<string, unknown>) {
    this.fit = vi.fn();
  }),
}));

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

import { InstanceTerminalModal } from "./InstanceTerminalModal";

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}

function makeInstance(overrides: Partial<RentedInstanceSummary> = {}): RentedInstanceSummary {
  return {
    performanceOverlayEnabled: false,
    instanceId: 42,
    label: "Test rig",
    status: "running",
    gpuName: "RTX 4090",
    sshHost: "ssh.example.test",
    sshPort: 2222,
    publicIp: "203.0.113.5",
    embeddedMoonlightPipelineEnabled: false,
    ...overrides,
  };
}

async function flush() {
  await act(async () => {
    await Promise.resolve();
  });
}

describe("InstanceTerminalModal", () => {
  beforeEach(() => {
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
    backend.openRemoteTerminal.mockReset().mockResolvedValue({ sessionId: "session-1" });
    backend.closeRemoteTerminal.mockReset().mockResolvedValue(undefined);
    backend.resizeRemoteTerminal.mockReset().mockResolvedValue(undefined);
    backend.writeRemoteTerminal.mockReset().mockResolvedValue(undefined);
  });

  it("does not reconnect when re-rendered with a new but equal instance object", async () => {
    const onClose = vi.fn();
    const { rerender } = render(<InstanceTerminalModal instance={makeInstance()} onClose={onClose} />);
    await flush();
    expect(backend.openRemoteTerminal).toHaveBeenCalledTimes(1);
    expect(backend.openRemoteTerminal).toHaveBeenCalledWith(42);

    // Simulate store refreshes: a fresh object each time, plus an unrelated field change.
    rerender(<InstanceTerminalModal instance={makeInstance()} onClose={() => undefined} />);
    rerender(<InstanceTerminalModal instance={makeInstance({ status: "running", label: "Renamed" })} onClose={onClose} />);
    await flush();

    expect(backend.openRemoteTerminal).toHaveBeenCalledTimes(1);
    expect(backend.closeRemoteTerminal).not.toHaveBeenCalled();
    expect(screen.getByText(/Renamed/)).toBeInTheDocument();
  });

  it("reconnects when the SSH endpoint changes", async () => {
    const { rerender } = render(<InstanceTerminalModal instance={makeInstance()} onClose={vi.fn()} />);
    await flush();
    rerender(<InstanceTerminalModal instance={makeInstance({ sshPort: 2223 })} onClose={vi.fn()} />);
    await flush();

    expect(backend.openRemoteTerminal).toHaveBeenCalledTimes(2);
    expect(backend.closeRemoteTerminal).toHaveBeenCalledWith("session-1");
  });

  it("closes the remote session on unmount", async () => {
    const { unmount } = render(<InstanceTerminalModal instance={makeInstance()} onClose={vi.fn()} />);
    await flush();
    unmount();
    expect(backend.closeRemoteTerminal).toHaveBeenCalledWith("session-1");
  });
});
