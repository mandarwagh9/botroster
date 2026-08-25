# OPENBOT design direction

## Thesis
This is a console, not a chat app. Chat is the input method. The product is a set of
named teammates working on one durable computer under your gate. Every screen should
answer: who is working, what are they touching, and what needs me.

## Consequences (these are decisions, not suggestions)
- The roster is a status board first, a contact list second. Status is the loudest thing
  on each row. Name is secondary.
- The thread is a run log, not bubbles. Tool steps are structured rows: verb, target,
  result, duration. Collapsible. Copyable. The model's prose is one row type among several,
  not the frame everything else lives inside.
- The computer is not hidden behind a button. It is a peer pane. It is the differentiator;
  burying it throws the differentiator away.
- Approvals are inline gates at the point in the log where they happened, plus a persistent
  "waiting on you" count in the chrome. A blocking modal is the lazy answer and it trains
  people to click through. See LOOP.md section 3 for what must not change.
- Empty states are the setup path. "No Bots yet" is a screen that creates the first Bot,
  with a real brief pre-filled, not an illustration and a shrug.
- Failure gets the same design attention as success. Guest disconnected, budget exhausted,
  connector 401, unparseable policy rule: each states what happened, what it means, and the
  one action that resolves it. Errors do not apologise and are never vague.

## Signature element
The provenance trail. Every artifact in a thread is traceable to the step that produced it
and the snapshot it landed in. Hovering a file, a browser action, or a command surfaces its
lineage. Nobody else in this category has this. Spend the boldness here and keep everything
around it quiet.

---

# Tokens

Dark is primary. Light must exist and must be as good, not an inverted afterthought.

**The eight below are pinned by hand and are not to be changed by the loop.** Everything
after them is *derived* — marked DERIVED — filled in so an unattended iteration has a
complete system to reach for instead of inventing one at 4am. A derived token may be
revised by the loop only by editing this file in the same commit and saying why.

## Pinned

```
  --base       #0E1116   deep cool graphite, not black
  --raised     #161B22
  --line       #232B36
  --text       #D6DEE8
  --muted      #8593A3
  --waiting    #F2A649   needs a human. the only warm colour in the app.
  --live       #4FD1C5   a bot is working right now
  --refused    #E5674E   denied, failed closed
```

Amber appears only where a person is blocking progress. If amber is on screen and nothing
needs you, that is a bug.

## DERIVED — the accent is neutral, and this is the load-bearing decision

**Colour in this app means status. It never means emphasis.**

The shipped design had a purple accent; DIRECTION bans purple-to-blue, and every hue that
remains is already spoken for — amber is "needs you", teal is "working", vermilion is
"refused". Introducing a fourth hue for "this is the primary button" would put a colour on
screen that carries no state, which is exactly the thing rubric line 18 exists to catch.

So the accent is a **neutral fill**: near-white on dark, near-black on light. A primary
button is the highest-contrast object on the screen without being a new colour. This keeps
the three status hues meaning exactly one thing each.

```
  --accent       light-dark(#10151B, #E8EEF5)   a fill, never a text colour
  --accent-ink   light-dark(#FBFCFD, #0E1116)   text on that fill
  --accent-wash  light-dark(rgba(16,21,27,.08), rgba(232,238,245,.10))
```

## DERIVED — light theme

Light is not the dark values inverted; the status hues have to be re-picked or they fail
contrast as text. `#F2A649` on white is 1.9:1 and unusable. Written as `light-dark()` pairs,
which is what `styles.css` already uses and what the shot harness switches with Playwright's
`colorScheme`.

```
  --base     light-dark(#FBFCFD, #0E1116)
  --raised   light-dark(#F2F5F8, #161B22)
  --line     light-dark(#DFE5EC, #232B36)
  --text     light-dark(#10151B, #D6DEE8)
  --muted    light-dark(#55616F, #8593A3)

  --waiting  light-dark(#8A5100, #F2A649)
  --live     light-dark(#0E6E66, #4FD1C5)
  --refused  light-dark(#B23A22, #E5674E)
```

Status *fills* (chips, dots, the roster status marker) keep the bright dark-mode hue in both
themes, because a fill is not read as text and the amber dot must stay recognisably amber.
Status *text* uses the pair above.

## DERIVED — remaining text tiers

```
  --faint    light-dark(#5F6B7C, #8794A4)   timestamps, durations, row metadata
  --ghost    light-dark(#98A3B1, #5A6675)   placeholders and disabled text ONLY
```

**REVISED 2026-08-25, implementing this file.** `--faint` was `light-dark(#6C7887, #6E7C8C)`
and cleared AA on nothing. Measured against the three surfaces it actually lands on, the dark
value gave 4.43 / 4.06 / 3.75 and the light value 4.37 / 4.11 / 3.85 — so a token this
document declares must clear AA did not, on either theme, on any surface. The paragraph below
exempts `--ghost` and nothing else, which makes the old value an internal contradiction rather
than a judgement call.

The new pair is derived from the requirement instead of chosen by eye: each clears 4.5 against
the *worst* surface it can sit on (`--raise`, the lightest dark step and the darkest light
step), giving 5.18 and 4.64 there and more elsewhere. Same cool-grey family, so the intent —
metadata recedes — survives; it now recedes without becoming unreadable.

`--ghost` does not clear AA and must never carry a label or a value somebody reads. This is
inherited verbatim from the shipped system, and the contrast gate exempts `::placeholder`
and `[disabled]` for exactly this reason. If a gate failure points at `--ghost` on anything
else, the fix is to stop using `--ghost` there, not to lighten the token.

## DERIVED — the eight Bot coats

A Bot's coat is identity, not status, and must never compete with amber/teal/vermilion. So
the coats are low-chroma, and appear in exactly three places, as the shipped system had it:
the Bot's mark, the open roster row, and a hairline on its turns. **A coat is never a
background fill and never a text colour.**

```
  --coat-1  #6E8FA8   slate
  --coat-2  #7C8B6B   moss
  --coat-3  #9A7F8E   mauve
  --coat-4  #55897F   pine
  --coat-5  #A08668   sand
  --coat-6  #7A82A6   iron
  --coat-7  #8E7A6E   clay
  --coat-8  #6F947C   sage
```

All eight sit between 35 and 55 in lightness so they read on both themes without a second
set. None is within 20 degrees of hue of `--waiting`, `--live`, or `--refused`.

## DERIVED — shape

"Rounded-everything" is banned; pick a scale and mean it. A console reads as instrumentation,
so the scale is tight and there are no pills.

```
  --r-1  3px    chips, tags, status markers, inputs
  --r-2  6px    rows, log steps, cards
  --r-3  10px   dialogs, panes, the computer pane
  --r-0  0      tables, the log gutter, anything that tiles
```

## DERIVED — depth

No glassmorphism, no blur. Depth is one hairline and one shadow.

```
  --scrim   light-dark(rgba(16,21,27,.32), rgba(0,0,0,.60))
  --shadow  light-dark(0 12px 32px rgba(16,21,27,.12), 0 16px 40px rgba(0,0,0,.55))
```

## Type

- **UI and display:** Geist Sans. Tight tracking at display sizes, not the default:
  `-0.02em` at 17px and above, `-0.01em` at 15px, `0` below.
- **Log and code:** Commit Mono, falling back to JetBrains Mono. The log is most of the app,
  so this face carries more weight than the display face.
- **Eyebrows, status labels, column headers:** Martian Mono, 10-11px, uppercase, wide
  tracking (`0.08em`). Used sparingly. It is what makes the console read as instrumentation.

Vendored as WOFF2 under `crates/openbot-app/ui/fonts/`, with a PROVENANCE.md row each. They
are not fetched at runtime — this is an offline desktop app and a webfont over the network
would be a request the product should never make. DERIVED fallback stacks:

```
  --sans  "Geist Sans", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif
  --mono  "Commit Mono", "JetBrains Mono", ui-monospace, "SF Mono", Menlo, Consolas, monospace
  --micro "Martian Mono", "Geist Sans", ui-sans-serif, system-ui, sans-serif
```

DERIVED sizes, carried over from the shipped system because seven sizes and three weights was
already the right answer and the re-skin is a change of palette and face, not of scale:
`11 12 13 14 15 17 24`, weights `400 500 600`. Body copy in the thread is 15px on a 640px
measure.

## Motion

One duration and one curve for everything that moves: `160ms` / `cubic-bezier(.2,0,0,1)`.
Entrances translate 4px and fade. Nothing loops. Motion budget is state transitions and the
computer pane only. Honours `prefers-reduced-motion`, where every transition goes to `0ms` —
the gate asserts this on load.

## Banned
- Cream backgrounds with terracotta accents. Near-black with a single acid-green accent.
  Hairline-rule broadsheet layouts. These are the three looks every AI design converges on.
- Gradient text, glassmorphism, purple-to-blue anything.
- Emoji as UI.
- Animation that does not communicate state. Motion budget: state transitions and the
  computer pane only.
- Rounded-everything. Pick a radius scale and mean it.
- **A fourth hue.** Three status colours and a neutral accent is the whole palette. Adding a
  colour that does not encode state is a P0 under rubric line 18.
