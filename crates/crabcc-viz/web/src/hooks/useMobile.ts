import { useSyncExternalStore } from "react";

const MOBILE_BREAKPOINT = 768;

function isMobileNow(): boolean {
  if (typeof window === "undefined") return false;
  return window.innerWidth < MOBILE_BREAKPOINT;
}

function subscribe(cb: () => void): () => void {
  if (typeof window === "undefined") return () => {};
  const mq = window.matchMedia(`(max-width: ${MOBILE_BREAKPOINT - 1}px)`);
  mq.addEventListener("change", cb);
  return () => mq.removeEventListener("change", cb);
}

export function useMobile(): boolean {
  return useSyncExternalStore(subscribe, isMobileNow, () => false);
}
