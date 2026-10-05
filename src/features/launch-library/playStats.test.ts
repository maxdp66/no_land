import { describe, expect, it } from "vitest";
import type { LaunchLibraryItem, PlayStats } from "../../lib/types";
import { formatLastPlayed, formatPlayTime, playStatsLine, sortByRecentPlay } from "./playStats";

const item = (appId: string) => ({ appId }) as LaunchLibraryItem;
const stats = (appId: string, lastPlayedAt: string, seconds = 3600): PlayStats => ({
  appId,
  displayName: appId,
  totalPlaySeconds: seconds,
  launchCount: 2,
  lastPlayedAt,
});

describe("playStats", () => {
  it("formats play time", () => {
    expect(formatPlayTime(30)).toBe("under a minute");
    expect(formatPlayTime(25 * 60)).toBe("25m");
    expect(formatPlayTime(2 * 3600)).toBe("2h");
    expect(formatPlayTime(2 * 3600 + 5 * 60)).toBe("2h 5m");
  });

  it("formats last played", () => {
    const now = new Date("2026-10-05T12:00:00Z");
    expect(formatLastPlayed("2026-10-05T08:00:00Z", now)).toBe("today");
    expect(formatLastPlayed("2026-10-04T08:00:00Z", now)).toBe("yesterday");
    expect(formatLastPlayed("2026-09-30T08:00:00Z", now)).toBe("5 days ago");
    expect(playStatsLine(stats("x", "2026-10-04T08:00:00Z", 5400), now)).toBe(
      "1h 30m played · 2 launches · last yesterday",
    );
  });

  it("puts recently played games first", () => {
    const sorted = sortByRecentPlay(
      [item("a"), item("b"), item("c"), item("d")],
      [stats("c", "2026-10-01T00:00:00Z"), stats("d", "2026-10-04T00:00:00Z")],
    );
    expect(sorted.map((entry) => entry.appId)).toEqual(["d", "c", "a", "b"]);
  });
});
