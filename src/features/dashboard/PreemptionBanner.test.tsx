import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { PreemptionBanner } from "./PreemptionBanner";

const event = {
  instanceId: 7,
  label: "Gaming rig",
  gpuName: "RTX 4090",
  provider: "vast",
  status: "exited",
  message: "Instance was outbid.",
};

describe("PreemptionBanner", () => {
  it("renders nothing without events", () => {
    const { container } = render(
      <PreemptionBanner events={[]} onFindReplacement={() => {}} onDismiss={() => {}} />,
    );
    expect(container.innerHTML).toBe("");
  });

  it("offers a replacement and can be dismissed", () => {
    const onFindReplacement = vi.fn();
    const onDismiss = vi.fn();
    render(<PreemptionBanner events={[event]} onFindReplacement={onFindReplacement} onDismiss={onDismiss} />);
    expect(screen.getByRole("alert").textContent).toContain("Gaming rig · RTX 4090 was interrupted");
    fireEvent.click(screen.getByText("Find a Replacement"));
    expect(onFindReplacement).toHaveBeenCalledWith(event);
    fireEvent.click(screen.getByText("Dismiss"));
    expect(onDismiss).toHaveBeenCalledWith(7);
  });
});
