// Keyboard control hook — listens at the document level so the user
// doesn't have to focus a particular element to drive the graph. We
// short-circuit when an input/textarea/contenteditable element is
// focused so typing in the search box doesn't pan the graph.

import { useEffect } from "react";

const PAN_STEP = 40;
const ZOOM_FACTOR = 1.15;

export interface KeyboardActions {
  pan(dx: number, dy: number): void;
  zoom(factor: number): void;
  reset(): void;
  unpin(): void;
}

export function useKeyboardControls(actions: KeyboardActions, enabled: boolean): void {
  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (isFromTextField(e.target)) return;
      switch (e.key) {
        case "ArrowUp":
          actions.pan(0, PAN_STEP);
          break;
        case "ArrowDown":
          actions.pan(0, -PAN_STEP);
          break;
        case "ArrowLeft":
          actions.pan(PAN_STEP, 0);
          break;
        case "ArrowRight":
          actions.pan(-PAN_STEP, 0);
          break;
        case "+":
        case "=":
          actions.zoom(ZOOM_FACTOR);
          break;
        case "-":
        case "_":
          actions.zoom(1 / ZOOM_FACTOR);
          break;
        case "Escape":
          actions.unpin();
          return; // skip preventDefault — Esc has other roles in the app.
        case "r":
        case "R":
          actions.reset();
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
