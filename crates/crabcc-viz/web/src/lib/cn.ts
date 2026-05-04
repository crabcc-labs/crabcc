// shadcn-style class-name helper (track B.2). Combines `clsx` for
// conditional / array forms with `tailwind-merge` for
// last-utility-wins resolution when classes conflict (e.g. a base
// `px-2` followed by an override `px-3`).
//
// Convention is identical to every shadcn `components/ui/*.tsx`
// drop-in so future B.3+ slices can paste them in without tweaks.

import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}
