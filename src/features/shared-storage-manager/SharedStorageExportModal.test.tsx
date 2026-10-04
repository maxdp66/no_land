import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { SharedStorageExportModal } from "./SharedStorageExportModal";

describe("folder export", () => {
  it("saves an explicit VM folder without selecting all applications", async () => {
    const save = vi.fn().mockResolvedValue(undefined);
    render(<SharedStorageExportModal open busy={false} instanceId={1} onClose={vi.fn()}
      onLoadObjects={vi.fn().mockResolvedValue([])} onConfirmExport={save} />);
    await waitFor(() => expect(screen.getByRole("button", { name: "Add Folder" })).toBeDisabled());
    await screen.findByText("No files found to export.");
    fireEvent.change(screen.getByLabelText("VM folder path"), { target: { value: "Documents/My Project" } });
    fireEvent.click(screen.getByRole("button", { name: "Add Folder" }));
    fireEvent.click(screen.getByRole("button", { name: "Export Selected" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(["/folders/Documents/My Project"], "balanced"));
  });
});
