// Slim orchestrator for the consolidated agents feature module. Owns:
//   - the active tab (live | profiles | kills | models)
//   - per-tab filter text + sort mode + selection / expansion
//   - the search-input ref so "/" can focus regardless of tab
//   - keyboard wiring (1-4 / / / Esc / ↑ ↓ / Enter / r)
//
// Every slice of derivation lives in the hook layer
// (useAgentsData, useAgentLog, useAgentTab) and every chunk of
// rendering is a small component below `components/agents/`. The
// orchestrator's job is composition only — keep it that way.

import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { logMount, logUnmount } from "../../lifecycle";
import { useNow } from "../../useNow";
import { AgentDetail } from "./AgentDetail";
import { AgentRow } from "./AgentRow";
import { AgentSearch } from "./AgentSearch";
import { AgentTabs } from "./AgentTabs";
import { KillRow } from "./KillRow";
import { ModelRow } from "./ModelRow";
import { ProfileRow } from "./ProfileRow";
import {
  filterAgents,
  filterKills,
  filterModels,
  filterProfiles,
  modelKey,
  profilesInUse,
  sortAgents,
  sortModels,
  todayMidnightSecs,
  withDayHeaders,
} from "./store";
import type {
  AgentSummary,
  KillRow as KillRowModel,
  LiveSort,
  TabId,
} from "./types";
import { useAgentsData } from "./useAgentsData";
import { useAgentTab } from "./useAgentTab";
import { useKeyboardControls } from "./useKeyboardControls";

interface Props {
  agents: AgentSummary[];
}

// `now` was previously threaded down from App.tsx as a prop, which
// dirtied this memo every second and re-rendered every visible agent
// row. The `useNow` hook now subscribes only at the ticks the panel
// is actually mounted; the panel re-renders on tick, but App.tsx and
// its sibling subtrees (graph, telemetry) no longer do.
export const AgentsPanel = memo(function AgentsPanel({ agents }: Props) {
  const now = useNow();
  useEffect(() => {
    logMount("AgentsPanel");
    return () => logUnmount("AgentsPanel");
  }, []);

  const { tab, setTab, setTabByIndex } = useAgentTab("live");
  const data = useAgentsData();

  // Per-tab text + per-tab selection / expansion. Each tab keeps its
  // own state so switching back doesn't clobber where you were.
  const [liveText, setLiveText] = useState("");
  const [liveSort, setLiveSort] = useState<LiveSort>("started");
  const [profileText, setProfileText] = useState("");
  const [killText, setKillText] = useState("");
  const [modelText, setModelText] = useState("");

  const [liveSelected, setLiveSelected] = useState<string | null>(null);
  const [liveExpanded, setLiveExpanded] = useState<string | null>(null);
  const [livePinned, setLivePinned] = useState<Set<string>>(new Set());
  const [profileSelected, setProfileSelected] = useState<string | null>(null);
  const [profileExpanded, setProfileExpanded] = useState<string | null>(null);
  const [killSelected, setKillSelected] = useState<string | null>(null);
  const [killExpanded, setKillExpanded] = useState<string | null>(null);
  const [modelSelected, setModelSelected] = useState<string | null>(null);
  const [modelExpanded, setModelExpanded] = useState<string | null>(null);

  // ── derivations ───────────────────────────────────────────────────
  const liveFiltered = useMemo(
    () => sortAgents(filterAgents(agents, liveText), liveSort, now),
    [agents, liveText, liveSort, now],
  );
  const profilesFiltered = useMemo(
    () => filterProfiles(data.profiles, profileText),
    [data.profiles, profileText],
  );
  const inUse = useMemo(
    () => profilesInUse(data.profiles, agents),
    [data.profiles, agents],
  );
  const killsFiltered = useMemo(
    () => filterKills(data.kills, killText),
    [data.kills, killText],
  );
  const killRows = useMemo<KillRowModel[]>(
    () => withDayHeaders(killsFiltered, todayMidnightSecs(now)),
    [killsFiltered, now],
  );
  const modelsFiltered = useMemo(
    () => sortModels(filterModels(data.models, modelText)),
    [data.models, modelText],
  );

  const counts = useMemo<Record<TabId, number>>(
    () => ({
      live: agents.length,
      profiles: data.profiles.length,
      kills: data.kills.length,
      models: data.models.length,
    }),
    [agents.length, data.profiles.length, data.kills.length, data.models.length],
  );

  // Auto-collapse a running→exited transition unless the user pinned it.
  // We track the previous status per id so we react only to the edge.
  const prevStatus = useRef<Map<string, "running" | "exited">>(new Map());
  useEffect(() => {
    const next = new Map<string, "running" | "exited">();
    for (const a of agents) {
      const before = prevStatus.current.get(a.id);
      if (
        before === "running" &&
        a.status === "exited" &&
        liveExpanded === a.id &&
        !livePinned.has(a.id)
      ) {
        // The agent we had expanded just exited; collapse unless pinned.
        setLiveExpanded(null);
      }
      next.set(a.id, a.status);
    }
    prevStatus.current = next;
  }, [agents, liveExpanded, livePinned]);

  // ── keyboard wiring ──────────────────────────────────────────────
  const searchRef = useRef<HTMLInputElement>(null);

  const focusSearch = useCallback(() => {
    searchRef.current?.focus();
    searchRef.current?.select();
  }, []);

  const clearOrCollapse = useCallback(() => {
    // Tab-aware: if there's text, clear it; else collapse expansion;
    // else clear selection. Mirrors the activity panel's "back-out
    // ladder" of escape stages.
    const text =
      tab === "live"
        ? liveText
        : tab === "profiles"
          ? profileText
          : tab === "kills"
            ? killText
            : modelText;
    if (text) {
      switch (tab) {
        case "live":
          setLiveText("");
          break;
        case "profiles":
          setProfileText("");
          break;
        case "kills":
          setKillText("");
          break;
        case "models":
          setModelText("");
          break;
      }
      return;
    }
    switch (tab) {
      case "live":
        if (liveExpanded) setLiveExpanded(null);
        else setLiveSelected(null);
        break;
      case "profiles":
        if (profileExpanded) setProfileExpanded(null);
        else setProfileSelected(null);
        break;
      case "kills":
        if (killExpanded) setKillExpanded(null);
        else setKillSelected(null);
        break;
      case "models":
        if (modelExpanded) setModelExpanded(null);
        else setModelSelected(null);
        break;
    }
  }, [
    tab,
    liveText,
    profileText,
    killText,
    modelText,
    liveExpanded,
    profileExpanded,
    killExpanded,
    modelExpanded,
  ]);

  // Selectable keys per tab — drives ↑/↓.
  const liveKeys = useMemo(() => liveFiltered.map((a) => a.id), [liveFiltered]);
  const profileKeys = useMemo(
    () => profilesFiltered.map((p) => p.id),
    [profilesFiltered],
  );
  // Skip headers when navigating kills.
  const killKeys = useMemo(
    () => killRows.filter((r) => r.kind === "kill").map((r) => r.key),
    [killRows],
  );
  const modelKeys = useMemo(
    () => modelsFiltered.map(modelKey),
    [modelsFiltered],
  );

  const selectStep = useCallback(
    (dir: 1 | -1) => {
      const [keys, current, set] = ((): [
        string[],
        string | null,
        (k: string | null) => void,
      ] => {
        switch (tab) {
          case "live":
            return [liveKeys, liveSelected, setLiveSelected];
          case "profiles":
            return [profileKeys, profileSelected, setProfileSelected];
          case "kills":
            return [killKeys, killSelected, setKillSelected];
          case "models":
            return [modelKeys, modelSelected, setModelSelected];
        }
      })();
      if (keys.length === 0) {
        set(null);
        return;
      }
      const idx = current !== null ? keys.indexOf(current) : -1;
      let next = (idx + dir + keys.length) % keys.length;
      if (idx < 0 && dir < 0) next = keys.length - 1;
      if (idx < 0 && dir > 0) next = 0;
      set(keys[next]);
    },
    [
      tab,
      liveKeys,
      liveSelected,
      profileKeys,
      profileSelected,
      killKeys,
      killSelected,
      modelKeys,
      modelSelected,
    ],
  );

  const selectPrev = useCallback(() => selectStep(-1), [selectStep]);
  const selectNext = useCallback(() => selectStep(+1), [selectStep]);

  const openSelected = useCallback(() => {
    switch (tab) {
      case "live":
        if (liveSelected) {
          setLiveExpanded((v) => (v === liveSelected ? null : liveSelected));
        }
        break;
      case "profiles":
        if (profileSelected) {
          setProfileExpanded((v) => (v === profileSelected ? null : profileSelected));
        }
        break;
      case "kills":
        if (killSelected) {
          setKillExpanded((v) => (v === killSelected ? null : killSelected));
        }
        break;
      case "models":
        if (modelSelected) {
          setModelExpanded((v) => (v === modelSelected ? null : modelSelected));
        }
        break;
    }
  }, [tab, liveSelected, profileSelected, killSelected, modelSelected]);

  const refresh = useCallback(() => {
    switch (tab) {
      case "live":
        // Live agents flow over SSE — no manual refetch path. The
        // user-visible affordance still exists in the search bar so
        // muscle memory holds; the action is a no-op here.
        break;
      case "profiles":
        data.refetchProfiles();
        break;
      case "kills":
        data.refetchKills();
        break;
      case "models":
        data.refetchModels();
        break;
    }
  }, [tab, data]);

  useKeyboardControls(
    useMemo(
      () => ({
        setTabByIndex,
        focusSearch,
        clearOrCollapse,
        selectPrev,
        selectNext,
        openSelected,
        refresh,
      }),
      [
        setTabByIndex,
        focusSearch,
        clearOrCollapse,
        selectPrev,
        selectNext,
        openSelected,
        refresh,
      ],
    ),
    true,
  );

  // ── render ────────────────────────────────────────────────────────
  return (
    <div className="agents-panel">
      <AgentTabs active={tab} onPick={setTab} counts={counts} />
      {tab === "live" ? (
        <LiveTab
          agents={liveFiltered}
          totalAll={agents.length}
          text={liveText}
          onText={setLiveText}
          sort={liveSort}
          onSort={setLiveSort}
          selected={liveSelected}
          expanded={liveExpanded}
          pinnedIds={livePinned}
          now={now}
          searchRef={searchRef}
          onPick={(id) => {
            setLiveSelected(id);
            setLiveExpanded((v) => (v === id ? null : id));
          }}
          onTogglePin={(id) =>
            setLivePinned((s) => {
              const n = new Set(s);
              if (n.has(id)) n.delete(id);
              else n.add(id);
              return n;
            })
          }
          onCollapse={() => setLiveExpanded(null)}
        />
      ) : null}
      {tab === "profiles" ? (
        <ProfilesTab
          profiles={profilesFiltered}
          inUse={inUse}
          totalAll={data.profiles.length}
          dir={data.profilesDir}
          error={data.profilesError}
          text={profileText}
          onText={setProfileText}
          selected={profileSelected}
          expanded={profileExpanded}
          searchRef={searchRef}
          onRefresh={data.refetchProfiles}
          onPick={(id) => {
            setProfileSelected(id);
            setProfileExpanded((v) => (v === id ? null : id));
          }}
        />
      ) : null}
      {tab === "kills" ? (
        <KillsTab
          rows={killRows}
          totalAll={data.kills.length}
          totalShown={killsFiltered.length}
          db={data.killsDb}
          error={data.killsError}
          text={killText}
          onText={setKillText}
          selected={killSelected}
          expanded={killExpanded}
          searchRef={searchRef}
          onRefresh={data.refetchKills}
          onPick={(key) => {
            setKillSelected(key);
            setKillExpanded((v) => (v === key ? null : key));
          }}
        />
      ) : null}
      {tab === "models" ? (
        <ModelsTab
          models={modelsFiltered}
          totalAll={data.models.length}
          dir={data.modelsDir}
          error={data.modelsError}
          text={modelText}
          onText={setModelText}
          selected={modelSelected}
          expanded={modelExpanded}
          searchRef={searchRef}
          onRefresh={data.refetchModels}
          onPick={(key) => {
            setModelSelected(key);
            setModelExpanded((v) => (v === key ? null : key));
          }}
        />
      ) : null}
    </div>
  );
});

// ── Per-tab views ────────────────────────────────────────────────────
// These are kept inline because they're slim ribbons — search bar +
// list + (sometimes) detail. Hoisting them into separate files would
// just bounce three more imports per tab without buying readability.

interface LiveTabProps {
  agents: AgentSummary[];
  totalAll: number;
  text: string;
  onText(v: string): void;
  sort: LiveSort;
  onSort(v: LiveSort): void;
  selected: string | null;
  expanded: string | null;
  pinnedIds: ReadonlySet<string>;
  now: number;
  searchRef: React.RefObject<HTMLInputElement | null>;
  onPick(id: string): void;
  onTogglePin(id: string): void;
  onCollapse(): void;
}

function LiveTab({
  agents,
  totalAll,
  text,
  onText,
  sort,
  onSort,
  selected,
  expanded,
  pinnedIds,
  now,
  searchRef,
  onPick,
  onTogglePin,
  onCollapse,
}: LiveTabProps) {
  const expandedAgent = expanded ? agents.find((a) => a.id === expanded) : null;
  return (
    <div className="agents-tab-body">
      <AgentSearch
        ref={searchRef}
        value={text}
        onChange={onText}
        placeholder="Filter agents… ( / )"
        totalShown={agents.length}
        totalAll={totalAll}
        controls={
          <select
            className="agents-sort"
            value={sort}
            onChange={(e) => onSort(e.target.value as LiveSort)}
            aria-label="Sort"
          >
            <option value="started">newest</option>
            <option value="status">status</option>
            <option value="uptime">uptime</option>
          </select>
        }
      />
      {totalAll === 0 ? (
        <div className="empty">No agent runs yet.</div>
      ) : agents.length === 0 ? (
        <div className="empty">No agents match.</div>
      ) : (
        <div className="agents-list">
          {agents.map((a) => (
            <div key={a.id}>
              <AgentRow
                agent={a}
                selected={a.id === selected}
                expanded={a.id === expanded}
                now={now}
                onPick={() => onPick(a.id)}
              />
              {a.id === expanded && expandedAgent ? (
                <AgentDetail
                  agent={expandedAgent}
                  pinned={pinnedIds.has(expandedAgent.id)}
                  now={now}
                  onTogglePin={() => onTogglePin(expandedAgent.id)}
                  onClose={onCollapse}
                />
              ) : null}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

interface ProfilesTabProps {
  profiles: ReturnType<typeof filterProfiles> extends infer T
    ? T extends Array<infer U>
      ? U[]
      : never
    : never;
  inUse: ReadonlySet<string>;
  totalAll: number;
  dir: string;
  error: string | null;
  text: string;
  onText(v: string): void;
  selected: string | null;
  expanded: string | null;
  searchRef: React.RefObject<HTMLInputElement | null>;
  onRefresh(): void;
  onPick(id: string): void;
}

function ProfilesTab({
  profiles,
  inUse,
  totalAll,
  dir,
  error,
  text,
  onText,
  selected,
  expanded,
  searchRef,
  onRefresh,
  onPick,
}: ProfilesTabProps) {
  return (
    <div className="agents-tab-body">
      <AgentSearch
        ref={searchRef}
        value={text}
        onChange={onText}
        placeholder="Filter profiles… ( / )"
        totalShown={profiles.length}
        totalAll={totalAll}
        onRefresh={onRefresh}
      />
      {error ? (
        <div className="empty">profiles unavailable: {error}</div>
      ) : totalAll === 0 ? (
        <div className="empty">
          no profiles in <code>{dir || "internal_agents/"}</code>
        </div>
      ) : profiles.length === 0 ? (
        <div className="empty">No profiles match.</div>
      ) : (
        <div className="agents-list">
          <div className="agents-source-line">
            <code>{dir}</code>
          </div>
          {profiles.map((p) => (
            <div key={p.id}>
              <ProfileRow
                profile={p}
                selected={p.id === selected}
                expanded={p.id === expanded}
                inUse={inUse.has(p.id)}
                onPick={() => onPick(p.id)}
              />
              {p.id === expanded ? (
                <pre className="agents-profile-detail">
                  {`# ${p.id}\n` +
                    (p.crate_ ? `crate = "${p.crate_}"\n` : "") +
                    (p.model ? `model = "${p.model}"\n` : "") +
                    (p.description ? `description = """\n${p.description}\n"""\n` : "")}
                </pre>
              ) : null}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

interface KillsTabProps {
  rows: KillRowModel[];
  totalAll: number;
  totalShown: number;
  db: string;
  error: string | null;
  text: string;
  onText(v: string): void;
  selected: string | null;
  expanded: string | null;
  searchRef: React.RefObject<HTMLInputElement | null>;
  onRefresh(): void;
  onPick(key: string): void;
}

function KillsTab({
  rows,
  totalAll,
  totalShown,
  db,
  error,
  text,
  onText,
  selected,
  expanded,
  searchRef,
  onRefresh,
  onPick,
}: KillsTabProps) {
  return (
    <div className="agents-tab-body">
      <AgentSearch
        ref={searchRef}
        value={text}
        onChange={onText}
        placeholder="Filter kills… ( / )"
        totalShown={totalShown}
        totalAll={totalAll}
        onRefresh={onRefresh}
      />
      {error ? (
        <div className="empty">kills unavailable: {error}</div>
      ) : totalAll === 0 ? (
        <div className="empty">no kill events recorded</div>
      ) : rows.length === 0 ? (
        <div className="empty">No kills match.</div>
      ) : (
        <div className="agents-list">
          <div className="agents-source-line">
            <code>{db}</code>
          </div>
          {rows.map((r) => {
            if (r.kind === "header") {
              return (
                <div key={r.key} className="agents-day-header">
                  {r.label}
                </div>
              );
            }
            return (
              <div key={r.key}>
                <KillRow
                  kill={r.row}
                  selected={r.key === selected}
                  expanded={r.key === expanded}
                  onPick={() => onPick(r.key)}
                />
                {r.key === expanded && r.row.detail ? (
                  <pre className="agents-kill-detail">{r.row.detail}</pre>
                ) : null}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

interface ModelsTabProps {
  models: ReturnType<typeof sortModels>;
  totalAll: number;
  dir: string;
  error: string | null;
  text: string;
  onText(v: string): void;
  selected: string | null;
  expanded: string | null;
  searchRef: React.RefObject<HTMLInputElement | null>;
  onRefresh(): void;
  onPick(key: string): void;
}

function ModelsTab({
  models,
  totalAll,
  dir,
  error,
  text,
  onText,
  selected,
  expanded,
  searchRef,
  onRefresh,
  onPick,
}: ModelsTabProps) {
  return (
    <div className="agents-tab-body">
      <AgentSearch
        ref={searchRef}
        value={text}
        onChange={onText}
        placeholder="Filter models… ( / )"
        totalShown={models.length}
        totalAll={totalAll}
        onRefresh={onRefresh}
      />
      {error ? (
        <div className="empty">models unavailable: {error}</div>
      ) : totalAll === 0 ? (
        <div className="empty">
          no models cataloged. Run <code>crabcc model-info seed-default</code>
        </div>
      ) : models.length === 0 ? (
        <div className="empty">No models match.</div>
      ) : (
        <div className="agents-list">
          <div className="agents-source-line">
            <code>{dir}</code>
          </div>
          {models.map((m) => {
            const key = modelKey(m);
            return (
              <div key={key}>
                <ModelRow
                  model={m}
                  selected={key === selected}
                  expanded={key === expanded}
                  onPick={() => onPick(key)}
                />
                {key === expanded ? (
                  <div className="agents-model-detail">
                    <dl className="agents-detail-grid">
                      <dt>provider</dt>
                      <dd>{m.provider}</dd>
                      <dt>name</dt>
                      <dd>{m.name}</dd>
                      {m.params ? (
                        <>
                          <dt>params</dt>
                          <dd>{m.params}</dd>
                        </>
                      ) : null}
                      {m.context !== null ? (
                        <>
                          <dt>context</dt>
                          <dd>{m.context.toLocaleString()} tokens</dd>
                        </>
                      ) : null}
                      <dt>file</dt>
                      <dd>
                        <code>{m.file}</code>
                      </dd>
                    </dl>
                    {m.docs_first ? (
                      <a
                        className="agents-model-docs"
                        href={m.docs_first}
                        target="_blank"
                        rel="noreferrer"
                      >
                        {m.docs_first}
                      </a>
                    ) : null}
                  </div>
                ) : null}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
