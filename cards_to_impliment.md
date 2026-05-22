# Cards to Implement

Cards sourced from staging coverage data (85.1% coverage, 34,708 total cards).
All entries have `gap_count = 1` — one missing feature per card.
Grouped by the gap pattern each card represents.

---

## Until-End-of-Turn Mana Triggers (2 cards)

Needs `until end of turn, whenever [event], [mana effect]` — a temporary triggered mana ability.

| Card | Gap |
|------|-----|
| **High Tide** | `Until end of turn, whenever a player taps an Island for mana, that player adds an additional {U}.` |
| **Don't Move** | `Until your next turn, whenever a creature becomes tapped, destroy it.` |

---

## Force Opponents' Creatures to Attack (1 card)

Needs an activated ability that sets creatures an opponent controls to attack this turn if able.

| Card | Gap |
|------|-----|
| **Grisly Anglerfish** | `{6}: Creatures your opponents control attack this turn if able.` |

---

## Damage Equal to Damage Already Dealt (1 card)

Needs a `QuantityRef` for "damage dealt to target this turn" — a dynamic quantity tracking in-turn damage history.

| Card | Gap |
|------|-----|
| **Whipkeeper** | `{T}: This creature deals damage to target creature equal to the damage already dealt to it this turn.` |

---

## For-Each Creature That Died This Turn (1 card)

Needs `for each nontoken creature you controlled that died this turn` as a for-each quantity source.

| Card | Gap |
|------|-----|
| **Tobias, Doomed Conqueror** | `When Tobias dies, for each nontoken creature you controlled that died this turn, create a 2/2 black Zombie creature token.` |

---

## Shared Creature Type Condition (1 card)

Needs a `Condition` variant for "shares a creature type with a creature you control."

| Card | Gap |
|------|-----|
| **Descendants' Path** | `If it's a creature card that shares a creature type with a creature you control, you may cast it without paying its mana cost.` |

---

## Suppress Activated Abilities This Turn (1 card)

Needs a duration-scoped ability suppression effect: `That permanent's activated abilities can't be activated this turn.`

| Card | Gap |
|------|-----|
| **Interdict** | `Counter target activated ability. That permanent's activated abilities can't be activated this turn.` |

---

## Mana of Any Type a Gate Could Produce (1 card)

Needs a mana-production variant that inherits the color options from another permanent type (Gates).

| Card | Gap |
|------|-----|
| **Plaza of Harmony** | `{T}: Add one mana of any type that a Gate you control could produce.` |

---

## Toughness-Based Trigger (1 card)

Needs a static trigger that fires when a toughness threshold is met among creatures you control.

| Card | Gap |
|------|-----|
| **Endangered Armodon** | `When you control a creature with toughness 2 or less, sacrifice this creature.` |
