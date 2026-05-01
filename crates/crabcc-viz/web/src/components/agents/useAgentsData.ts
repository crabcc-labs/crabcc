// Polls the three quasi-static agent feeds (profiles / kills / models)
// at their established cadences. Live agents flow in over SSE from the
// parent App; this hook only handles the polled tabs so the live tab
// can stay reactive without a redundant fetch.
//
// Cadences match the prior hand-rolled panels:
//   - profiles: 5 s    (cheap, reflects file-system changes)
//   - kills:    10 s   (sqlite tail, infrequent)
//   - models:   30 s   (config files, even rarer)
//
// Each refresh exposes a `refetch()` callback so the per-tab "r"
// shortcut + refresh button can re-poll on demand.

import { useCallback, useEffect, useState } from "react";
import { api } from "../../api";
import { logFetchErr, logFetchOk } from "../../lifecycle";
import type {
  AgentKillRow,
  AgentModelEntry,
  AgentProfileEntry,
} from "./types";

export interface UseAgentsData {
  profiles: AgentProfileEntry[];
  profilesDir: string;
  profilesError: string | null;
  refetchProfiles(): void;

  kills: AgentKillRow[];
  killsDb: string;
  killsError: string | null;
  refetchKills(): void;

  models: AgentModelEntry[];
  modelsDir: string;
  modelsError: string | null;
  refetchModels(): void;
}

/// Runs all three pollers as long as the panel is mounted. Each
/// background interval is independent so a slow models endpoint can't
/// stall the kills feed.
export function useAgentsData(): UseAgentsData {
  const [profiles, setProfiles] = useState<AgentProfileEntry[]>([]);
  const [profilesDir, setProfilesDir] = useState("");
  const [profilesError, setProfilesError] = useState<string | null>(null);

  const [kills, setKills] = useState<AgentKillRow[]>([]);
  const [killsDb, setKillsDb] = useState("");
  const [killsError, setKillsError] = useState<string | null>(null);

  const [models, setModels] = useState<AgentModelEntry[]>([]);
  const [modelsDir, setModelsDir] = useState("");
  const [modelsError, setModelsError] = useState<string | null>(null);

  // ── profiles ───────────────────────────────────────────────────────
  const loadProfiles = useCallback(() => {
    api
      .agentProfiles()
      .then((r) => {
        setProfiles(r.profiles);
        setProfilesDir(r.dir);
        setProfilesError(null);
        logFetchOk("/api/agent-profiles", `${r.profiles.length} profiles`);
      })
      .catch((e) => {
        setProfilesError(String(e));
        logFetchErr("/api/agent-profiles", e);
      });
  }, []);

  // ── kills ──────────────────────────────────────────────────────────
  const loadKills = useCallback(() => {
    api
      .agentKills()
      .then((r) => {
        setKills(r.rows);
        setKillsDb(r.db);
        setKillsError(null);
        logFetchOk("/api/agent-kills", `${r.rows.length} rows`);
      })
      .catch((e) => {
        setKillsError(String(e));
        logFetchErr("/api/agent-kills", e);
      });
  }, []);

  // ── models ─────────────────────────────────────────────────────────
  const loadModels = useCallback(() => {
    api
      .agentModels()
      .then((r) => {
        setModels(r.models);
        setModelsDir(r.dir);
        setModelsError(null);
        logFetchOk("/api/agent-models", `${r.models.length} models`);
      })
      .catch((e) => {
        setModelsError(String(e));
        logFetchErr("/api/agent-models", e);
      });
  }, []);

  useEffect(() => {
    loadProfiles();
    const t = window.setInterval(loadProfiles, 5_000);
    return () => window.clearInterval(t);
  }, [loadProfiles]);

  useEffect(() => {
    loadKills();
    const t = window.setInterval(loadKills, 10_000);
    return () => window.clearInterval(t);
  }, [loadKills]);

  useEffect(() => {
    loadModels();
    const t = window.setInterval(loadModels, 30_000);
    return () => window.clearInterval(t);
  }, [loadModels]);

  return {
    profiles,
    profilesDir,
    profilesError,
    refetchProfiles: loadProfiles,
    kills,
    killsDb,
    killsError,
    refetchKills: loadKills,
    models,
    modelsDir,
    modelsError,
    refetchModels: loadModels,
  };
}
