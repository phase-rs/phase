# Tablet deck header and sideboard icon phase-fit

## Plan

1. In `client/src/components/draft/workspace/DraftWorkspaceToolbar.tsx`, make the
   sticky toolbar used by tablet draft layouts use an explicitly opaque panel
   background while retaining its existing sticky positioning and behavior.
2. In `client/src/components/draft/workspace/CompactSideboard.tsx`, render the
   expanded tablet-landscape sideboard control from the same upward triangle
   glyph as the collapsed control, rotating it right with `rotate-90` (the
   collapsed `-rotate-90` glyph points left).
3. In `client/src/components/draft/__tests__/PackDisplay.layout.browser.test.tsx`,
   extend the existing tablet layout browser test to assert an opaque computed
   Deck toolbar background for both tablet orientations and the expanded
   tablet-landscape triangle glyph plus rotation. Run that focused browser-layout
   test.

## Phase-fit record

1. Initial adjudication — T1: 2 independently verifiable UI behaviors (opaque
   tablet Deck toolbar; same-glyph right-facing tablet-landscape Sideboard
   control). T2: 3 grouped source/test paths expected; no fixtures or generated
   artifacts counted. T3: no dependency seam requiring a split. T4 axis:
   frontend UI (toolbar, sideboard, browser test); inactive until review rounds
   3+. Verdict: single-step, because T1 is true but T2 is false.
2. Plan-review revision — clarified the expanded tablet-landscape transform as
   `rotate-90` after independent review. T1 remains 2; T2 remains 3 grouped
   paths (the phase-fit record itself is process metadata, not implementation
   review surface); T3 unchanged; T4 has not reached an eligible round. Verdict:
   single-step.

## Gate outcomes

- Plan review 1: no blocking gaps; clarified that `rotate-90` is the
  right-facing transform for `▲`.
- Plan review 2: no gaps.
- Implementation: the sticky toolbar's translucent and opaque backgrounds are
  now mutually exclusive; expanded tablet-landscape Sideboard now uses `▲` with
  `rotate-90`, while collapsed remains `▲` with `-rotate-90`.
- Verification: `pnpm run test:browser:pack-layout` passed (38/38); `git diff
  --check` passed.
- Implementation review: no gaps.
