// Single styling unifier on top of lucide-react.
//
// Why a wrapper: every call site previously rendered a unicode glyph
// inline. Centralising default size + stroke-width here means call
// sites stay terse — `<Icon of={Settings} />` — and visual density
// stays consistent without each consumer re-deriving width/stroke.
//
// Typing follows the canonical pattern from
// https://lucide.dev/guide/react/advanced/typescript:
//
//   - `LucideProps` is what every icon component accepts (size, color,
//     strokeWidth, absoluteStrokeWidth, plus any SVG attrs).
//   - `LucideIcon` is `React.FC<LucideProps>`.
//
// Extending `LucideProps` (instead of `SVGProps<SVGSVGElement>`) means
// callers get accurate IntelliSense for the Lucide-specific props
// (`absoluteStrokeWidth`, `color`) without needing manual omissions.
//
// Lucide inherits `currentColor` by default; we never hardcode a fill
// so icons pick up the surrounding text colour + theme variables.

import type { LucideIcon, LucideProps } from "lucide-react";

interface IconProps extends LucideProps {
  /** The Lucide icon component to render. */
  of: LucideIcon;
}

export function Icon({ of: Inner, size = 14, strokeWidth = 1.75, ...rest }: IconProps) {
  return <Inner size={size} strokeWidth={strokeWidth} {...rest} />;
}
