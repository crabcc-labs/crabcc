import { describe, expect, it } from "bun:test";
import type { ActivityHit, AgentSummary, TelemetryEvent } from "../../api";
import {
  activitySparkline,
  eventsPerMinute,
  fmtAge,
  levelBreakdown,
  runningAgents,
  topRecentEvents,
  topTargets,
} from "./selectors";

const now = 1_700_000_000;

function mkActivity(deltaSec: number): ActivityHit {
  return { ts: now - deltaSec, op: "sym", query: "Foo", count: 1 };
}

function mkEvent(
  level: TelemetryEvent["level"],
  target: string,
  deltaSec = 0,
): TelemetryEvent {
  return { ts: now - deltaSec, level, target, fields: { message: `${target} hit` } };
}

describe("activitySparkline", () => {
  it("buckets items into N equal-width windows ending at now", () => {
    const items = [mkActivity(10), mkActivity(40), mkActivity(70), mkActivity(700)];
    const out = activitySparkline(items, now, 60, 6);
    expect(out).toHaveLength(6);
    // 60s window, 6 buckets → 10s wide. Items at 10s and 40s are inside;
    // 70s is outside (cutoff is now-60). 700s is way outside.
    const total = out.reduce((s, b) => s + b.count, 0);
    expect(total).toBe(2);
  });

  it("returns oldest → newest so a flex row renders chronologically", () => {
    const items = [mkActivity(50), mkActivity(5)];
    const out = activitySparkline(items, now, 60, 6);
    expect(out[0]!.bucketStart).toBeLessThan(out[5]!.bucketStart);
  });

  it("survives an empty input", () => {
    const out = activitySparkline([], now, 60, 5);
    expect(out).toHaveLength(5);
    expect(out.every((b) => b.count === 0)).toBe(true);
  });
});

describe("eventsPerMinute", () => {
  it("counts items inside the trailing 60s window", () => {
    expect(eventsPerMinute([mkActivity(10), mkActivity(50), mkActivity(70)], now)).toBe(2);
  });
  it("returns 0 when nothing is fresh", () => {
    expect(eventsPerMinute([mkActivity(120)], now)).toBe(0);
  });
});

describe("runningAgents", () => {
  it("filters by status", () => {
    const agents: AgentSummary[] = [
      { id: "a", status: "running", started_ts: now },
      { id: "b", status: "exited", started_ts: now },
    ];
    expect(runningAgents(agents).map((a) => a.id)).toEqual(["a"]);
  });
});

describe("topRecentEvents", () => {
  it("returns descending by ts and respects N", () => {
    const events = [mkEvent("INFO", "x", 5), mkEvent("INFO", "y", 1), mkEvent("INFO", "z", 10)];
    const out = topRecentEvents(events, 2);
    expect(out.map((e) => e.target)).toEqual(["y", "x"]);
  });
});

describe("levelBreakdown", () => {
  it("returns the canonical 5 levels in order with counts", () => {
    const events = [
      mkEvent("INFO", "a"),
      mkEvent("INFO", "b"),
      mkEvent("ERROR", "c"),
      mkEvent("DEBUG", "d"),
    ];
    const out = levelBreakdown(events);
    expect(out.map((c) => c.level)).toEqual(["TRACE", "DEBUG", "INFO", "WARN", "ERROR"]);
    expect(out.find((c) => c.level === "INFO")!.count).toBe(2);
    expect(out.find((c) => c.level === "ERROR")!.count).toBe(1);
  });
});

describe("topTargets", () => {
  it("ranks unique targets by frequency descending", () => {
    const events = [
      mkEvent("INFO", "a"),
      mkEvent("INFO", "a"),
      mkEvent("INFO", "b"),
    ];
    expect(topTargets(events, 2)).toEqual(["a", "b"]);
  });
});

describe("fmtAge", () => {
  it("renders short labels for each magnitude", () => {
    expect(fmtAge(5)).toBe("5s");
    expect(fmtAge(120)).toBe("2m");
    expect(fmtAge(7200)).toBe("2h");
    expect(fmtAge(172_800)).toBe("2d");
  });
  it("clamps negatives to 0s", () => {
    expect(fmtAge(-30)).toBe("0s");
  });
});
