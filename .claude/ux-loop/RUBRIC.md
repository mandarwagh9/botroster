# RUBRIC

Every line must be answerable by looking at a screenshot. If two people could disagree about
whether it is met, rewrite it. Score each 0-3. A justification that does not name a specific
screenshot file is not a justification.

1. From the cold-start screen (s01), is the next action unambiguous without reading docs?
2. On the roster (s04), can you separate the four statuses in under one second, from
   position and colour alone, without reading the labels?
3. Is exactly one thing the loudest element on each screen? Name it per scenario.
4. In a mid-run thread (s05), can you tell what the Bot is doing *right now*, as distinct
   from what it already did?
5. Does every tool step state its target, not just its verb? "Ran a command" fails.
   "shell.exec  cargo test --workspace" passes.
6. Is the computer pane visible without an interaction in the default layout?
7. At the approval gate (s06), is the consequential choice visually subordinate to the
   safe one, and is the full argument list readable before any button is reachable?
8. Does a denied-by-policy state (s07) read as final, with no control implying appeal?
9. In a 200-step thread (s08), can you find the last approval you granted in under
   three actions?
10. In a group thread (s09), is it unambiguous which Bot produced which step?
11. During takeover (s10), is it obvious that the Bot is locked out and how to release it?
12. Does every failure state (s11) name the cause and offer exactly one resolving action?
13. Are all three routine states (s12) distinguishable without reading?
14. Is every empty state a path to the thing being empty, not an illustration?
15. Does the provenance trail exist and is it reachable from any artifact in the log?
16. Is light theme equal in quality to dark, judged on the same lines?
17. Does anything on screen look like it came from a component library default?
18. Is amber present anywhere nothing requires a human? (Any yes is an automatic P0.)
19. Would a screenshot of this be recognisable as the reference product with the name
    swapped? (Any yes is an automatic P0.)
20. Take the Chanel test: name the one element you would remove from this screen.
    If you cannot name one, the screen is under-designed, not finished.

## Scoring

- **0** — absent, or actively wrong.
- **1** — present but a person would get it wrong on first look.
- **2** — correct, unremarkable.
- **3** — correct and the screen is better for how it is done.

Lines 18 and 19 are inverted: a "yes" is a P0 defect regardless of the numeric score, and
the numeric score for those lines is 3 when the answer is no and 0 when it is yes.

Lines 6, 15 and 11 describe surfaces that may not exist yet. Score them 0 and file the
defect; do not score them N/A and move on. An absent differentiator is the highest-value
defect in the file, not an exemption.
