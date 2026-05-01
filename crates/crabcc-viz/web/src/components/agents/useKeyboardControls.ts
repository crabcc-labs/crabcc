// Document-level keyboard shortcuts for the agents panel. Mirrors the
// activity module's handler — short-circuits when typing inside an
// input/textarea/contenteditable so search-box keystrokes don't fire
// panel actions. The one exception is "/" which we always honor so the
// user can focus the search box from anywhere.

import { useEffect } from "react";

export interface KeyboardActions {
  setTabByIndex(i: number): void;
  focusSearch(): void;
  clearOrCollapse(): void;
  selectPrev(): void;
  selectNext(): void;
  openSelected(): void;
  refresh(): void;
}

export function useKeyboardControls(
  actions: KeyboardActions,
  enabled: boolean,
): void {
  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "/") {
        e.preventDefault();
        actions.focusSearch();
        return;
      }
      // From inside a text field — only Escape escapes back out.
      if (isFromTextField(e.target)) {
        if (e.key === "Escape") {
          actions.clearOrCollapse();
          (e.target as HTMLElement).blur();
        }
        return;
      }
      switch (e.key) {
        case "1":
          actions.setTabByIndex(0);
          break;
        case "2":
          actions.setTabByIndex(1);
          break;
        case "3":
          actions.setTabByIndex(2);
          break;
        case "4":
          actions.setTabByIndex(3);
          break;
        case "Escape":
          actions.clearOrCollapse();
          break;
        case "ArrowUp":
          actions.selectPrev();
          break;
        case "ArrowDown":
          actions.selectNext();
          break;
        case "Enter":
          actions.openSelected();
          break;
        case "r":
        case "R":
          actions.refresh();
          break;
        default:
          return;
      }
      e.preventDefault();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [actions, enabled]);
}

function isFromTextField(t: EventTarget | null): boolean {
  if (!(t instanceof HTMLElement)) return false;
  const tag = t.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
  return t.isContentEditable;
}
