# Pre-built Commander Decks

Pre-constructed Commander decks for immediate play. These are defined in
`client/src/data/commanderPrecons.ts` and automatically seeded into the deck
gallery when Commander format is selected.

## Included Decks

| Deck | Commander | Colors | Strategy |
|------|-----------|--------|----------|
| Heavenly Blades | Kemba, Kha Regent | W | Equipment and tokens |
| Shadow Schemes | Sylas Darkblade | UB | Control and card advantage |
| Primal Force | Ruric Thar, the Unbowed | RG | Stompy and ramp |
| Esper Authority | Sharuum the Hegemon | WUB | Artifact control |
| Naya Unleashed | Mayael the Anima | RGW | Creature aggro |

Each deck contains 100 cards (99 + commander), follows singleton rules, and
respects color identity constraints. Cards are chosen from the engine's existing
card database.
