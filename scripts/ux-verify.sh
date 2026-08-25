#!/usr/bin/env sh
# The gates. Exit non-zero on any failure; print a machine-readable summary last.
#
# A change that fails one of these gets reverted, not argued with. Two of the
# gates in LOOP.md needed translating rather than inventing, and the honest
# translation is recorded here:
#
#   "frontend typecheck and production build" — there is no TypeScript and no
#   bundler. `crates/openbot-app/ui/` is three hand-written files served
#   verbatim by Tauri. The real equivalent is a syntax check on the shipped
#   script plus `cargo test -p openbot-app --test page`, which drives that
#   exact file in a real browser and is where the behaviour is actually pinned.
#
#   "bundle size" — the *compressed* byte size of ui/*.{html,js,css} plus any
#   vendored fonts. Baseline in .claude/ux-loop/.baseline-bytes, fail on >15%
#   growth. The file is named for the unit on purpose: it is gitignored, so a
#   machine that ran the old gate still has a `.baseline-bytes` holding a raw
#   number. Read as a compressed one it would put the ceiling at three times
#   the real size and pass everything forever — a gate that has silently
#   stopped gating, which is worse than one that fails. A new name means that
#   file is simply not this file, and the baseline is recorded on first run.
#
#   Measured gzipped rather than raw, for the reason in BASELINE.md:
#   this repository requires comments explaining what broke and why, comments
#   were 44% of the raw bundle, and a raw ceiling is therefore mostly a limit
#   on how much of the reasoning survives. Gzip discounts prose by roughly ten
#   to one and counts novel code close to full, which is the thing the ceiling
#   was put there to watch.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
LOOP=".claude/ux-loop"
fails=0
note() { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }

# A held binary makes `cargo check` fail with "Access is denied" and the tests
# then run against a stale build, which looks like a pass. Catch it here rather
# than five iterations later.
#
# Only a binary running *out of this checkout* can hold this build. The first
# version of this check matched any process named `openbot*`, which blocked the
# whole gate on the installed app in %LOCALAPPDATA%\OPENBOT — a different file
# on a different path that cargo never writes to. A gate that stops the run for
# something harmless gets switched off, so it matches on the path now.
step "preflight"
if command -v powershell >/dev/null 2>&1; then
  win_root=$(pwd -W 2>/dev/null | tr '/' '\\' || true)
  if [ -n "$win_root" ]; then
    held=$(powershell -NoProfile -Command \
      "Get-Process -Name openbot,openbot-app -ErrorAction SilentlyContinue |
       Where-Object { \$_.Path -and \$_.Path.StartsWith('$win_root', 'OrdinalIgnoreCase') } |
       ForEach-Object { \$_.Path }" 2>/dev/null | tr -d '\r' | grep . || true)
    if [ -n "$held" ]; then
      note "FAIL preflight: a binary from this checkout is running and will hold the build:"
      printf '  %s\n' $held
      exit 1
    fi
  fi
fi
if [ ! -e crates/openbot-app/binaries/openbot-x86_64-pc-windows-msvc.exe ] \
   && [ -z "$(ls crates/openbot-app/binaries/ 2>/dev/null)" ]; then
  note "FAIL preflight: no sidecar staged; run sh scripts/sidecar.sh"
  exit 1
fi
note "ok preflight"

step "rust: check + clippy"
if cargo check -p openbot-app -p openbot-desktop --tests >/dev/null 2>"$LOOP/.check.log"; then
  note "ok cargo check"
else
  note "FAIL cargo check"; tail -30 "$LOOP/.check.log"; fails=$((fails+1))
fi
if cargo clippy -p openbot-app -p openbot-desktop --all-targets -- -D warnings >/dev/null 2>"$LOOP/.clippy.log"; then
  note "ok clippy"
else
  note "FAIL clippy"; tail -30 "$LOOP/.clippy.log"; fails=$((fails+1))
fi

step "rust: fmt"
if cargo fmt --all -- --check >/dev/null 2>&1; then
  note "ok fmt"
else
  note "FAIL fmt (run cargo fmt --all)"; fails=$((fails+1))
fi

step "frontend: syntax + the shipped-JS browser suite"
if node --check crates/openbot-app/ui/main.js >/dev/null 2>&1; then
  note "ok main.js parses"
else
  note "FAIL main.js does not parse"; fails=$((fails+1))
fi
if node --check "$LOOP/fixture/scenarios.js" >/dev/null 2>&1; then
  note "ok scenarios.js parses"
else
  note "FAIL scenarios.js does not parse"; fails=$((fails+1))
fi
# The page suite is the only thing pinning main.js's approval queue, refusal
# mapping and credential handling. The loop edits that file, so this runs every
# iteration despite the cost.
if cargo test -p openbot-app --test page >"$LOOP/.page.log" 2>&1; then
  note "ok page suite ($(grep -oE '[0-9]+ passed' "$LOOP/.page.log" | tail -1))"
else
  note "FAIL page suite"; grep -E "^(test |failures:|---- )" "$LOOP/.page.log" | tail -25; fails=$((fails+1))
fi

step "browser gates: axe, contrast, keyboard, reduced motion, approval invariants"
if node scripts/ux-audit.mjs >"$LOOP/.audit.log" 2>&1; then
  note "ok browser gates"
else
  note "FAIL browser gates"; grep "FAIL " "$LOOP/.audit.log" | head -40; fails=$((fails+1))
fi

step "bundle size"
# `-9` pinned, so the number does not move because a different gzip shipped a
# different default. The fonts are counted as they are: WOFF2 is already
# compressed and gzipping it again would measure the compressor, not the font.
bytes=$(cat crates/openbot-app/ui/index.html crates/openbot-app/ui/main.js crates/openbot-app/ui/styles.css 2>/dev/null | gzip -9 | wc -c)
fontbytes=0
if [ -d crates/openbot-app/ui/fonts ]; then
  fontbytes=$(find crates/openbot-app/ui/fonts -type f -exec cat {} + 2>/dev/null | wc -c)
fi
total=$((bytes + fontbytes))
if [ -f "$LOOP/.baseline-gzip-bytes" ]; then
  base=$(cat "$LOOP/.baseline-gzip-bytes")
  limit=$((base * 115 / 100))
  if [ "$total" -gt "$limit" ]; then
    note "FAIL bundle grew to $total compressed bytes, over the 15% ceiling of $limit (baseline $base)"
    fails=$((fails+1))
  else
    note "ok bundle $total compressed bytes (baseline $base, ceiling $limit)"
  fi
else
  echo "$total" > "$LOOP/.baseline-gzip-bytes"
  note "ok bundle $total bytes (baseline recorded)"
fi

step "summary"
audit_json="$LOOP/audit.json"
printf 'GATES total_failures=%s bundle_gzip_bytes=%s' "$fails" "$total"
if [ -f "$audit_json" ]; then
  printf ' %s' "$(node -e "const a=require('./$audit_json');process.stdout.write('axe_serious='+(a.axe.serious||0)+' axe_critical='+(a.axe.critical||0)+' contrast_failures='+a.contrastFailures+' worst_contrast='+(a.worstContrast===null?'none':a.worstContrast))" 2>/dev/null || echo '')"
fi
printf '\n'

if [ "$fails" -gt 0 ]; then
  note "RESULT fail ($fails gate(s))"
  exit 1
fi
note "RESULT pass"
