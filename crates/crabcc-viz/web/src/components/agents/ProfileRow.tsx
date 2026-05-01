// One profile row — the on-disk agent profile entry, plus an "in use"
// flag when a running agent matches the profile's id or model.

import { memo } from "react";
import type { AgentProfileEntry } from "./types";

interface Props {
  profile: AgentProfileEntry;
  selected: boolean;
  expanded: boolean;
  inUse: boolean;
  onPick(): void;
}

export const ProfileRow = memo(function ProfileRow({
  profile,
  selected,
  expanded,
  inUse,
  onPick,
}: Props) {
  const cls =
    "agents-row profile" +
    (selected ? " selected" : "") +
    (expanded ? " expanded" : "") +
    (inUse ? " in-use" : "");
  return (
    <button type="button" className={cls} onClick={onPick}>
      <span className="agents-profile-id">{profile.id}</span>
      <span className="agents-profile-meta">
        {profile.crate_ ? (
          <span className="agents-tag">crate {profile.crate_}</span>
        ) : null}
        {profile.model ? (
          <span className="agents-tag">{profile.model}</span>
        ) : null}
        {inUse ? <span className="agents-tag agents-tag-live">in use</span> : null}
      </span>
      {profile.description ? (
        <span className="agents-profile-desc">{profile.description}</span>
      ) : null}
    </button>
  );
});
