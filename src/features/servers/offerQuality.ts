import type { OfferCandidate } from "../../lib/types";

export interface OfferQualityHint {
  text: string;
  tone: "good" | "fair" | "poor" | "estimate";
}

/** What this user can expect from an offer, from their own history first. */
export function offerQualityHint(offer: OfferCandidate): OfferQualityHint | null {
  const observed = offer.observedQuality;
  if (observed) {
    const rtt = observed.avgRttMs != null ? `${Math.round(observed.avgRttMs)} ms` : "no RTT data";
    const where = observed.basis === "host" ? "this host" : "this region";
    const sessions = `${observed.sessions} session${observed.sessions === 1 ? "" : "s"}`;
    const tone = observed.score >= 80 ? "good" : observed.score >= 60 ? "fair" : "poor";
    return { text: `Your history on ${where}: ${rtt} · ${sessions}`, tone };
  }
  if (offer.estimatedRttMs != null) {
    return { text: `~${Math.round(offer.estimatedRttMs)} ms estimated from distance`, tone: "estimate" };
  }
  return null;
}
