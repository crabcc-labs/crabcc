// Pure tests for the agents-panel store. No DOM, no React.

import { describe, expect, it } from "bun:test";
import {
  dayBucketFor,
  filterAgents,
  filterKills,
  filterModels,
  filterProfiles,
  modelKey,
  profilesInUse,
  sortAgents,
  sortModels,
  todayMidnightSecs,
  uptimeLabel,
  withDayHeaders,
} from "./store";
import type {
  AgentKillRow,
  AgentModelEntry,
  AgentProfileEntry,
  AgentSummary,
} from "./types";

const NOW = 1_700_000_000;

function agent(p: Partial<AgentSummary> & { id: string }): AgentSummary {
  return {
    status: "running",
    started_ts: NOW - 60,
    ...p,
  } as AgentSummary;
}

describe("filterAgents", () => {
  const agents: AgentSummary[] = [
    agent({ id: "abc-1", prompt_preview: "Find Store::open", model: "qwen2.5", pid: 1234 }),
    agent({ id: "def-2", prompt_preview: "Refactor login", model: "claude-3", pid: 5678, status: "exited" }),
    agent({ id: "abc-3", prompt_preview: "Update docs", model: "qwen2.5", pid: 9012 }),
  ];

  it("returns input by reference when text is empty", () => {
    expect(filterAgents(agents, "")).toBe(agents);
    expect(filterAgents(agents, "   ")).toBe(agents);
  });

  it("filters by id substring (case-insensitive)", () => {
    const out = filterAgents(agents, "ABC");
    expect(out).toHaveLength(2);
  });

  it("filters by prompt preview", () => {
    const out = filterAgents(agents, "store");
    expect(out).toHaveLength(1);
    expect(out[0].id).toBe("abc-1");
  });

  it("filters by model", () => {
    const out = filterAgents(agents, "qwen");
    expect(out).toHaveLength(2);
  });

  it("filters by status", () => {
    const out = filterAgents(agents, "exited");
    expect(out).toHaveLength(1);
    expect(out[0].id).toBe("def-2");
  });

  it("filters by pid", () => {
    const out = filterAgents(agents, "5678");
    expect(out).toHaveLength(1);
  });
});

describe("sortAgents", () => {
  const agents: AgentSummary[] = [
    agent({ id: "old-running", started_ts: NOW - 1000 }),
    agent({ id: "fresh-running", started_ts: NOW - 10 }),
    agent({ id: "exited-old", started_ts: NOW - 500, status: "exited" }),
  ];

  it("sorts by started_ts newest-first", () => {
    const out = sortAgents(agents, "started", NOW);
    expect(out.map((a) => a.id)).toEqual(["fresh-running", "exited-old", "old-running"]);
  });

  it("sorts running before exited under 'status'", () => {
    const out = sortAgents(agents, "status", NOW);
    expect(out[0].status).toBe("running");
    expect(out[1].status).toBe("running");
    expect(out[2].status).toBe("exited");
  });

  it("sorts by uptime longest-first; exited at the bottom", () => {
    const out = sortAgents(agents, "uptime", NOW);
    expect(out[0].id).toBe("old-running");
    expect(out[1].id).toBe("fresh-running");
    expect(out[2].id).toBe("exited-old");
  });

  it("does not mutate input", () => {
    const before = agents.map((a) => a.id);
    sortAgents(agents, "started", NOW);
    expect(agents.map((a) => a.id)).toEqual(before);
  });
});

describe("uptimeLabel", () => {
  it("returns — for exited agents", () => {
    expect(uptimeLabel(agent({ id: "x", status: "exited" }), NOW)).toBe("—");
  });
  it("formats seconds", () => {
    expect(uptimeLabel(agent({ id: "x", started_ts: NOW - 30 }), NOW)).toBe("30s");
  });
  it("formats minutes", () => {
    expect(uptimeLabel(agent({ id: "x", started_ts: NOW - 120 }), NOW)).toBe("2m");
  });
  it("formats hours", () => {
    expect(uptimeLabel(agent({ id: "x", started_ts: NOW - 7200 }), NOW)).toBe("2h");
  });
});

describe("filterProfiles", () => {
  const profiles: AgentProfileEntry[] = [
    { id: "doc-writer", crate_: "crabcc-cli", description: "Writes docs", model: "qwen2.5" },
    { id: "test-runner", crate_: null, description: null, model: "claude-3" },
  ];
  it("filters by id, crate, or description", () => {
    expect(filterProfiles(profiles, "doc")).toHaveLength(1);
    expect(filterProfiles(profiles, "cli")).toHaveLength(1);
    expect(filterProfiles(profiles, "claude")).toHaveLength(1);
  });
  it("returns input by reference on empty", () => {
    expect(filterProfiles(profiles, "")).toBe(profiles);
  });
});

describe("profilesInUse", () => {
  it("flags profiles whose id matches a running agent", () => {
    const profiles: AgentProfileEntry[] = [
      { id: "p1", crate_: null, description: null, model: null },
      { id: "p2", crate_: null, description: null, model: null },
    ];
    const agents: AgentSummary[] = [agent({ id: "p1" })];
    const used = profilesInUse(profiles, agents);
    expect(used.has("p1")).toBe(true);
    expect(used.has("p2")).toBe(false);
  });
  it("flags profiles whose model matches a running agent's model", () => {
    const profiles: AgentProfileEntry[] = [
      { id: "p1", crate_: null, description: null, model: "qwen2.5" },
    ];
    const agents: AgentSummary[] = [agent({ id: "x", model: "qwen2.5" })];
    expect(profilesInUse(profiles, agents).has("p1")).toBe(true);
  });
  it("ignores exited agents", () => {
    const profiles: AgentProfileEntry[] = [
      { id: "p1", crate_: null, description: null, model: null },
    ];
    const agents: AgentSummary[] = [agent({ id: "p1", status: "exited" })];
    expect(profilesInUse(profiles, agents).size).toBe(0);
  });
});

describe("filterKills", () => {
  const rows: AgentKillRow[] = [
    { run_id: "run-aaa", reason: "zombie", pid: 100, detail: "no heartbeat", killed_at: NOW },
    { run_id: "run-bbb", reason: "stuck", pid: 200, detail: null, killed_at: NOW - 10 },
  ];
  it("filters by reason / run_id / pid", () => {
    expect(filterKills(rows, "zombie")).toHaveLength(1);
    expect(filterKills(rows, "bbb")).toHaveLength(1);
    expect(filterKills(rows, "200")).toHaveLength(1);
  });
});

describe("dayBucketFor / withDayHeaders", () => {
  // Build a deterministic local-midnight reference. Use NOW.
  const today = todayMidnightSecs(NOW);
  it("Today / Yesterday / weekday / Mon Day", () => {
    expect(dayBucketFor(today + 100, today)).toBe("Today");
    expect(dayBucketFor(today - 100, today)).toBe("Yesterday");
    // 3 days ago — short weekday.
    const w = dayBucketFor(today - 3 * 86_400, today);
    expect(w.length).toBeGreaterThan(0);
    // 30 days ago — month-day. Spot-check the format ("Apr 1" / "Apr 01")
    // without locking the test to a specific locale.
    const md = dayBucketFor(today - 30 * 86_400, today);
    expect(md.length).toBeGreaterThan(2);
    expect(md.includes(" ")).toBe(true);
  });

  it("inserts sticky headers between rows", () => {
    const today = todayMidnightSecs(NOW);
    const rows: AgentKillRow[] = [
      { run_id: "a", reason: "zombie", pid: 1, detail: null, killed_at: today + 60 },
      { run_id: "b", reason: "zombie", pid: 1, detail: null, killed_at: today + 30 },
      { run_id: "c", reason: "stuck", pid: 1, detail: null, killed_at: today - 60 },
    ];
    const out = withDayHeaders(rows, today);
    expect(out[0].kind).toBe("header");
    expect((out[0] as { label: string }).label).toBe("Today");
    expect(out[1].kind).toBe("kill");
    expect(out[2].kind).toBe("kill");
    expect(out[3].kind).toBe("header");
    expect((out[3] as { label: string }).label).toBe("Yesterday");
    expect(out[4].kind).toBe("kill");
  });
});

describe("filterModels / sortModels", () => {
  const models: AgentModelEntry[] = [
    { file: "f1", provider: "ollama", name: "qwen2.5-coder", params: "7b", context: 32000, docs_first: null },
    { file: "f2", provider: "anthropic", name: "claude-sonnet", params: null, context: 200000, docs_first: null },
    { file: "f3", provider: "ollama", name: "llama3", params: "8b", context: 8192, docs_first: null },
  ];
  it("filters by provider or name", () => {
    expect(filterModels(models, "ollama")).toHaveLength(2);
    expect(filterModels(models, "claude")).toHaveLength(1);
  });
  it("sorts by provider then name", () => {
    const out = sortModels(models);
    expect(out.map((m) => m.name)).toEqual([
      "claude-sonnet",
      "llama3",
      "qwen2.5-coder",
    ]);
  });
  it("modelKey is provider:name", () => {
    expect(modelKey(models[0])).toBe("ollama:qwen2.5-coder");
  });
});
