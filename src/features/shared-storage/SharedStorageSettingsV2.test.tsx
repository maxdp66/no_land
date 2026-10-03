import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ProviderDefinition } from "../../lib/types";
import { SharedStorageSettingsV2 } from "./SharedStorageSettingsV2";

// Shaped like the backend's serde output for the B2 provider definition.
const b2Provider: ProviderDefinition = {
  provider: "backblaze_b2",
  label: "Backblaze B2",
  category: "object-storage",
  isOauth: false,
  description: "Backblaze B2 object storage",
  fields: [
    { key: "key_id", label: "Key ID", fieldType: "text", required: true, placeholder: null, helpText: null },
    {
      key: "key_scope",
      label: "Key Access",
      fieldType: {
        select: {
          options: [
            { value: "all_buckets", label: "All buckets" },
            { value: "single_bucket", label: "Restricted to one bucket" },
          ],
        },
      },
      required: false,
      placeholder: null,
      helpText: null,
    },
    { key: "bucket", label: "Bucket Name", fieldType: "text", required: false, placeholder: null, helpText: null },
  ],
} as ProviderDefinition;

function renderWithB2() {
  render(
    <SharedStorageSettingsV2
      busy={false}
      providers={[b2Provider]}
      profiles={[]}
      testResult={null}
      oauthSessionId={null}
      onConnectProvider={vi.fn()}
      onTestConnection={vi.fn()}
      onSetActiveProfile={vi.fn()}
      onDisconnect={vi.fn()}
      onLoadProviders={vi.fn(() => Promise.resolve())}
      onLoadProfiles={vi.fn(() => Promise.resolve())}
      onBeginOauthFlow={vi.fn()}
      onCompleteOauthFlow={vi.fn()}
      onCancelOauthFlow={vi.fn()}
    />,
  );
  fireEvent.click(screen.getByText("Connect Storage Provider"));
  fireEvent.click(screen.getByText("Backblaze B2"));
}

describe("SharedStorageSettingsV2 Backblaze B2 form", () => {
  it("renders Key Access as a dropdown and asks for a bucket only for a single-bucket key", () => {
    renderWithB2();

    const keyAccess = screen.getByRole("combobox");
    expect(keyAccess).toHaveValue("all_buckets");
    expect(screen.queryByText("Bucket Name")).not.toBeInTheDocument();

    fireEvent.change(keyAccess, { target: { value: "single_bucket" } });

    expect(screen.getByText("Bucket Name")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Connect Backblaze B2/ })).toBeDisabled();
  });
});
