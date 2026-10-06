import { SHADEFORM_API_KEY_URL } from "../../lib/constants";
import { useAppStore } from "../../store/appStore";
import { ProviderKeySettings } from "./ProviderKeySettings";

interface Props {
  currentKey: string;
  busy: boolean;
}

export function ShadeformKeySettings({ currentKey, busy }: Props) {
  const saveShadeformApiKey = useAppStore((state) => state.saveShadeformApiKey);
  return (
    <ProviderKeySettings
      providerName="Shadeform"
      description="Add a Shadeform API key to search VMs from the GPU clouds Shadeform resells (Lambda, Massed Compute, Hyperstack and others). Only single-GPU machines with a video encoder are shown. Shadeform servers can be destroyed but not stopped."
      keyPageUrl={SHADEFORM_API_KEY_URL}
      currentKey={currentKey}
      busy={busy}
      testId="shadeform-key-settings"
      onSave={saveShadeformApiKey}
    />
  );
}
