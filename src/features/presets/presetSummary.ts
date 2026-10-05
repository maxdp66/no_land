import type { ServerPreset } from "../../lib/types";

/** One-line description of what a preset will search for and stream at. */
export function presetSummary(preset: ServerPreset): string {
  const prefs = preset.serverPreferences;
  const parts: string[] = [];
  if (prefs.minGpuRamGb > 0) {
    parts.push(`${prefs.minGpuRamGb}GB+ VRAM`);
  }
  const country = prefs.geolocationCountryCode?.trim();
  parts.push(country && country.toUpperCase() !== "GLOBAL" ? country.toUpperCase() : "any region");
  if (prefs.maxHourlyPrice > 0) {
    parts.push(`≤ $${prefs.maxHourlyPrice.toFixed(2)}/hr`);
  }
  parts.push(`${prefs.storageGb}GB disk`);
  const { width, height, fps, bitrate } = preset.stream;
  parts.push(`${width}×${height}@${fps} · ${Math.round(bitrate / 1000)} Mbps`);
  return parts.join(" · ");
}
