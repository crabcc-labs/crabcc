// Minimal React 19 render helper for component tests run under
// happy-dom. We intentionally avoid pulling in @testing-library —
// the React core APIs (`createRoot` + `act`) are sufficient for the
// component contracts we assert in this module, and dropping a third
// testing layer keeps the bundle of dev-deps smaller.

import { createRoot, type Root } from "react-dom/client";
import { act } from "react";
import type { ReactElement } from "react";

export interface RenderResult {
  container: HTMLElement;
  root: Root;
  unmount: () => void;
  rerender: (el: ReactElement) => void;
}

export function render(el: ReactElement): RenderResult {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(el);
  });
  return {
    container,
    root,
    unmount: () => {
      act(() => {
        root.unmount();
      });
      container.remove();
    },
    rerender: (next: ReactElement) => {
      act(() => {
        root.render(next);
      });
    },
  };
}

/** Run an async block under act() so React commits + effects flush. */
export async function actAsync(fn: () => Promise<void> | void): Promise<void> {
  await act(async () => {
    await fn();
  });
}

/**
 * Drive a controlled `<input>` / `<textarea>`. React tracks the
 * previous value on the element and skips re-rendering if you assign
 * via the simple `el.value = …` setter (the `valueTracker` shim sees
 * the same value on the next event). The official workaround is to
 * call the prototype's native setter, then dispatch the synthetic
 * event React listens for.
 */
export function setInputValue(el: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  const proto = el instanceof HTMLTextAreaElement
    ? HTMLTextAreaElement.prototype
    : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
  if (!setter) {
    throw new Error("no native value setter");
  }
  setter.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}
