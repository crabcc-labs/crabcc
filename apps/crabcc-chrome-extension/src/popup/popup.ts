const el = document.getElementById("status");

async function probe(): Promise<void> {
  if (!el) return;
  try {
    const r = await fetch("http://localhost:7878/api/health", { cache: "no-store" });
    el.textContent = r.ok
      ? "✓ crabcc serve reachable on :7878"
      : `✗ crabcc serve returned ${r.status}`;
    el.className = r.ok ? "ok" : "bad";
  } catch {
    el.textContent = "✗ crabcc serve not reachable on :7878";
    el.className = "bad";
  }
}

void probe();
