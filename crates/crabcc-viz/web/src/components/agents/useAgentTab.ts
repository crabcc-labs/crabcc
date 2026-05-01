// Active-tab state. A tiny hook so the orchestrator stays slim and the
// keyboard handler can switch tabs without prop-drilling setters.

import { useCallback, useState } from "react";
import { TAB_ORDER, type TabId } from "./types";

export interface UseAgentTab {
  tab: TabId;
  setTab(t: TabId): void;
  setTabByIndex(i: number): void;
}

export function useAgentTab(initial: TabId = "live"): UseAgentTab {
  const [tab, setTab] = useState<TabId>(initial);
  const setTabByIndex = useCallback((i: number) => {
    if (i >= 0 && i < TAB_ORDER.length) setTab(TAB_ORDER[i]);
  }, []);
  return { tab, setTab, setTabByIndex };
}
