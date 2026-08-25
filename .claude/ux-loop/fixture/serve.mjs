// Serves the shipped `ui/` with the IPC double spliced in, so the frontend runs
// in a plain browser with no Rust process behind it.
//
// The splice is the same one `crates/botroster-app/tests/page.rs` performs, and
// it reads the same `tauri-stub.js` off disk, so the Rust suite and this
// harness cannot disagree about what the IPC boundary looks like. Two doubles
// is that drift twice over.
//
// Run standalone for hand-driving:  node .claude/ux-loop/fixture/serve.mjs
// then open http://127.0.0.1:4173/?s=s06

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join, extname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
export const ROOT = join(HERE, "..", "..", "..");
const UI = join(ROOT, "crates", "botroster-app", "ui");
const STUB = join(ROOT, "crates", "botroster-app", "tests", "fixture", "tauri-stub.js");

// The marker `page.rs` also asserts on. If the script tag is ever renamed, both
// harnesses must fail loudly rather than silently serving a page with no stub.
const MARKER = '<script src="main.js"></script>';

// Runs after main.js has wired itself up. Drives the chosen scenario into
// place, then raises a flag the shot script waits on — waiting for a condition
// instead of a duration is what keeps a screenshot from catching a half-painted
// DOM on a loaded machine.
const DRIVER = `<script>
(async () => {
  try {
    await window.__scenario.post();
  } catch (e) {
    window.__scenarioError = String((e && e.stack) || e);
  }
  // Let layout and the 160ms transitions settle before anything is captured.
  await new Promise((r) => setTimeout(r, 220));
  await new Promise((r) => requestAnimationFrame(() => r()));
  window.__ready = true;
})();
</script>`;

const TYPES = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".woff2": "font/woff2",
  ".png": "image/png",
  ".svg": "image/svg+xml",
};

async function page() {
  const [html, stub, scenarios] = await Promise.all([
    readFile(join(UI, "index.html"), "utf8"),
    readFile(STUB, "utf8"),
    readFile(join(HERE, "scenarios.js"), "utf8"),
  ]);
  if (!html.includes(MARKER)) {
    throw new Error("index.html no longer loads main.js the way this harness expects");
  }
  const injected =
    `<script>${stub}</script>\n` + `<script>${scenarios}</script>\n` + MARKER + "\n" + DRIVER;
  return html.replace(MARKER, injected);
}

export async function start(port = 0) {
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://127.0.0.1");
    const path = url.pathname;
    try {
      if (path === "/" || path === "/index.html") {
        const body = await page();
        res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
        return res.end(body);
      }
      // Everything else comes off disk from the shipped ui/ directory: main.js,
      // styles.css, and the vendored fonts.
      const file = join(UI, path.replace(/^\/+/, ""));
      if (!file.startsWith(UI) || !existsSync(file)) {
        res.writeHead(404, { "Content-Type": "text/plain" });
        return res.end("not found");
      }
      const body = await readFile(file);
      res.writeHead(200, { "Content-Type": TYPES[extname(file)] || "application/octet-stream" });
      return res.end(body);
    } catch (e) {
      res.writeHead(500, { "Content-Type": "text/plain" });
      return res.end(String((e && e.stack) || e));
    }
  });
  await new Promise((r) => server.listen(port, "127.0.0.1", r));
  const { port: bound } = server.address();
  return { server, origin: `http://127.0.0.1:${bound}` };
}

// The twelve, in order. Kept here so the shot script and the gate script agree
// on the list without either owning it.
export const SCENARIOS = [
  ["s01", "cold start, nothing configured"],
  ["s02", "connect panel, sidecar not found"],
  ["s03", "empty roster, zero Bots"],
  ["s04", "roster with 4 Bots"],
  ["s05", "thread mid-run, tool steps streaming"],
  ["s06", "pending shell.exec approval"],
  ["s07", "denied by policy"],
  ["s08", "long thread, 200+ steps"],
  ["s09", "group thread, three Bots"],
  ["s10", "computer viewer"],
  ["s11", "guest disconnected, budget exhausted"],
  ["s12", "routines list"],
  // J2's other failure, and the one a fresh install actually hits. Added in
  // run 2: every scenario from s03 on asserts `connected = true`, so the
  // harness opened on the far side of the wall and could not see the state
  // most people meet first.
  ["s13", "connect fails: no model configured"],
  ["s14", "bypass on: answered without asking, and recorded"],
];

if (import.meta.url === `file://${process.argv[1].replace(/\\/g, "/")}`) {
  const { origin } = await start(4173);
  console.log(`fixture on ${origin}`);
  for (const [id, what] of SCENARIOS) console.log(`  ${origin}/?s=${id}   ${what}`);
}
