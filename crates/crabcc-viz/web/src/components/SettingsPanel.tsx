// SettingsPanel — user-configurable dashboard preferences.
// All settings persist to localStorage and survive page reload.
// Expose via the settings gear icon in the header.

import { useState } from "react";
import { Settings as SettingsIcon, X } from "lucide-react";
import { Icon } from "./icons";

export type Settings = {
  /** OTLP health probe interval in ms (15 s – 3600 s). */
  otlpPollMs: number;
  /** Telemetry event list poll interval in ms (1 s – 60 s). */
  telPollMs: number;
  /** Max telemetry events shown in the panel (10 – 500). */
  telMaxEvents: number;
  /** Agent list poll interval in ms (2 s – 60 s). */
  agentPollMs: number;
};

const DEFAULTS: Settings = {
  otlpPollMs:    30_000,
  telPollMs:      3_000,
  telMaxEvents:     100,
  agentPollMs:    5_000,
};

const STORAGE_KEY = "crabcc_dashboard_settings";

export function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<Settings>;
    return {
      otlpPollMs:  clamp(parsed.otlpPollMs  ?? DEFAULTS.otlpPollMs,  15_000, 3_600_000),
      telPollMs:   clamp(parsed.telPollMs   ?? DEFAULTS.telPollMs,    1_000,    60_000),
      telMaxEvents:clamp(parsed.telMaxEvents ?? DEFAULTS.telMaxEvents,    10,       500),
      agentPollMs: clamp(parsed.agentPollMs  ?? DEFAULTS.agentPollMs, 2_000,    60_000),
    };
  } catch {
    return { ...DEFAULTS };
  }
}

export function saveSettings(s: Settings): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(s));
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, Number.isFinite(v) ? v : lo));
}

function msToSecs(ms: number): number { return Math.round(ms / 1000); }
function secsToMs(s: number): number  { return s * 1000; }

function formatDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60 > 0 ? `${s % 60}s` : ""}`.trim();
  return `${Math.floor(s / 3600)}h`;
}

export function SettingsPanel({
  settings,
  onChange,
  onClose,
}: {
  settings: Settings;
  onChange: (s: Settings) => void;
  onClose: () => void;
}) {
  const [local, setLocal] = useState<Settings>({ ...settings });

  function update<K extends keyof Settings>(key: K, value: Settings[K]) {
    setLocal((prev) => ({ ...prev, [key]: value }));
  }

  function apply() {
    onChange(local);
    saveSettings(local);
    onClose();
  }

  function reset() {
    setLocal({ ...DEFAULTS });
  }

  return (
    <div className="settings-overlay" onClick={onClose}>
      <div
        className="settings-panel"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-label="Dashboard settings"
      >
        <div className="settings-header">
          <span>
            <Icon of={SettingsIcon} size={14} aria-hidden="true" /> Dashboard settings
          </span>
          <button className="settings-close" onClick={onClose} aria-label="Close">
            <Icon of={X} size={14} />
          </button>
        </div>

        <div className="settings-body">
          <Row label="OTLP probe interval"
               hint="min 15 s · max 1 h"
               value={msToSecs(local.otlpPollMs)}
               min={15} max={3600}
               onChange={(v) => update("otlpPollMs", secsToMs(v))}
               display={formatDuration(local.otlpPollMs)} />

          <Row label="Telemetry poll interval"
               hint="min 1 s · max 60 s"
               value={msToSecs(local.telPollMs)}
               min={1} max={60}
               onChange={(v) => update("telPollMs", secsToMs(v))}
               display={formatDuration(local.telPollMs)} />

          <Row label="Max telemetry events"
               hint="10 – 500"
               value={local.telMaxEvents}
               min={10} max={500}
               onChange={(v) => update("telMaxEvents", v)}
               display={String(local.telMaxEvents)} />

          <Row label="Agent poll interval"
               hint="min 2 s · max 60 s"
               value={msToSecs(local.agentPollMs)}
               min={2} max={60}
               onChange={(v) => update("agentPollMs", secsToMs(v))}
               display={formatDuration(local.agentPollMs)} />
        </div>

        <div className="settings-footer">
          <button className="settings-btn-secondary" onClick={reset}>Reset defaults</button>
          <button className="settings-btn-primary" onClick={apply}>Apply &amp; reload</button>
        </div>
      </div>
    </div>
  );
}

function Row({
  label, hint, value, min, max, onChange, display,
}: {
  label: string; hint: string; value: number;
  min: number; max: number;
  onChange: (v: number) => void; display: string;
}) {
  return (
    <label className="settings-row">
      <div className="settings-row-label">
        <span>{label}</span>
        <span className="settings-hint">{hint}</span>
      </div>
      <div className="settings-row-control">
        <input
          type="range"
          min={min} max={max} value={value}
          onChange={(e) => onChange(parseInt(e.target.value, 10))}
        />
        <span className="settings-value">{display}</span>
      </div>
    </label>
  );
}
