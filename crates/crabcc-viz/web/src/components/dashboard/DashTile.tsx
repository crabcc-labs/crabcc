// `<DashTile />` — the shared wrapper every dashboard tile reuses.
//
// Why a wrapper instead of free-form markup: the home page now packs ~9
// tiles into one viewport. Centralising the chrome (title bar, padding,
// footer slot, optional "open ›" link) keeps every tile pixel-aligned
// without each one re-deriving its own border-radius + h2 styling.

import { memo, type ReactNode } from "react";

interface Props {
  title: string;
  /** Anchor + label for an "open ›" link in the title bar. */
  openHref?: string;
  openLabel?: string;
  /** Optional right-aligned chip (e.g. a count or a status pill). */
  meta?: ReactNode;
  /** Force a tighter top/bottom padding for KPI tiles. */
  compact?: boolean;
  /** Optional grid-area assignment so the home grid can place tiles. */
  area?: string;
  children: ReactNode;
}

export const DashTile = memo(function DashTile({
  title,
  openHref,
  openLabel,
  meta,
  compact = false,
  area,
  children,
}: Props) {
  return (
    <section
      className={`dash-tile${compact ? " dash-tile-compact" : ""}`}
      style={area ? { gridArea: area } : undefined}
    >
      <header className="dash-tile-head">
        <h3 className="dash-tile-title">{title}</h3>
        {meta && <span className="dash-tile-meta">{meta}</span>}
        {openHref && (
          <a
            href={openHref}
            className="dash-tile-open"
            tabIndex={0}
            aria-label={`Open ${openLabel ?? title}`}
          >
            {openLabel ?? "open"} ›
          </a>
        )}
      </header>
      <div className="dash-tile-body">{children}</div>
    </section>
  );
});
