// Document-level keyboard shortcuts. Mirror of the graph viewer's
// `useKeyboardControls` — short-circuits when the user is typing
// inside an input/textarea/contenteditable so search-box keystrokes
// don't fire panel actions.
//
// The one exception is `/`: we *want* it to focus the search box
// even from the input itself, so the parent can't lose focus and
// suddenly start eating characters.

import { useEffect } from "react";

export interface KeyboardActions {
  focusSearch(): void;
  clearOrUnpin(): void;
  selectPrev(): void;
  selectNext(): void;
  openSelected(): void;
  toggleGroupBy(): void;
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
      // Anything else from inside a text field is the user typing.
      if (isFromTextField(e.target)) {
        if (e.key === "Escape") {
          actions.clearOrUnpin();
          // Blur so the next "/" press is a noop-then-focus, not a stuck slash.
          (e.target as HTMLElement).blur();
        }
        return;
      }
      switch (e.key) {
        case "Escape":
          actions.clearOrUnpin();
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
        case "g":
        case "G":
          actions.toggleGroupBy();
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
