import { Button } from "../../components/ui/Button";
import type { InstancePreemptedEvent } from "../../lib/types";

interface Props {
  events: InstancePreemptedEvent[];
  onFindReplacement: (event: InstancePreemptedEvent) => void;
  onDismiss: (instanceId: number) => void;
}

export function PreemptionBanner({ events, onFindReplacement, onDismiss }: Props) {
  if (events.length === 0) {
    return null;
  }
  return (
    <section className="space-y-2" aria-live="polite">
      {events.map((event) => (
        <div
          key={event.instanceId}
          role="alert"
          className="flex flex-wrap items-center justify-between gap-3 border-2 border-[#ffd166] bg-[#3c2c13] px-4 py-3 text-[#ffe0a3]"
        >
          <div className="min-w-0">
            <p className="font-display text-[11px] uppercase tracking-[0.08em]">
              {event.label}
              {event.gpuName ? ` · ${event.gpuName}` : ""} was interrupted
            </p>
            <p className="mt-1 text-[1rem]">
              {event.message} With shared storage set up, a replacement restores your last backup.
            </p>
          </div>
          <div className="flex gap-2">
            <Button onClick={() => onFindReplacement(event)}>Find a Replacement</Button>
            <Button variant="ghost" onClick={() => onDismiss(event.instanceId)}>
              Dismiss
            </Button>
          </div>
        </div>
      ))}
    </section>
  );
}
