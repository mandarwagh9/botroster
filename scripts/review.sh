#!/bin/sh
# The BOTROSTER product review gate.
#
# scripts/ux-verify.sh is the frontend gate and stays the authority there; this is the
# whole-product one and it *calls* that script rather than reimplementing any of it. Two gates
# with two opinions about the same CSS is how a project ends up with two definitions of done.
#
# Why a script and not a checklist: the UX loop compounded because a failing gate reverted a
# change without anyone arguing with it, and a review that only produces prose gets read once.
# Every check here is mechanical, and every one of them is a rule this repository already states
# somewhere in writing - the bar in CONTRIBUTING.md, the two invariants in CLAUDE.md, and the
# "nothing enters without a row" rule in PROVENANCE.md. The job of this script is to stop those
# rules from being aspirational.
#
#   sh scripts/review.sh            structural + honesty gates  (seconds, no build)
#   sh scripts/review.sh --full     the above plus fmt, clippy, the test suite and the UX gate
#
# Exit 0 = clean. Exit 1 = at least one gate failed. Findings land in
# .claude/product-review/data/gate.log and as one line each on stdout.

set -u

cd "$(dirname "$0")/.." || exit 2
OUT=.claude/product-review/data
mkdir -p "$OUT"
LOG="$OUT/gate.log"
: > "$LOG"

FAILED=0
FULL=0
[ "${1:-}" = "--full" ] && FULL=1

pass() {
	printf '  ok    %s\n' "$1"
	printf 'ok    %s\n' "$1" >> "$LOG"
}

fail() {
	FAILED=$((FAILED + 1))
	printf '  FAIL  %s\n' "$1"
	printf 'FAIL  %s\n' "$1" >> "$LOG"
	if [ -n "${2:-}" ]; then
		printf '%s\n' "$2" | sed 's/^/        /'
		printf '%s\n' "$2" | sed 's/^/        /' >> "$LOG"
	fi
}

echo "== structural invariants =="

# G1/G2. CLAUDE.md: "botroster-guest must never be able to reach botrosterd", and "the policy gate
# stays in the hub". Both are properties of the dependency graph.
#
# crates/botroster-guest/tests/isolation.rs is the authority on this and is a better test than
# anything this script could be: it walks the workspace manifests, follows indirect routes,
# resolves renamed packages (`store = { package = "botrosterd" }` would defeat a name check), and
# pins the location of the credential store so that moving secrets into a crate the guest already
# depends on fails there rather than being discovered later. What follows is a cheap pre-check
# that catches the direct case in a third of a second without a build - not a second opinion.
#
# It matches that test's definition of reachable and must keep matching it: normal and build
# dependencies count, dev-dependencies do not. A test may use anything it likes; the invariant is
# about the shipped binary. The first version of this check used a bare `cargo tree`, which
# includes dev-dependencies, and duly reported that botroster-agent depends on botrosterd - true of
# its test profile, false of everything that ships, and exactly the kind of alarm that gets a
# gate switched off within a week.
for crate in botroster-guest botroster-agent; do
	tree=$(cargo tree -p "$crate" -e normal,build --prefix none 2>/dev/null)
	if [ -z "$tree" ]; then
		fail "$crate: cargo tree produced nothing, so isolation is unverified"
	elif printf '%s' "$tree" | grep -q '^botrosterd '; then
		fail "$crate ships a dependency on botrosterd" "$(printf '%s' "$tree" | grep '^botrosterd ')"
	else
		pass "$crate cannot reach botrosterd"
	fi
done

# G3. The isolation test is the enforcement; the check above is only a fast echo of it. Deleting
# it is a silent weakening no other gate would notice, because a suite with one fewer test still
# passes. Both of its load-bearing cases are named here, so that gutting the file while leaving
# it in place fails too.
iso=crates/botroster-guest/tests/isolation.rs
if [ ! -f "$iso" ]; then
	fail "$iso is gone, and it is what enforces the credential boundary"
elif ! grep -q 'fn the_guest_cannot_reach_the_credential_store' "$iso" \
	|| ! grep -q 'fn the_walk_actually_follows_indirect_routes' "$iso"; then
	fail "$iso no longer contains the cases that make it load-bearing"
else
	pass "the guest isolation test still exists, with its indirect-route case"
fi

# G4. rust-version is described in CLAUDE.md as "a promise to anyone on an older toolchain". It
# said 1.82 while the code needed 1.89 once already. The promise and the job that keeps it true
# live in two files, so they can drift apart again.
msrv=$(grep -m1 '^rust-version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
ci=$(grep -m1 'rust-toolchain@1\.' .github/workflows/ci.yml | sed 's/.*rust-toolchain@//')
if [ -n "$msrv" ] && [ "$msrv" = "$ci" ]; then
	pass "MSRV promise ($msrv) matches the job that keeps it"
else
	fail "MSRV drift: Cargo.toml says '$msrv', ci.yml pins '$ci'"
fi

echo "== honesty gates =="

# G5. CLAUDE.md: "Do not write documentation, comments or UI copy that implies isolation the
# project does not have." The guest is an ordinary process running as the user; fs.* refuses paths
# escaping the workspace root, but this code enforces that and not the operating system. It is the
# one claim in the project that is both the most tempting to make and the most damaging to make
# falsely - a person who believes the guest is contained will hand it a repository they would not
# otherwise - so it gets a scanner rather than a promise.
#
# A plain word scan is unusable here and the first version proved it: five hits, all legitimate.
# Prose about the *reference* product's sandbox is accurate and belongs in the architecture spec,
# "sandbox" also names a config file format, "credential isolation" is a different property that
# BOTROSTER genuinely has, and a Bot's coat goes on a DOM container. A gate that cries wolf five
# times out of five gets deleted, so this one is a ratchet instead: every occurrence reviewed and
# recorded in isolation-allowlist.txt with the reason it is fine, and anything not on that list
# fails. The cost is a line of upkeep when the text legitimately changes. The benefit is that a
# newly written overclaim cannot arrive unnoticed, which the noisy version could not deliver.
# The scan covers Rust source as well as prose, and that was not the original scope. It read
# README.md, docs/SPEC.md and the UI only, on the reasoning that shipped *text* is what a user
# believes. The Guest & Tools review found the hole: `crates/botroster-guest/src/browser.rs` carried
# the comment "the guest is already a sandbox", and it was not idle prose — it was the stated
# justification for passing `--no-sandbox` to Chrome, switching off the renderer sandbox in the one
# process that parses pages a model chose. A false claim in a comment had become a live defect,
# below the floor of a gate aimed at documentation.
#
# The wider scan then showed the claim was not one comment but a vocabulary: fifteen places across
# five crates called the guest "a sandbox". Nobody wrote a lie; each author read the neighbouring
# comments and matched them.
ALLOW=.claude/product-review/isolation-allowlist.txt
hits=$(grep -rniE '(sandbox|isolat|virtual machine|container)' \
	README.md docs/SPEC.md crates/botroster-app/ui/ crates/*/src/ 2>/dev/null || true)
unreviewed=""
IFS='
'
for line in $hits; do
	ok=0
	if [ -f "$ALLOW" ]; then
		while IFS='	' read -r pat _reason; do
			case "$pat" in
			'' | '#'*) continue ;;
			esac
			case "$line" in
			*"$pat"*)
				ok=1
				break
				;;
			esac
		done < "$ALLOW"
	fi
	[ "$ok" -eq 0 ] && unreviewed="$unreviewed$line
"
done
unset IFS
n=$(printf '%s' "$unreviewed" | grep -c . || true)
if [ "${n:-0}" -eq 0 ]; then
	pass "every containment word in shipped text is reviewed and allowed"
else
	fail "$n containment claim(s) not reviewed - add to $ALLOW with a reason, or fix the text" \
		"$(printf '%s' "$unreviewed" | head -12)"
fi

# G6. CONTRIBUTING.md says a doc comment containing "never" or "always" is a claim, and a claim
# gets a test. This cannot check that the test is correct, so it records the count and the list
# instead of failing. A hard gate on a heuristic here would only teach people to stop writing
# "never", which makes the documentation worse and the property no better tested.
grep -rn '///.*\(never\|always\)' crates/ 2>/dev/null | grep -v '/target/' > "$OUT/claims.txt" || true
# `grep -c` prints 0 and *exits 1* when it counts nothing, so the obvious
# `$(grep -c . f || echo 0)` yields the two-line string "0\n0" and every later
# arithmetic test on it is a syntax error. `|| true` keeps the printed count.
abs=$(grep -c . "$OUT/claims.txt" 2>/dev/null || true)
abs=${abs:-0}
printf '  note  %s absolute claim(s) in doc comments - data/claims.txt\n' "$abs"
printf 'note  %s absolute doc claims\n' "$abs" >> "$LOG"

# G7. CONTRIBUTING.md: "A test that cannot fail" is not merged. A test function whose body holds
# no assertion, no expect, no unwrap, no ? and no panic cannot fail except by hanging.
#
# The body is found by matching braces, and the first version counted braces in the raw source.
# That reported eleven assertion-free tests, all of which had assertions: a test feeding a parser
# the string "this is not toml {{{" left the depth counter three deep, and every test after it in
# the file was measured against the wrong closing brace. The lesson generalises past this script -
# a brace counter that has not been told about string literals is wrong on exactly the files that
# test a parser, which is to say the files where it matters. Literals and comments are blanked to
# spaces before counting, so offsets stay valid.
python - > "$OUT/weak-tests.txt" 2>/dev/null <<'PYEOF'
import re, pathlib

def blank(src):
    """Replace the contents of strings, chars and comments with spaces.

    Length is preserved so that indexes into the result still address the same
    characters in the original. Rust raw strings (r"..", r#".."#) are handled
    because a test fixture is the most likely place to find an unbalanced brace.
    """
    out, i, n = list(src), 0, len(src)
    while i < n:
        c = src[i]
        if c == "r" and i + 1 < n and src[i + 1] in '#"':
            j = i + 1
            hashes = 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                close = '"' + "#" * hashes
                end = src.find(close, j + 1)
                end = n if end < 0 else end + len(close)
                for k in range(i, end):
                    out[k] = " "
                i = end
                continue
        if c == '"':
            j = i + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == '"':
                    j += 1
                    break
                j += 1
            for k in range(i, min(j, n)):
                out[k] = " "
            i = j
            continue
        if c == "'":
            m = re.match(r"'(?:\\.|[^'\\])'", src[i:])
            if m:
                for k in range(i, i + m.end()):
                    out[k] = " "
                i += m.end()
                continue
        if src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            for k in range(i, j):
                out[k] = " "
            i = j
            continue
        if src.startswith("/*", i):
            j = src.find("*/", i)
            j = n if j < 0 else j + 2
            for k in range(i, j):
                out[k] = " "
            i = j
            continue
        i += 1
    return "".join(out)

hits = []
for p in pathlib.Path("crates").rglob("*.rs"):
    if "target" in p.parts:
        continue
    src = p.read_text(encoding="utf-8", errors="replace")
    scan = blank(src)
    pat = r"#\[(?:tokio::)?test\][^\n]*\n(?:\s*#\[[^\]]*\]\s*\n)*\s*(?:async\s+)?fn\s+(\w+)"
    for m in re.finditer(pat, src):
        name = m.group(1)
        i = scan.find("{", m.end())
        if i < 0:
            continue
        depth, body = 0, ""
        for j in range(i, len(scan)):
            depth += (scan[j] == "{") - (scan[j] == "}")
            if depth == 0:
                body = src[i:j]
                break
        if not re.search(r"assert|expect\(|unwrap\(|panic!|\?;", body):
            hits.append(str(p).replace("\\", "/") + ":" + name)
print("\n".join(hits))
PYEOF
weak=$(grep -c . "$OUT/weak-tests.txt" 2>/dev/null || true)
weak=${weak:-0}
if [ "${weak:-0}" -eq 0 ]; then
	pass "every test contains something that can fail"
else
	fail "$weak test(s) contain nothing that can fail" "$(head -8 "$OUT/weak-tests.txt")"
fi

# G8. PROVENANCE.md: "nothing enters this repository without a row in this table." Source files
# are covered by the licence header story; what this catches is the case that actually happened -
# a binary asset committed with no row, noticed a turn later and only by hand.
# Coverage is decided by glob-matching each asset against the paths PROVENANCE.md quotes in
# backticks, so one row can legitimately cover a set (`crates/botroster-app/icons/*`, `docs/botroster-*.png`)
# and does so by saying which set.
#
# Two earlier versions of this were wrong in opposite directions and both are worth recording. The
# first accepted a bare directory-name match, so a single recorded file silently vouched for every
# future sibling — planting docs/brand/unrecorded.svg passed, because docs/brand/app-icon-source.png
# had put the substring "docs/brand" in the table. The second demanded a literal `dir/*` and could
# not read a row that legitimately globbed by filename. Substring matching on a path is the trap in
# both: paths nest, so a substring test answers a question about prefixes, not about coverage.
patterns=$(grep -oE '`[A-Za-z0-9_./*-]+\.(png|jpg|jpeg|svg|ico|ttf|woff2?|otf)`|`[A-Za-z0-9_./-]+/\*`' PROVENANCE.md \
	| tr -d '`' | sort -u)
missing=""
for f in $(git ls-files '*.png' '*.jpg' '*.jpeg' '*.svg' '*.ico' '*.ttf' '*.woff' '*.woff2' '*.otf' 2>/dev/null); do
	ok=0
	for pat in $patterns; do
		# Unquoted on purpose: `case` is the only glob matcher POSIX sh has, and
		# quoting the pattern would turn `*` back into a literal asterisk.
		case "$f" in
		$pat)
			ok=1
			break
			;;
		esac
	done
	if [ "$ok" -eq 0 ]; then
		missing="$missing$f
"
	fi
done
if [ -z "$missing" ]; then
	pass "every committed binary asset has a PROVENANCE row"
else
	cnt=$(printf '%s' "$missing" | grep -c .)
	fail "$cnt asset(s) with no PROVENANCE row" "$(printf '%s' "$missing" | head -8)"
fi

if [ "$FULL" -eq 1 ]; then
	echo "== the bar (CONTRIBUTING.md) =="
	if cargo fmt --all --check >/dev/null 2>&1; then
		pass "cargo fmt"
	else
		fail "cargo fmt --all --check"
	fi
	if cargo clippy --workspace --all-targets -- -D warnings > "$OUT/clippy.log" 2>&1; then
		pass "cargo clippy"
	else
		fail "cargo clippy" "$(grep -m3 '^error' "$OUT/clippy.log")"
	fi
	if cargo test --workspace > "$OUT/test.log" 2>&1; then
		pass "cargo test --workspace"
	else
		fail "cargo test" "$(grep -m5 'FAILED\|panicked at' "$OUT/test.log")"
	fi
	echo "== frontend (delegated to scripts/ux-verify.sh) =="
	if sh scripts/ux-verify.sh > "$OUT/ux.log" 2>&1; then
		pass "ux-verify"
	else
		fail "ux-verify" "$(grep -m5 'FAIL\|RESULT' "$OUT/ux.log")"
	fi
fi

echo
if [ "$FAILED" -eq 0 ]; then
	echo "RESULT pass"
	exit 0
fi
echo "RESULT fail ($FAILED gate(s))"
exit 1
