# Phase tabletop 3D client

## Product intent

This fork treats the battlefield as a spatial digital game surface rather than
a simulation of paper on a flat web page. The target is a restrained, painterly
game stage: broad value grouping, confident card silhouettes, and brief
state-driven motion without environmental spectacle.

The visual system follows four principles:

1. **Readable before spectacular.** Current characteristics, legal affordances,
   combat state, and target state must survive every camera and effect choice.
2. **Depth communicates state.** Hover lifts, attack travel, tap rotation,
   stack height, and impact response carry meaning instead of decorating it.
3. **Engine state is the only gameplay authority.** The scene consumes current
   objects, derived views, legal actions, waiting states, and attribution. It
   never infers legality from card text, art, type names, or animation state.
4. **Art is an ingredient, not the card.** Scryfall provides printing art.
   Phase composes the live name, mana cost, type line, rules surface, stats,
   counters, and modification signals.

## Renderer boundary

`TabletopCardPresentation` is the renderer-neutral visual contract:

```text
engine GameObject + derived presentation state
                    │
                    ▼
          TabletopCardPresentation
              ┌─────┴─────┐
              ▼           ▼
     Composed hand face CanvasTexture
                          │
                          ▼
                   Three.js permanent
```

The local hand remains a DOM interaction surface, but its visible face is a
high-resolution canvas composition shared by hand and inspection modes.
Concealed opponent hands are world-space Three.js seat objects. Both paths use
measured M15 frame geometry, Beleren title/type typography, MPlantin
rules typography, and locally rasterized Scryfall pips. Battlefield permanents
use small, bounded canvas textures that rebuild only when their presentation
revision changes.

Frame, font, and pip assets are runtime-only and gitignored. Follow
[`ASSETS.md`](./ASSETS.md) to bootstrap them for a fresh checkout.

The existing Phase dispatch pipeline remains authoritative. Three.js hit tests
route object IDs into the same engine-provided legal action buckets and
waiting-state responses used by the DOM board.

Critical controls remain screen-space DOM: life totals and player names, phase
and priority controls, pass/resolve actions, the local hand, menus, stack
controls, and zone viewers. They never inherit the world's perspective. This
keeps text crisp and touch targets stable across desktop, iPad, and phone
layouts. Three.js owns battlefield cards, opponent hands and their public
command-zone leaders, piles, combat positions, attachments, and future
environment themes.

## First vertical slice

The first slice targets matches across desktop, tablet, and phone, including
four-player Commander pods:

- live engine state on a perspective material plane;
- current characteristics composed over art crops;
- dynamic DOM hand cards;
- targeting, attacker/blocker selection, activated abilities, mana actions,
  equip targets, board choices, tap undo, inspection, and selection;
- low-cost hover lift, tap spring, attack travel, and semantic glow language;
- capped DPR, demand rendering, no post-processing, and delayed texture
  disposal.

Attachments, combat lines, flight/impact events, stack projection anchors, and
a full semantic rules-text document are the next milestones.

## Responsive stage composition

The tabletop game page is one continuous stage rather than a canvas squeezed
between opponent-hand and player-hand page rows. A large environmental plane
continues beyond every viewport edge; the opponent hand and status plate occupy
its far seat, while the local hand, status plate, and command shelf occupy its
near seat.

The camera adapts composition rather than merely scaling the desktop view.
Wide screens reveal more of the shared environment without introducing a
table perimeter. Compact screens crop that environment so central play lanes
and zone piles stay large enough to read. Screen-space controls remain DOM
overlays with safe-area insets and stable touch targets, and the mobile hand
drawer remains the authoritative compact-screen interaction surface.

Information hierarchy is intentionally asymmetric:

1. the current required decision and pass action;
2. priority, phase, life, and mana;
3. hand and standing automation state;
4. menus and secondary utilities.

The command shelf groups those controls into one translucent near-edge surface
instead of stacking independent floating panels over the battlefield.

## Multiplayer seat hands and command zones

`TabletopGameBoard` assigns every live opponent to a stable `left`, `far`, or
`right` seat with `assignTabletopOpponentSeats`. Each seat receives one
`TabletopHeldHand` group. The group's world position, Y-axis facing angle, and
scale are the single transform authority for both the concealed hand fan and
the public command-zone leaders beside it; command cards are not screen-space
elements with approximate CSS rotations.

Opponent hand presentation is deliberately bounded:

- up to seven cards render in the world-space fan;
- hands above that presentation capacity keep their authoritative engine count
  and show it on a small count sprite beside the fan;
- the HUD nameplate does not duplicate hand size while the visible fan remains
  within capacity;
- engine-filtered reveal/look state remains the only authority for whether a
  held card renders its face.

`commandZoneLeaders` supplies commanders, partners, backgrounds, and
Oathbreaker signature spells that are currently in the command zone. Their
Three.js held-card meshes continue the same seat plane immediately after the
hand fan, so side and far seats inherit identical camera perspective. Clicking
an opponent leader routes its object ID to the existing inspector. The local
player's command-zone cards remain in the DOM hand dock, where
`CommanderCardZone` continues to own casting, commander tax, commander
ninjutsu, drag interaction, and mana-payment preview.

Opponent identity remains screen-space: compact portrait-filled pills show the
life total and smaller name text without a redundant hand count. The persistent
"Follow active opponent" preference lives under Settings → Multiplayer rather
than reserving a top-center gameplay control.

## Second vertical slice

The in-game desktop inspection surface now uses `TabletopCardFace` for the
engine-current active face. This preserves the existing preview's cursor
tracking, hand-origin animation, Shift/side preferences, Alt parse view,
attribution footer, and reporting controls while replacing the printing image
with readable live characteristics. Alternate-face inspection remains on the
printing renderer until the engine exposes a renderer-neutral projection for
the inactive face.

New battlefield permanents also arrive with a restrained lift, scale settle,
and one-shot brass resolve ring. The effect is deliberately attached to the
object mount rather than guessed from card text or zone names.

## Visual language

- A screen-filling blue-gray material plane rather than a freestanding table.
  The surface is a code-only matte material, so the base environment has no
  bundled texture dependency. Its geometry remains completely flat and
  continues far beyond the camera crop.
- A near-black neutral background. Environment themes are a later layer and
  cannot alter the screen-space HUD contract.
- One broad warm upper-left key and one restrained cool lower-right fill.
  Shadows provide spatial separation; point lights and post-processing bloom
  are intentionally absent.
- Cards are matte printed objects: the composed Magic face is the detailed
  element, supported by plain dark paperboard thickness and no foil-like edge
  treatment. Permanents cast real shadows.
- Concealed opponent hands are Three.js seat objects, not DOM ribbons. Each
  opponent receives an upright, bottom-pivoted floating card fan elevated
  above the plane and partially cropped against their authored outer seat edge.
  Side-seat fans remain visually comparable to their own library piles. There is
  no glove, clip, holder, or heavy cast shadow competing with the battlefield.
  The same component covers far, left, and right pod seats; card identities
  still come only from the engine-filtered state and existing reveal/look
  visibility contract. Public command-zone leaders continue the same fan plane
  rather than using separately rotated DOM cards.
- Sea-glass blue: a soft underlight for an engine-authored available action.
- Muted teal: solid corner brackets for an engine-authored legal target.
- Muted copper: a low-energy combat underlight.
- Warm parchment: selected/inspected corner brackets.
- Green edge filament on a card face: engine-attributed live modification.

There is no ambient scene motion. Arrival settling, tap rotation, hover lift,
and combat travel are reserved for state changes and direct input so the board
stays still while the player thinks.

## Live game-feel review

The vertical slice was exercised in a real Commander AI game rather than only
with fixture state. The review covered mulligan-to-main-phase flow, land play,
manual mana, casting, token creation, a continuous +1/+1 effect, and attacker
selection.

What currently earns its place:

- The shallow perspective establishes a duel space without compromising hand
  access or the existing priority controls. The environment deliberately fills
  the available play area rather than floating as a small object over a
  background.
- Land tapping, permanent arrival, hover lift, and attack travel are brief and
  state-linked. The world is still while the player is reading.
- Legal-action sea-glass, modifier green, and combat copper remain
  distinguishable against both green and multicolor art.
- Engine-current 2/2 stats and the modifier filament appeared on a Squirrel
  token immediately after Squirrel Sovereign resolved, which validates the
  primary advantage over static printing images.

What should be rejected or changed in the next slice:

- Permanents resolve directly into a lane; they need projected zone-to-stage
  travel and impact timing before spell resolution has Master Duel-like weight.
- The stable left/far/right pod composition keeps all three opponent hands and
  public command-zone leaders visible without adding dashboard rows. Future
  focus transitions may still add camera emphasis, but are not required for
  basic four-player readability.
- Sound and particles should reinforce engine events only after projected
  anchors exist. Generic ambient spectacle would make priority-heavy Magic
  harder to read.

## Performance constraints

- Battlefield textures are 640×420 without mipmaps.
- Texture entries are revision-keyed, shared, reference-counted, and disposed
  after a short reuse window.
- Canvas DPR is capped at 2.
- The material plane uses a code-only standard material with scalar roughness;
  it loads no color, normal, roughness, or displacement texture.
- The R3F loop uses `frameloop="demand"`; springs invalidate only while moving,
  with no ambient invalidation loop.
- Geometry is deliberately simple and post-processing is absent in the first
  slice. The only custom shader is the analytic actionable-card underlight.
- Full-card text remains DOM-only; battlefield textures prioritize glance
  recognition.
- Scryfall's image CDN is intentionally suitable for opaque `<img>` requests,
  not canvas/WebGL composition. Composited cards therefore use fixed-host,
  same-origin image transports (`/card-image-art` and `/card-image-back`).
  Vite supplies those routes for local proof-of-concept development.

## Known first-slice limitations

- Attachments and exile links are not yet spatialized.
- The old DOM animation position registry does not yet receive projected
  Three.js anchors, so some event overlays will not originate from the mesh.
- Rules text currently combines the printed text surface with engine-current
  characteristics and an attribution signal. Arbitrary phrase-level changes
  require a future engine-authored structured rules-text projection.
- The stack remains an existing DOM surface.
- Inactive-face inspection still uses printing images. It should adopt the live
  face once its renderer-neutral projection is available without reconstructing
  characteristics in React.
