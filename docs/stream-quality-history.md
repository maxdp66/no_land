# Stream Quality History

No Land remembers how well each streaming session went and uses that to rank
offers and to set expectations before renting.

## Recording

`src-tauri/src/services/quality_recorder.rs` samples the Moonlight runtime
every 5 seconds while the stream state is `streaming` and the statistics are
fresh (≤ 3 s old). Each sample records:

- control-channel RTT and RTT variation (`estimated_rtt_ms`, `estimated_rtt_variance_ms`);
- submitted FPS, missing-frames percent and video Mbps.

When the stream stops or switches instance, the samples become a
`SessionQualityRecord` (`models/quality.rs`) in `state.qualityHistory`.
Sessions under 6 samples (30 s) are dropped, and only the newest 200
records are kept. Each record is tagged with the provider, host id, GPU and
city/region/country of the offer the instance was rented from, when known.
`quality:recorded` is emitted for each new record.

## Score

`quality_score` gives 0–100: full marks up to 15 ms RTT, then −1 per ms (max
−50), −2 per ms of RTT variation (max −25), and −5 per percent of missing
frames (max −25). Below 60 (`POOR_SCORE`) counts as a poor experience.

## Use in ranking

`OfferSelector::rank_offers_with_history` annotates each offer with:

- `observedQuality`: average score, average RTT and session count for the same
  provider host, otherwise for the same country/region;
- `estimatedRttMs`: `distance_km / 100 + 8`, a rough fiber estimate used
  when there is no history.

Offers with a poor observed score sort after the others in the same
location tier, ahead of price. The server picker shows the history line
(green, amber or red), or the distance estimate when there is no history.
