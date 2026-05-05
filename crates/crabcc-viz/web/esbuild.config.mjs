// esbuild config — bundles src/main.tsx + src/styles.css into a
// single self-contained HTML file at dist/live.html. The Rust crate
// `include_str!`s the resulting file the same way it consumed the
// hand-written assets/live.html.
//
// Output strategy:
//   1. esbuild emits dist/main.js + dist/main.css (minified in prod,
//      raw + sourcemapped in --watch).
//   2. We read both, then assemble dist/live.html with the JS + CSS
//      inlined inside <script> + <style> tags.
//
// Local-dev tuning (--watch):
//   - opt for fast incremental rebuilds: minify=false, sourcemap=inline.
//   - splitting=false: a single bundle keeps the assemble step trivial.
//   - rebuild on file save (esbuild's incremental cache + bun's fast FS).
//   - on each rebuild, re-run assembleHtml() so the dev server (or a
//     simple `python -m http.server` in dist/) sees fresh output.

import * as esbuild from "esbuild";
import { readFile, writeFile, mkdir, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const watch = process.argv.includes("--watch");

await mkdir(resolve(HERE, "dist"), { recursive: true });

/** @type {import('esbuild').BuildOptions} */
const opts = {
  entryPoints: [resolve(HERE, "src/main.tsx")],
  bundle: true,
  outdir: resolve(HERE, "dist"),
  outbase: resolve(HERE, "src"),
  format: "iife",
  target: ["es2022"],
  minify: !watch,
  sourcemap: watch ? "inline" : false,
  jsx: "automatic",
  define: {
    "process.env.NODE_ENV": watch ? '"development"' : '"production"',
  },
  loader: {
    ".css": "css",
  },
  treeShaking: true,
  legalComments: "none",
  // Local-dev win: keep the file-watch poll snappy + skip type-checking
  // (typecheck runs separately via `bun run typecheck` → tsgo).
  logLevel: watch ? "info" : "warning",
};

async function buildOnce() {
  const result = await esbuild.build({ ...opts, metafile: true });
  await assembleHtml();
  const out = result.metafile?.outputs ?? {};
  const sizes = Object.entries(out)
    .map(([f, m]) => `  ${f.replace(`${HERE}/`, "")}: ${m.bytes} B`)
    .join("\n");
  console.log(`built dist/live.html\n${sizes}`);
}

async function assembleHtml() {
  const js = await readFile(resolve(HERE, "dist/main.js"), "utf8");
  let css = "";
  try {
    css = await readFile(resolve(HERE, "dist/main.css"), "utf8");
  } catch {
    // CSS is emitted only when src/styles.css is reachable from main.tsx.
  }
  const html = TEMPLATE.replace("/*STYLES*/", css).replace("/*SCRIPT*/", js);
  await writeFile(resolve(HERE, "dist/live.html"), html);
}

const TEMPLATE = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="referrer" content="no-referrer">
<meta name="color-scheme" content="light dark">
<title>crabcc · live</title>
<link rel="icon" type="image/png" sizes="32x32" href="/static/favicon-32.png">
<link rel="icon" type="image/png" sizes="16x16" href="/static/favicon-16.png">
<link rel="apple-touch-icon" sizes="180x180" href="/static/apple-touch-icon.png">
<style>/*STYLES*/</style>
</head>
<body>
<div id="root"></div>
<script>/*SCRIPT*/</script>
</body>
</html>`;

if (watch) {
  const ctx = await esbuild.context(opts);
  await ctx.watch();
  console.log("watching for changes…");
  // Poll the bundle for changes (esbuild writes both main.js + main.css
  // on each rebuild; checking main.js mtime is enough). We re-assemble
  // dist/live.html only when the JS bundle has actually changed.
  let lastMtime = 0;
  setInterval(async () => {
    try {
      const m = await stat(resolve(HERE, "dist/main.js"));
      if (m.mtimeMs !== lastMtime) {
        lastMtime = m.mtimeMs;
        await assembleHtml();
      }
    } catch {
      // bundle hasn't been written yet — wait for the next tick.
    }
  }, 250);
} else {
  await buildOnce();
}
