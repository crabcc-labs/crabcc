import { useState, useEffect } from "react";

interface WormholeSession {
  session_hex: string;
  node_id_hex: string;
  connected_at: number;
  route: string;
}

export function useWormholeSessions() {
  const [sessions, setSessions] = useState<WormholeSession[]>([]);
  useEffect(() => {
    fetch("/api/wormhole/sessions")
      .then((r) => (r.ok ? r.json() : []))
      .then(setSessions)
      .catch(() => {});
  }, []);
  return sessions;
}
