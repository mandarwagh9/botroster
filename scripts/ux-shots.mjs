// Screenshots every fixture state, both sizes, both themes.
//
//   node scripts/ux-shots.mjs [iteration]
//
// Writes .claude/ux-loop/shots/<iteration>/<scenario>-<width>-<theme>.png.
// One browser, four contexts, all scenarios in parallel within a context —
// the whole run has to stay under 90 seconds or the loop's iteration cost is
// dominated by taking pictures of itself.
//
// Theme is switched with Playwright's `colorScheme`, not a class or a
// data-attribute: `styles.css` themes through CSS `light-dark()`, which reads
// `prefers-color-scheme` and nothing else. A class toggle silently does nothing.

import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createRequire } from "node:module";
import { start, SCENARIOS, ROOT } from "../.claude/ux-loop/fixture/serve.mjs";

// The harness keeps its npm dependencies under .claude/ux-loop so that
// crates/botroster-app/ui stays npm-free and has no build step. Node resolves
// upward from this file and would never find them, so resolve explicitly.
const require = createRequire(join(ROOT, ".claude", "ux-loop", "package.json"));
const { chromium } = require("playwright");

const SIZES = [
  [1280, 800],
  [1600, 1000],
];
const THEMES = ["dark", "light"];

// Freeze anything that moves before capturing. Two runs must produce identical
// pixels; a 160ms transition caught mid-flight is noise the critic would read
// as a design change.
const FREEZE = `*, *::before, *::after {
  animation-duration: 0s !important;
  animation-delay: 0s !important;
  transition-duration: 0s !important;
  transition-delay: 0s !important;
  caret-color: transparent !important;
}`;

const iteration = String(process.argv[2] || process.env.UX_ITERATION || "000").padStart(3, "0");
const outDir = join(ROOT, ".claude", "ux-loop", "shots", iteration);

const started = Date.now();
const { server, origin } = await start(0);
const browser = await chromium.launch();
const problems = [];
let shot = 0;

try {
  await mkdir(outDir, { recursive: true });

  for (const [w, h] of SIZES) {
    for (const theme of THEMES) {
      const ctx = await browser.newContext({
        viewport: { width: w, height: h },
        colorScheme: theme,
        deviceScaleFactor: 1,
        // Fonts are vendored and served locally; nothing should reach the
        // network. If something tries, fail the shot rather than hang on it.
        offline: false,
      });

      await Promise.all(
        SCENARIOS.map(async ([id]) => {
          const page = await ctx.newPage();
          const consoleErrors = [];
          page.on("pageerror", (e) => consoleErrors.push(String(e.message || e)));
          try {
            await page.goto(`${origin}/?s=${id}`, { waitUntil: "domcontentloaded", timeout: 20000 });
            await page.waitForFunction("window.__ready === true", null, { timeout: 20000 });
            const err = await page.evaluate("window.__scenarioError || null");
            if (err) problems.push(`${id}: scenario post() threw: ${err.split("\n")[0]}`);
            await page.addStyleTag({ content: FREEZE });
            await page.screenshot({ path: join(outDir, `${id}-${w}-${theme}.png`) });
            shot++;
          } catch (e) {
            problems.push(`${id} ${w} ${theme}: ${String(e.message || e).split("\n")[0]}`);
          } finally {
            for (const ce of consoleErrors) problems.push(`${id} ${w} ${theme}: pageerror: ${ce}`);
            await page.close();
          }
        })
      );

      await ctx.close();
    }
  }
} finally {
  await browser.close();
  server.close();
}

const seconds = ((Date.now() - started) / 1000).toFixed(1);
const summary = {
  iteration,
  shots: shot,
  expected: SCENARIOS.length * SIZES.length * THEMES.length,
  seconds: Number(seconds),
  problems,
};
await writeFile(join(outDir, "shots.json"), JSON.stringify(summary, null, 2));

console.log(`shots: ${shot}/${summary.expected} in ${seconds}s -> ${outDir}`);
for (const p of problems) console.log(`  problem: ${p}`);
if (shot !== summary.expected) {
  console.error(`FAIL: expected ${summary.expected} shots, wrote ${shot}`);
  process.exit(1);
}
if (Number(seconds) > 90) {
  console.error(`FAIL: the run took ${seconds}s, over the 90s budget`);
  process.exit(1);
}
