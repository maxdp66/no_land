import type { LaunchLibraryItem, PlayStats } from "../../lib/types";

export function formatPlayTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 60) {
    return "under a minute";
  }
  const totalMinutes = Math.floor(seconds / 60);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours === 0) {
    return `${minutes}m`;
  }
  return minutes === 0 ? `${hours}h` : `${hours}h ${minutes}m`;
}

export function formatLastPlayed(iso: string, now: Date = new Date()): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) {
    return "";
  }
  const days = Math.floor((now.getTime() - then) / 86_400_000);
  if (days <= 0) {
    return "today";
  }
  if (days === 1) {
    return "yesterday";
  }
  if (days < 30) {
    return `${days} days ago`;
  }
  return new Date(iso).toLocaleDateString();
}

export function playStatsLine(stats: PlayStats, now: Date = new Date()): string {
  const launches = `${stats.launchCount} launch${stats.launchCount === 1 ? "" : "es"}`;
  return `${formatPlayTime(stats.totalPlaySeconds)} played · ${launches} · last ${formatLastPlayed(stats.lastPlayedAt, now)}`;
}

/** Recently played first (most recent at the top), then the original order. */
export function sortByRecentPlay(items: LaunchLibraryItem[], history: PlayStats[]): LaunchLibraryItem[] {
  const lastPlayed = new Map(history.map((entry) => [entry.appId, new Date(entry.lastPlayedAt).getTime()]));
  return items
    .map((item, index) => ({ item, index, played: lastPlayed.get(item.appId) ?? null }))
    .sort((left, right) => {
      if (left.played !== null && right.played !== null) {
        return right.played - left.played;
      }
      if (left.played !== null) {
        return -1;
      }
      if (right.played !== null) {
        return 1;
      }
      return left.index - right.index;
    })
    .map(({ item }) => item);
}
