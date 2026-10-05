import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import type { PersistedAppState, WireGuardSetupStatus } from "../../lib/types";

vi.mock("../../lib/backend", () => ({
  getInstanceConnectionStatus: vi.fn(() => new Promise(() => {})),
  repairInstanceConnection: vi.fn(),
  setInstanceConnectionPreference: vi.fn(),
}));
vi.mock("../../lib/arcadeAudio", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../lib/arcadeAudio")>()),
  playArcadeClick: vi.fn(),
}));

import { SettingsScreen } from "./SettingsScreen";

function buildAppState(
  status: WireGuardSetupStatus,
  provisioned: boolean,
): PersistedAppState {
  return {
    onboardingCompleted: true,
    credentials: {
      vastApiKey: "",
      appUsername: "",
      appPassword: "",
      twitchClientId: "",
      twitchClientSecret: "",
    },
    ssh: { sshUsername: "", sshPassword: "" },
    serverPreferences: {
      minReliability: 0.9,
      storageGb: 100,
      templateHash: "",
    },
    moonlightPreferences: {
      bitrate: 20000,
      fps: 60,
      refreshRateMode: "auto",
      width: 1920,
      height: 1080,
      displayOutput: "",
      aspectRatio: "16:9",
      hostaudio: 0,
      showperfoverlay: 0,
      keepawake: 1,
      framepacing: 0,
      vsync: 0,
      hdr: 0,
      videocfg: 0,
      videodec: 0,
      yuv444: 0,
      gameopts: 0,
      gamepadmouse: 0,
      detectnetblocking: 0,
      showInputDebugHud: 0,
    },
    sunshine: {
      configured: false,
      headlessEdidBase64: "",
      edidMode: "auto_detect",
      edidRefreshRateHz: 60,
      edidSourceLabel: "",
    },
    cloudflareTurn: { enabled: false },
    autoShutdown: {},
    provisionedServers: provisioned
      ? [
          {
            instanceId: 42,
            network: {
              preference: "direct",
              activeTransport: "direct",
              cloudflareTurn: { enabled: false },
            },
          },
        ]
      : [],
    postWireguardSetup: { wireguardSetupStatus: status },
  } as unknown as PersistedAppState;
}

function renderSettings(
  appState: PersistedAppState,
  overrides: Partial<{
    onTunnelConnect: () => Promise<string | null>;
    onTunnelDisconnect: () => Promise<string | null>;
    onTunnelVerify: () => Promise<unknown>;
  }> = {},
) {
  const noop = vi.fn(async () => {});
  const props = {
    appState,
    busy: false,
    storageProviders: [],
    sharedStorageProfiles: [],
    sharedStorageTestResult: null,
    onLoadStorageProviders: noop,
    onConnectStorageProvider: noop,
    onTestStorageConnection: noop,
    onLoadSharedStorageProfiles: noop,
    onSetActiveStorageProfile: noop,
    onDisconnectStorageProfile: noop,
    oauthSessionId: null,
    onBeginOauthFlow: vi.fn(async () => null),
    onCompleteOauthFlow: noop,
    onCancelOauthFlow: noop,
    onSaveApiKey: noop,
    onSavePlatformCredentials: noop,
    onSaveIgdbCredentials: noop,
    onSaveAutoShutdownSettings: noop,
    onSaveServerPreferences: noop,
    onSaveMoonlightPreferences: noop,
    onSaveSshCredentials: noop,
    cloudflareTurnSettings: null,
    cloudflareTurnTestResult: null,
    onLoadCloudflareTurnSettings: noop,
    onTestCloudflareTurnSettings: vi.fn(async () => null),
    onSaveCloudflareTurnSettings: noop,
    onClearCloudflareTurnSettings: noop,
    onRegenerateEdid: noop,
    onTunnelConnect: vi.fn(async () => "Tunnel up"),
    onTunnelDisconnect: vi.fn(async () => "Tunnel down"),
    onTunnelVerify: vi.fn(async () => ({
      reachable: true,
      host: "10.77.0.1",
      checkedPorts: [47989, 47990],
      reachablePorts: [47989, 47990],
    })),
    ...overrides,
  };
  render(
    <MemoryRouter>
      <SettingsScreen {...(props as Parameters<typeof SettingsScreen>[0])} />
    </MemoryRouter>,
  );
  fireEvent.click(screen.getByRole("button", { name: "Connection" }));
  return props;
}

describe("Settings managed tunnel control", () => {
  it("disables the controls until an instance is provisioned", () => {
    renderSettings(buildAppState("not_started", false));

    expect(screen.getByRole("button", { name: "Turn On" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Turn Off" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Check Status" })).toBeDisabled();
    expect(screen.getByText(/Provision an instance first/)).toBeInTheDocument();
  });

  it("shows the persisted tunnel status", () => {
    renderSettings(buildAppState("connected", true));
    expect(screen.getByText("Connected")).toBeInTheDocument();
  });

  it("turns the tunnel on and verifies it", async () => {
    const props = renderSettings(buildAppState("not_started", true));
    expect(screen.getByText("Off")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Turn On" }));

    await screen.findByText("Reachable: 10.77.0.1 on ports 47989, 47990");
    expect(props.onTunnelConnect).toHaveBeenCalledTimes(1);
    expect(props.onTunnelVerify).toHaveBeenCalledTimes(1);
    expect(screen.getByText("Tunnel up")).toBeInTheDocument();
  });

  it("skips verification when turning on fails", async () => {
    const props = renderSettings(buildAppState("not_started", true), {
      onTunnelConnect: vi.fn(async () => null),
    });

    fireEvent.click(screen.getByRole("button", { name: "Turn On" }));

    await waitFor(() => expect(props.onTunnelConnect).toHaveBeenCalled());
    expect(props.onTunnelVerify).not.toHaveBeenCalled();
  });

  it("turns the tunnel off", async () => {
    const props = renderSettings(buildAppState("connected", true));

    fireEvent.click(screen.getByRole("button", { name: "Turn Off" }));

    await screen.findByText("Tunnel down");
    expect(props.onTunnelDisconnect).toHaveBeenCalledTimes(1);
  });

  it("reports an unreachable tunnel on status check", async () => {
    renderSettings(buildAppState("connected", true), {
      onTunnelVerify: vi.fn(async () => ({
        reachable: false,
        host: "10.77.0.1",
        checkedPorts: [47989],
        reachablePorts: [],
        error: "handshake timed out",
      })),
    });

    fireEvent.click(screen.getByRole("button", { name: "Check Status" }));

    await screen.findByText("Not reachable: handshake timed out");
  });
});
