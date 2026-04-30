import { context, build as esbuild } from "esbuild";
import { cp, mkdir, rm } from "node:fs/promises";
import { join } from "node:path";

const ROOT = import.meta.dir;
const DIST = join(ROOT, "dist");
const watch = process.argv.includes("--watch");

const entries = {
  "background/service-worker": "src/background/service-worker.ts",
  "offscreen/offscreen": "src/offscreen/offscreen.ts",
  "content/cua-driver": "src/content/cua-driver.ts",
  "popup/popup": "src/popup/popup.ts",
};

async function copyStatics() {
  await cp(join(ROOT, "manifest.json"), join(DIST, "manifest.json"));
  await cp(join(ROOT, "src/offscreen/offscreen.html"), join(DIST, "offscreen/offscreen.html"));
  await cp(join(ROOT, "src/popup/popup.html"), join(DIST, "popup/popup.html"));
}

async function main() {
  await rm(DIST, { recursive: true, force: true });
  await mkdir(DIST, { recursive: true });

  const opts: Parameters<typeof esbuild>[0] = {
    entryPoints: Object.fromEntries(
      Object.entries(entries).map(([out, src]) => [out, join(ROOT, src)]),
    ),
    bundle: true,
    format: "esm",
    target: "chrome116",
    platform: "browser",
    outdir: DIST,
    sourcemap: watch ? "inline" : false,
    minify: !watch,
    logLevel: "info",
  };

  if (watch) {
    const ctx = await context(opts);
    await ctx.watch();
    await copyStatics();
    console.log("[build] watching…  reload the unpacked extension in Chrome after edits");
  } else {
    await esbuild(opts);
    await copyStatics();
    console.log(`[build] done → ${DIST}`);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
