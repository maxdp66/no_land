import { TENSORDOCK_API_KEY_URL } from "../../lib/constants";
import { useAppStore } from "../../store/appStore";
import { ProviderKeySettings } from "./ProviderKeySettings";

interface Props {
  currentKey: string;
  busy: boolean;
}

export function TensorDockKeySettings({ currentKey, busy }: Props) {
  const saveTensordockApiKey = useAppStore((state) => state.saveTensordockApiKey);
  return (
    <ProviderKeySettings
      providerName="TensorDock"
      description="Add a TensorDock API key to include TensorDock virtual machines in server search. Offers from every provider are ranked together."
      keyPageUrl={TENSORDOCK_API_KEY_URL}
      currentKey={currentKey}
      busy={busy}
      testId="tensordock-key-settings"
      onSave={saveTensordockApiKey}
    />
  );
}
