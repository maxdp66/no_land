import { describe, expect, it } from "vitest";
import type { OfferCandidate } from "../../lib/types";
import { offerQualityHint } from "./offerQuality";

const base = {} as OfferCandidate;

describe("offerQualityHint", () => {
  it("prefers the user's own history", () => {
    expect(
      offerQualityHint({
        ...base,
        observedQuality: { score: 92, sessions: 3, avgRttMs: 18.4, basis: "host" },
        estimatedRttMs: 40,
      }),
    ).toEqual({ text: "Your history on this host: 18 ms · 3 sessions", tone: "good" });
    expect(
      offerQualityHint({ ...base, observedQuality: { score: 40, sessions: 1, avgRttMs: null, basis: "region" } }),
    ).toEqual({ text: "Your history on this region: no RTT data · 1 session", tone: "poor" });
  });

  it("falls back to the distance estimate", () => {
    expect(offerQualityHint({ ...base, estimatedRttMs: 23 })).toEqual({
      text: "~23 ms estimated from distance",
      tone: "estimate",
    });
    expect(offerQualityHint(base)).toBeNull();
  });
});
