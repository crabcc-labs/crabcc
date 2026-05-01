// Bundles the extension into ./dist/. Each bundle is a single file that
// matches a manifest entry point — service worker, popup script — so
// Chrome can load the directory directly with no extra plumbing.
//
// `--watch` keeps esbuild's incremental rebuild loop running. `manifest.json`
// + `popup.html` are copied verbatim.

import { build, context } from "esbuild";
import { copyFile, mkdir, readdir, stat } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, "dist");
const watch = process.argv.includes("--watch");

const entryPoints = {
  background: join(here, "src/background.ts"),
  popup: join(here, "src/popup.ts"),
};

const common = {
  bundle: true,
  format: "esm",
  target: ["chrome116"],
  outdir: dist,
  // Chrome MV3 service worker can't use eval-based source maps; inline maps
  // would bloat the bundle, so we drop them in production builds.
  sourcemap: process.env.NODE_ENV === "production" ? false : "linked",
  logLevel: "info",
  define: {
    "process.env.NODE_ENV": JSON.stringify(process.env.NODE_ENV ?? "development"),
  },
};

async function copyStatic() {
  await mkdir(dist, { recursive: true });
  await copyFile(join(here, "manifest.json"), join(dist, "manifest.json"));
  await copyFile(join(here, "src/popup.html"), join(dist, "popup.html"));
  await copyFile(join(here, "src/popup.css"), join(dist, "popup.css"));
  // Optional: copy any icon files dropped under src/icons/ — skip silently
  // when the directory doesn't exist (Phase 0 ships without icons).
  try {
    const iconsSrc = join(here, "src/icons");
    await stat(iconsSrc);
    const iconsDst = join(dist, "icons");
    await mkdir(iconsDst, { recursive: true });
    for (const f of await readdir(iconsSrc)) {
      await copyFile(join(iconsSrc, f), join(iconsDst, f));
    }
  } catch {
    // no icons/, ignore
  }
}

await copyStatic();

if (watch) {
  const ctx = await context({ entryPoints, ...common });
  await ctx.watch();
  console.log("[crabcc-chrome] esbuild watching…");
} else {
  await build({ entryPoints, ...common });
  console.log("[crabcc-chrome] built dist/");
}
