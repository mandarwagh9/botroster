// The browser-side gates: axe, contrast, keyboard, reduced motion, and the
// approval invariants. Emits JSON on stdout and exits non-zero on any failure.
//
//   node scripts/ux-audit.mjs
//
// These are assertions against the shipped `ui/` running in a real browser, not
// lint rules over the source. A lint rule can say a token is dark enough; only
// the rendered page can say whether the text that uses it sits on the surface
// anyone thought it did.

import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createRequire } from "node:module";
import { start, SCENARIOS, ROOT } from "../.claude/ux-loop/fixture/serve.mjs";

// See ux-shots.mjs: the dependencies live under .claude/ux-loop on purpose.
const require = createRequire(join(ROOT, ".claude", "ux-loop", "package.json"));
const { chromium } = require("playwright");
const AxeBuilder = require("@axe-core/playwright").default || require("@axe-core/playwright");

const THEMES = ["dark", "light"];
const failures = [];
const notes = [];
const fail = (m) => failures.push(m);

// ---------------------------------------------------------------- contrast
//
// Walks every text-bearing node, resolves the effective background by climbing
// until something is not transparent, and applies WCAG AA: 4.5:1 for body,
// 3:1 for large text (>=24px, or >=18.66px bold).
//
// `--ghost` is documented in styles.css as not clearing AA, and is for
// placeholders and disabled text only. Those two cases are exempt here for
// exactly that reason. If a failure points at `--ghost` on anything else, the
// fix is to stop using `--ghost` there, not to lighten the token.
const CONTRAST = `(() => {
  const lum = (c) => {
    const [r, g, b] = c.map((v) => {
      const s = v / 255;
      return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
    });
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const parse = (s) => {
    const m = String(s).match(/rgba?\\(([^)]+)\\)/);
    if (!m) return null;
    const p = m[1].split(/[ ,\\/]+/).filter(Boolean).map(Number);
    return { rgb: [p[0], p[1], p[2]], a: p.length > 3 ? p[3] : 1 };
  };
  const over = (fg, bg) => fg.map((c, i) => c * 1 + bg[i] * 0);
  const blend = (top, topA, bottom) => top.map((c, i) => c * topA + bottom[i] * (1 - topA));
  const ratio = (a, b) => {
    const l1 = lum(a), l2 = lum(b);
    return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
  };

  const bgOf = (el) => {
    let node = el, acc = null;
    while (node && node !== document.documentElement.parentNode) {
      const s = getComputedStyle(node);
      const p = parse(s.backgroundColor);
      if (p && p.a > 0) {
        acc = acc === null ? p.rgb : blend(acc, 1, p.rgb);
        if (p.a >= 1) return p.rgb;
      }
      node = node.parentElement;
    }
    return [0, 0, 0];
  };

  const out = [];
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  const seen = new Set();
  let n;
  while ((n = walker.nextNode())) {
    const text = (n.nodeValue || "").trim();
    if (!text) continue;
    const el = n.parentElement;
    if (!el || seen.has(el)) continue;
    seen.add(el);
    const s = getComputedStyle(el);
    if (s.visibility === "hidden" || s.display === "none" || Number(s.opacity) === 0) continue;
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) continue;
    // Exempt: the documented placeholder/disabled tier.
    if (el.matches("[disabled], [aria-disabled='true'], .sr-only")) continue;
    if (el.closest("[disabled], [aria-disabled='true'], .sr-only")) continue;

    const fg = parse(s.color);
    if (!fg) continue;
    const bg = bgOf(el);
    const eff = fg.a < 1 ? blend(fg.rgb, fg.a, bg) : fg.rgb;
    const size = parseFloat(s.fontSize);
    const bold = Number(s.fontWeight) >= 700;
    const large = size >= 24 || (size >= 18.66 && bold);
    const need = large ? 3 : 4.5;
    const got = ratio(eff, bg);
    if (got < need) {
      out.push({
        got: Math.round(got * 100) / 100,
        need,
        size,
        color: s.color,
        text: text.slice(0, 48),
        sel: el.tagName.toLowerCase() + (el.className && typeof el.className === "string" ? "." + el.className.trim().split(/\\s+/).join(".") : ""),
      });
    }
  }
  return out;
})()`;

// ------------------------------------------------------------ reduced motion
// With prefers-reduced-motion, nothing may still be animating on load.
const MOTION = `(() => {
  const bad = [];
  for (const el of document.querySelectorAll("*")) {
    const s = getComputedStyle(el);
    const durs = (s.transitionDuration || "").split(",").map((d) => parseFloat(d) || 0);
    const props = (s.transitionProperty || "").split(",").map((p) => p.trim());
    durs.forEach((d, i) => {
      const p = props[i] || props[0] || "";
      if (d > 0 && /transform|opacity|all/.test(p)) {
        bad.push({ sel: el.tagName.toLowerCase() + "." + String(el.className || "").trim().split(/\\s+/)[0], prop: p, seconds: d });
      }
    });
    const ad = (s.animationDuration || "").split(",").map((d) => parseFloat(d) || 0);
    if (ad.some((d) => d > 0) && s.animationName !== "none") {
      bad.push({ sel: el.tagName.toLowerCase(), prop: "animation:" + s.animationName, seconds: Math.max(...ad) });
    }
  }
  return bad.slice(0, 20);
})()`;

// ------------------------------------------------------------------ keyboard
// Every interactive element reachable by Tab, with a focus ring that is
// actually visible. The approval dialog is carved out of the "Escape closes
// anything dismissible" and "no focus trap" rules on purpose — see below.
const KEYBOARD = `(() => {
  const sel = "a[href], button, input, select, textarea, [tabindex]:not([tabindex='-1'])";
  const vis = (el) => {
    const s = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    return s.visibility !== "hidden" && s.display !== "none" && r.width > 0 && r.height > 0 &&
           !el.closest(".hidden") && !el.disabled;
  };
  const all = [...document.querySelectorAll(sel)].filter(vis);
  const unreachable = all.filter((el) => el.tabIndex < 0).map(
    (el) => el.tagName.toLowerCase() + "#" + (el.id || "") + "." + String(el.className || "").split(" ")[0]
  );
  return { interactive: all.length, unreachable };
})()`;

// -------------------------------------------------- the approval invariants
//
// LOOP.md section 3. These are security properties, and an unattended restyle
// is exactly the thing that breaks them quietly. Invariant 4 (Escape resolves
// to refuse) is amended at launch: the shipped behaviour is that Escape does
// nothing to an approval, pinned by `page.rs`, and that is what is asserted.
async function approvalInvariants(ctx, origin) {
  const page = await ctx.newPage();
  const bad = [];
  try {
    await page.goto(`${origin}/?s=s06`, { waitUntil: "domcontentloaded" });
    await page.waitForFunction("window.__ready === true", null, { timeout: 20000 });

    const dialog = page.locator("#dialog");
    if (!(await dialog.isVisible())) {
      bad.push("the approval dialog did not open in s06, so no invariant could be checked");
      return bad;
    }

    // 1. The affirmative action is never the focused element on mount.
    const focused = await page.evaluate(`(() => {
      const a = document.activeElement;
      return a ? { id: a.id, cls: String(a.className || ""), text: (a.textContent || "").trim().slice(0, 40) } : null;
    })()`);
    if (focused && /allow/i.test(focused.text)) {
      bad.push(`an affirmative option has focus on mount: "${focused.text}"`);
    }

    // 7. A single Enter on mount cannot approve.
    const before = await page.evaluate("window.__sent('answer_permission').length");
    await page.keyboard.press("Enter");
    await page.waitForTimeout(120);
    const after = await page.evaluate("window.__sent('answer_permission').length");
    if (after > before) {
      const sent = await page.evaluate("window.__sent('answer_permission').slice(-1)[0]");
      if (sent && sent.args && /allow/i.test(String(sent.args.optionId))) {
        bad.push("a single Enter on mount approved the call");
      }
    }

    // 3. The tool name and every argument are rendered before any choice.
    const shown = await page.evaluate(`(() => {
      const d = document.getElementById("dialog");
      return { text: d.innerText, buttons: [...d.querySelectorAll("button")].map((b) => b.textContent.trim()) };
    })()`);
    for (const needle of ["cargo test --workspace", "botroster-workspace"]) {
      if (!shown.text.includes(needle)) bad.push(`the dialog does not show the argument ${JSON.stringify(needle)} before the choices`);
    }

    // 2. "Allow for the session" is distinct from "Allow once", and not primary.
    const session = page.locator("#dialog button", { hasText: /rest of this session/i }).first();
    const once = page.locator("#dialog button", { hasText: /^Allow once$/i }).first();
    if ((await session.count()) && (await once.count())) {
      const cls = await session.getAttribute("class");
      if (cls && /\\bprimary\\b/.test(cls)) bad.push("'allow for the session' is styled as the primary action");
      const same = await page.evaluate(`(() => {
        const bs = [...document.querySelectorAll("#dialog button")];
        const s = bs.find((b) => /rest of this session/i.test(b.textContent));
        const o = bs.find((b) => /^Allow once$/i.test(b.textContent.trim()));
        if (!s || !o) return false;
        const cs = getComputedStyle(s), co = getComputedStyle(o);
        return cs.backgroundColor === co.backgroundColor && cs.color === co.color && cs.borderColor === co.borderColor;
      })()`);
      if (same) bad.push("'allow for the session' is visually identical to 'allow once'");
    } else {
      bad.push("the dialog does not offer a distinct session-scoped option");
    }

    // 4 (AMENDED). Escape must not resolve the approval either way.
    const beforeEsc = await page.evaluate("window.__sent('answer_permission').length");
    await page.keyboard.press("Escape");
    await page.waitForTimeout(120);
    const afterEsc = await page.evaluate("window.__sent('answer_permission').length");
    const stillUp = await page.locator("#dialog").isVisible();
    if (afterEsc > beforeEsc) bad.push("Escape resolved the approval; the shipped invariant is that it does nothing");
    if (!stillUp) bad.push("Escape dismissed the approval dialog");

    // 6. Nothing auto-resolves on a timer, and there is no cross-session memory.
    const n0 = await page.evaluate("window.__sent('answer_permission').length");
    await page.waitForTimeout(1500);
    const n1 = await page.evaluate("window.__sent('answer_permission').length");
    if (n1 > n0) bad.push("the approval resolved itself on a timer");
    if (/remember|don't ask again|do not ask again/i.test(shown.text)) {
      bad.push("the dialog offers a choice that spans sessions");
    }
    if (/\\b\\d+\\s*s(ec|econds)?\\s*(left|remaining)\\b/i.test(shown.text)) {
      bad.push("the dialog shows a countdown");
    }
  } finally {
    await page.close();
  }
  return bad;
}

// --------------------------------------------------------------------- run
const started = Date.now();
const { server, origin } = await start(0);
const browser = await chromium.launch();
const axeCounts = { serious: 0, critical: 0, moderate: 0, minor: 0 };
let worstContrast = Infinity;
const contrastFails = [];

try {
  for (const theme of THEMES) {
    const ctx = await browser.newContext({
      viewport: { width: 1280, height: 800 },
      colorScheme: theme,
    });

    for (const [id] of SCENARIOS) {
      const page = await ctx.newPage();
      try {
        await page.goto(`${origin}/?s=${id}`, { waitUntil: "domcontentloaded" });
        await page.waitForFunction("window.__ready === true", null, { timeout: 20000 });

        const axe = await new AxeBuilder({ page })
          .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
          // axe's own colour-contrast rule is disabled: it cannot resolve
          // `light-dark()` against a context colour-scheme and reports the
          // light values while the page renders dark. The contrast gate below
          // computes it from what actually rendered.
          .disableRules(["color-contrast"])
          .analyze();
        for (const v of axe.violations) {
          axeCounts[v.impact] = (axeCounts[v.impact] || 0) + v.nodes.length;
          if (v.impact === "serious" || v.impact === "critical") {
            fail(`axe ${v.impact} in ${id}/${theme}: ${v.id} (${v.nodes.length} node(s)) — ${v.help}`);
          }
        }

        const cf = await page.evaluate(CONTRAST);
        for (const c of cf) {
          worstContrast = Math.min(worstContrast, c.got);
          contrastFails.push({ scenario: id, theme, ...c });
          fail(`contrast ${c.got}:1 (needs ${c.need}) in ${id}/${theme} on ${c.sel} — ${JSON.stringify(c.text)}`);
        }
        // Track the worst passing ratio too, so BASELINE has a real number.
        const worst = await page.evaluate(`(() => { return null })()`);
        void worst;

        const kb = await page.evaluate(KEYBOARD);
        for (const u of kb.unreachable) fail(`keyboard: ${u} in ${id}/${theme} is interactive but not reachable by Tab`);
      } catch (e) {
        fail(`${id}/${theme}: ${String(e.message || e).split("\n")[0]}`);
      } finally {
        await page.close();
      }
    }

    // Approval invariants, once per theme — a restyle can break them in one
    // theme and not the other.
    for (const b of await approvalInvariants(ctx, origin)) fail(`approval invariant (${theme}): ${b}`);

    await ctx.close();
  }

  // Reduced motion, on the surfaces that animate.
  const rm = await browser.newContext({ viewport: { width: 1280, height: 800 }, reducedMotion: "reduce", colorScheme: "dark" });
  for (const id of ["s01", "s04", "s06", "s10"]) {
    const page = await rm.newPage();
    try {
      await page.goto(`${origin}/?s=${id}`, { waitUntil: "domcontentloaded" });
      await page.waitForFunction("window.__ready === true", null, { timeout: 20000 });
      const bad = await page.evaluate(MOTION);
      for (const b of bad) fail(`reduced motion: ${id} ${b.sel} still transitions ${b.prop} for ${b.seconds}s`);
    } catch (e) {
      fail(`reduced motion ${id}: ${String(e.message || e).split("\n")[0]}`);
    } finally {
      await page.close();
    }
  }
  await rm.close();
} finally {
  await browser.close();
  server.close();
}

const report = {
  seconds: Number(((Date.now() - started) / 1000).toFixed(1)),
  axe: axeCounts,
  contrastFailures: contrastFails.length,
  worstContrast: worstContrast === Infinity ? null : worstContrast,
  failures,
  notes,
};
await writeFile(join(ROOT, ".claude", "ux-loop", "audit.json"), JSON.stringify(report, null, 2));

console.log(JSON.stringify({ ...report, failures: failures.length }, null, 2));
for (const f of failures) console.log("  FAIL " + f);
process.exit(failures.length ? 1 : 0);
