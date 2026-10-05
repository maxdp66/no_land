/** Display name for a GPU provider id ("vast", "tensordock", "shadeform"). */
export function providerDisplayName(provider: string | null | undefined): string {
  switch ((provider ?? "").trim().toLowerCase()) {
    case "tensordock":
      return "TensorDock";
    case "shadeform":
      return "Shadeform";
    default:
      return "Vast.ai";
  }
}
