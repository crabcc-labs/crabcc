// Tool-family line-icon set — 11 single-purpose icons matching the
// greenfield design system spec (16 px stroke 1.5, one icon per tool
// family). Closes sub-task 1 of issue #389 on the React side; mirrors
// crates/crabcc-desktop/src/icons.rs.
//
// Each icon is exported as a `() => JSX.Element` returning an inline
// `<svg>` with `currentColor` strokes, so the consumer controls the
// colour via Tailwind / CSS / parent `color`. Same shape as
// `lucide-react` icons so call-site swap-ins are mechanical.
//
// The SVG bodies are embedded inline rather than imported from the
// .svg files because esbuild's `loader: "file"` rule produces URLs,
// not React components, and adding an SVGR plugin to the existing
// `esbuild.config.mjs` is more dependency surface than the 11 icons
// justify. Future icons go here too — the inline cost is low (~30
// LOC per icon) and the .svg files in this directory remain the
// source of truth for designers / Stitch round-trips.

import type { JSX, SVGProps } from "react";

export type IconProps = SVGProps<SVGSVGElement> & {
  /** Edge length in pixels. Defaults to 16 (the design spec size). */
  size?: number;
};

/** Common base props applied to every tool icon. */
function base(props: IconProps): SVGProps<SVGSVGElement> {
  const { size = 16, ...rest } = props;
  return {
    width: size,
    height: size,
    viewBox: "0 0 16 16",
    fill: "none",
    xmlns: "http://www.w3.org/2000/svg",
    ...rest,
  };
}

export function SymIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <circle cx="7" cy="7" r="5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="M10.5 10.5L14 14" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="M5 6C4.5 6 4 6.5 4 7C4 7.5 4.5 8 5 8" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
      <path d="M9 6C9.5 6 10 6.5 10 7C10 7.5 9.5 8 9 8" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
    </svg>
  );
}

export function RefsIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <circle cx="8" cy="8" r="1.5" stroke="currentColor" strokeWidth="1.5" />
      <path d="M8 6.5V2" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="M6.5 9.5L3 13" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="M9.5 9.5L13 13" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <circle cx="8" cy="1.5" r="0.75" fill="currentColor" />
      <circle cx="2.5" cy="13.5" r="0.75" fill="currentColor" />
      <circle cx="13.5" cy="13.5" r="0.75" fill="currentColor" />
    </svg>
  );
}

export function CallersIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <rect x="5" y="5" width="6" height="6" rx="1" stroke="currentColor" strokeWidth="1.5" />
      <path d="M8 2V4M2 8H4M14 8H12" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="M7 4L8 5L9 4M4 7L5 8L4 9M12 7L11 8L12 9" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function OutlineIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <path d="M3 2H10L13 5V14H3V2Z" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M6 6H10M6 9H8M6 12H10" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
    </svg>
  );
}

export function FuzzyIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <circle cx="7" cy="7" r="5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="M10.5 10.5L14 14" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="M5 7C5.5 6 6.5 8 7 7C7.5 6 8.5 8 9 7" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
    </svg>
  );
}

export function MemoryIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <rect x="2" y="3" width="12" height="5" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
      <rect x="2" y="9" width="12" height="5" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
      <path d="M4 5.5H5M4 11.5H5" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" />
    </svg>
  );
}

export function FetchIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <circle cx="8" cy="8" r="6" stroke="currentColor" strokeWidth="1.5" />
      <path d="M2 8H14M8 2C9.5 4 10 6 10 8C10 10 9.5 12 8 14C6.5 12 6 10 6 8C6 6 6.5 4 8 2Z" stroke="currentColor" strokeWidth="1" opacity="0.6" />
      <path d="M8 6V11M6 9L8 11L10 9" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function AgentIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <circle cx="8" cy="5" r="2.5" stroke="currentColor" strokeWidth="1.5" />
      <path d="M3 14C3 11 5 10 8 10C11 10 13 11 13 14" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="M10 2C11 2.5 12 4 12 5" stroke="currentColor" strokeWidth="1" strokeLinecap="round" />
    </svg>
  );
}

export function IndexIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <rect x="2" y="2" width="5" height="5" rx="1" stroke="currentColor" strokeWidth="1.5" />
      <rect x="9" y="2" width="5" height="5" rx="1" stroke="currentColor" strokeWidth="1" opacity="0.4" />
      <rect x="2" y="9" width="5" height="5" rx="1" stroke="currentColor" strokeWidth="1" opacity="0.4" />
      <rect x="9" y="9" width="5" height="5" rx="1" stroke="currentColor" strokeWidth="1" opacity="0.4" />
    </svg>
  );
}

export function ServeIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <rect x="2" y="9" width="12" height="5" rx="2" stroke="currentColor" strokeWidth="1.5" />
      <path d="M5 6C6 4.5 10 4.5 11 6" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <circle cx="8" cy="7" r="0.5" fill="currentColor" />
    </svg>
  );
}

export function McpIcon(props: IconProps): JSX.Element {
  return (
    <svg {...base(props)}>
      <circle cx="8" cy="8" r="1" fill="currentColor" />
      <path d="M11 5C12 6 12 10 11 11" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      <path d="M5 5C4 6 4 10 5 11" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
    </svg>
  );
}

/// Lookup map for tool families addressed by name. Useful for dynamic
/// dispatch (e.g. rendering a tool list from server-supplied strings)
/// without a 11-arm switch at every call site.
export const TOOL_ICONS = {
  sym: SymIcon,
  refs: RefsIcon,
  callers: CallersIcon,
  outline: OutlineIcon,
  fuzzy: FuzzyIcon,
  memory: MemoryIcon,
  fetch: FetchIcon,
  agent: AgentIcon,
  index: IndexIcon,
  serve: ServeIcon,
  mcp: McpIcon,
} as const;

export type ToolName = keyof typeof TOOL_ICONS;
