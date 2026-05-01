// Hand-rolled fixed-row-height virtual scroller. Uses
// requestAnimationFrame to coalesce scroll events — without that,
// fast scrolls re-render every intermediate scrollTop, blowing the
// frame budget at ~3000 rows.
//
// We deliberately don't pull in react-window / react-virtuoso —
// they're ~6 KB+ minified and the activity panel is the only list
// big enough to need virtualization in this dashboard.

import { useCallback, useEffect, useRef, useState } from "react";
import { computeWindow, type VirtualWindow } from "./store";

const OVERSCAN = 4;

export interface UseVirtualWindow {
  /** Bind to the scrollable container. Callback ref so we can measure
   *  the moment the element mounts (the panel switches between an
   *  empty placeholder and the scroller — a useEffect ref'd to `[]`
   *  would miss the second mount). */
  containerRef: (el: HTMLDivElement | null) => void;
  /** Slice + spacers to render. */
  window: VirtualWindow;
  /** Current scroll-top, in px. */
  scrollTop: number;
  /** Programmatically scroll to top — used by `tail-follow`. */
  scrollToTop(smooth?: boolean): void;
  /** Programmatically scroll to a row index. Used by ↑/↓ keyboard nav. */
  scrollToIndex(idx: number): void;
  /** Whether the user has scrolled away from the top (used to gate auto-tail). */
  scrolledAway: boolean;
}

export function useVirtualWindow(
  total: number,
  rowHeight: number,
): UseVirtualWindow {
  const elRef = useRef<HTMLDivElement | null>(null);
  const roRef = useRef<ResizeObserver | null>(null);
  const scrollListenerRef = useRef<(() => void) | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(0);
  const rafRef = useRef<number | null>(null);

  const onScroll = useCallback(() => {
    const el = elRef.current;
    if (!el) return;
    if (rafRef.current !== null) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      setScrollTop(el.scrollTop);
    });
  }, []);

  // Callback ref — fires both on mount and on unmount of the ref'd
  // element, which is exactly what we need when the panel transitions
  // between empty and populated states.
  const containerRef = useCallback(
    (el: HTMLDivElement | null) => {
      // Tear down any previous binding first.
      if (elRef.current && scrollListenerRef.current) {
        elRef.current.removeEventListener("scroll", scrollListenerRef.current);
      }
      roRef.current?.disconnect();
      roRef.current = null;
      scrollListenerRef.current = null;
      elRef.current = el;
      if (!el) {
        setViewportH(0);
        return;
      }
      setViewportH(el.clientHeight);
      const ro = new ResizeObserver(() => setViewportH(el.clientHeight));
      ro.observe(el);
      roRef.current = ro;
      el.addEventListener("scroll", onScroll, { passive: true });
      scrollListenerRef.current = onScroll;
    },
    [onScroll],
  );

  useEffect(() => {
    return () => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      roRef.current?.disconnect();
    };
  }, []);

  const window = computeWindow(total, scrollTop, viewportH, rowHeight, OVERSCAN);

  const scrollToTop = useCallback((smooth = false) => {
    const el = elRef.current;
    if (!el) return;
    el.scrollTo({ top: 0, behavior: smooth ? "smooth" : "auto" });
  }, []);

  const scrollToIndex = useCallback(
    (idx: number) => {
      const el = elRef.current;
      if (!el) return;
      const targetTop = idx * rowHeight;
      const targetBot = targetTop + rowHeight;
      if (targetTop < el.scrollTop) {
        el.scrollTo({ top: targetTop, behavior: "auto" });
      } else if (targetBot > el.scrollTop + el.clientHeight) {
        el.scrollTo({
          top: targetBot - el.clientHeight,
          behavior: "auto",
        });
      }
    },
    [rowHeight],
  );

  // 8 px slack matches macOS scroll bounce / overshoot — without slack
  // tail-follow flickers off the moment the user pauses the wheel at top.
  const scrolledAway = scrollTop > 8;

  return { containerRef, window, scrollTop, scrollToTop, scrollToIndex, scrolledAway };
}
