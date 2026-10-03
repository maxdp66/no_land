import { describe, expect, it } from "vitest";
import { buildDiagnosticIssueUrl } from "./githubIssue";
import type { DiagnosticReportResponse, SystemHealthReport } from "./types";

function health(summary: string, vastStatus: "ok" | "failed", vastSummary: string): SystemHealthReport {
  return {
    ok: vastStatus === "ok",
    checkedAtUnix: 1,
    os: "Linux",
    arch: "x64",
    summary,
    probes: [
      {
        id: "vast.credentials",
        label: "Vast.ai API key",
        category: "vast",
        status: vastStatus,
        summary: vastSummary,
        details: null,
        fixHint: null,
      },
    ],
  };
}

function issueBody(url: string): string {
  return new URL(url).searchParams.get("body") ?? "";
}

describe("buildDiagnosticIssueUrl", () => {
  it("summarizes from the health embedded in the report, not a stale snapshot", () => {
    const fresh = health("All local health checks passed.", "ok", "Vast API key is present and accepted.");
    const stale = health("1 blocking issue(s), 0 warning(s).", "failed", "Vast API key is missing.");
    const report: DiagnosticReportResponse = {
      path: "/tmp/report.md",
      summary: fresh.summary,
      reportMarkdown: "- `Ok` **Vast.ai API key**: Vast API key is present and accepted.",
      health: fresh,
    };

    const body = issueBody(buildDiagnosticIssueUrl({ report, reason: "manual", health: stale }));
    expect(body).toContain("Health summary: All local health checks passed.");
    expect(body).not.toContain("Vast API key is missing.");
    expect(body).not.toContain("## Blocking health checks");
  });

  it("falls back to the caller's health when the report has none", () => {
    const stale = health("1 blocking issue(s), 0 warning(s).", "failed", "Vast API key is missing.");
    const report: DiagnosticReportResponse = { path: "/tmp/r.md", summary: "x", reportMarkdown: "" };
    const body = issueBody(buildDiagnosticIssueUrl({ report, reason: "manual", health: stale }));
    expect(body).toContain("Vast.ai API key: Vast API key is missing.");
  });
});
