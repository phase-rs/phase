# Coverage Impact Analysis

**Current: 67.9% (23,311 / 34,313 cards)** — updated 2026-03-12

### Completed Work

| Date | Work | Cards Gained | New Total |
|------|------|-------------|-----------|
| 2026-03-12 | 7 trigger matchers (land/aura/equipment/vehicle enters, leaves, becomes blocked, dealt damage, you attack, cast this spell, becomes tapped) | +901 | 23,311 (67.9%) |

---

## Strategy: Cards Needing Only ONE Handler

These cards flip from unsupported → supported by implementing a single feature.

### Top 25 Single-Handler Unlock Targets

| Rank | Handler | Cards Unlocked | Category |
|------|---------|---------------|----------|
| 1 | Effect:unknown (parser gaps) | 3,643 | Parser |
| 2 | Effect:choose (modal spells) | 350 | New Effect |
| 3 | Effect:regenerate | 230 | New Effect |
| 4 | Effect:prevent | 155 | New Effect |
| 5 | Effect:cast | 127 | New Effect |
| 6 | Effect:the | 112 | Parser |
| 7 | Effect:gain | 95 | Parser |
| 8 | Trigger:When ~ is turned face up | 82 | New Trigger |
| 9 | Trigger:When you set this scheme in motion | 76 | Niche (Archenemy) |
| 10 | Effect:become | 74 | Parser |
| 11 | Effect:get | 64 | Parser |
| 12 | Trigger:Whenever chaos ensues | 64 | Niche (Planechase) |
| 13 | Effect:for | 62 | Parser |
| 14 | Effect:transform | 62 | New Effect |
| 15 | Effect:target | 61 | Parser |
| 16 | Effect:lose | 54 | Parser |
| 17 | Trigger:When ~ is put into graveyard from battlefield | 51 | New Trigger |
| 18 | Trigger:Whenever equipped creature attacks | 48 | New Trigger |
| 19 | Trigger:When enchanted creature dies | 46 | New Trigger |
| 20 | Trigger:When you cycle this card | 46 | New Trigger |
| 21 | Effect:creatures | 45 | Parser |
| 22 | Trigger:Whenever you cast your second spell each turn | 43 | New Trigger |
| 23 | Effect:add | 41 | Parser |
| 24 | Trigger:Whenever equipped creature deals combat damage to a player | 41 | New Trigger |
| 25 | Trigger:Whenever you crank this Contraption | 41 | Niche (Unstable) |

---

## Breakdown: Effect:unknown (parser gaps — 3,643 single-handler cards)

These are Oracle text lines the parser doesn't match. Top patterns by count:

### Aura/Equipment Static Effects (~299 instances)
| Pattern | Count | Description |
|---------|-------|-------------|
| enchanted creature can't ... | 77 | Restriction effects |
| enchanted creature has ... | 73 | Keyword granting |
| enchanted creature doesn't ... | 53 | Restriction effects |
| you control enchanted ... | 41 | Controller-referencing |
| enchanted land is ... | 21 | Land type changing |
| equipped creature has ... | 18 | Keyword granting |
| enchanted creature loses ... | 16 | Keyword removal |

### "As enters" / Replacement Effects (~226 instances)
| Pattern | Count |
|---------|-------|
| as this creature enters | 89 |
| as this enchantment enters | 73 |
| as this artifact enters | 44 |
| as this aura enters | 20 |

### Modal Bullet Points (~577 instances, blocked by Effect:choose)
| Pattern | Count |
|---------|-------|
| • create a ... | 52 |
| • put a ... | 42 |
| • destroy target ... | 29 |
| • you gain ... | 27 |
| • target creature ... | 25 |
| • this creature ... | 21 |
| • return target ... | 17 |
| • target player ... | 17 |
| • each player ... | 16 |

### Static/Continuous Effect Patterns (~198 instances)
| Pattern | Count |
|---------|-------|
| creatures you control ... | 72 |
| each creature you control ... | 57 |
| creatures your opponents ... | 24 |
| other creatures you ... | 23 |
| creature tokens you ... | 22 |

### Casting Permission/Restriction (~308 instances)
| Pattern | Count |
|---------|-------|
| you may cast ... | 112 |
| you may have ... | 62 |
| you may play ... | 58 |
| you may pay ... | 29 |
| your opponents can't ... | 29 |
| you can't cast ... | 18 |

### Self-Reference / Combat (~183 instances)
| Pattern | Count |
|---------|-------|
| this creature attacks ... | 60 |
| this creature can ... | 59 |
| during your turn, ... | 48 |
| this creature has ... | 16 |

### Conditional / Replacement (~119 instances)
| Pattern | Count |
|---------|-------|
| if a source ... | 35 |
| if you would ... | 32 |
| if damage would ... | 28 |
| if a creature ... | 24 |

---

## Missing Triggers (Remaining)

| Trigger | Cards Affected |
|---------|---------------|
| Whenever chaos ensues | 150 (Planechase) |
| When ~ is turned face up | 91 |
| When you set this scheme in motion | 82 (Archenemy) |
| When ~ is put into graveyard from battlefield | 62 |
| When enchanted creature dies | 55 |
| Whenever equipped creature attacks | 54 |
| When you cycle this card | 49 |
| Whenever you cast your second spell each turn | 48 |
| Whenever equipped creature deals combat damage to a player | 43 |
| Whenever ~ or another Ally enters | 41 |
| Whenever you crank this Contraption | 41 (Unstable) |
| Whenever you draw your second card each turn | 40 |
| When this Siege enters | 36 |

---

## Recommended Priority Order (Impact × Feasibility)

### Tier 1: Highest Impact, Buildable Now
1. ~~**New trigger matchers**~~ ✅ DONE (+901 cards)

2. **"Enchanted creature gets/has" parser patterns** → route to existing static abilities
   - ~300 cards unlocked
   - The *engine* already handles granting keywords/P/T via statics
   - The *parser* just doesn't route "enchanted creature has flying" → `StaticMode::GrantKeyword`

3. **Modal spells (Effect:choose)** — "choose one/two" + bullet parsing
   - ~350 cards unlocked (single-handler)
   - Requires new `Effect::Choose` type + UI for modal selection
   - Individual bullets are mostly already-supported effects (create, destroy, pump, draw)

### Tier 2: High Impact, Moderate Complexity
4. **Effect:regenerate** — 230 cards, well-defined mechanic
5. **Effect:prevent** — 155 cards, damage prevention
6. **"As enters" replacement effects** — 226 instances, copy/naming/choice on ETB
7. **Casting permissions** ("you may cast/play from graveyard/exile") — 170+ cards

### Tier 3: Valuable but Complex
8. **Effect:transform** — 62 cards, DFC support
9. **Effect:cast** — 127 cards, "cast from exile/graveyard" alternative costs
10. **Lord/pump patterns** ("creatures you control get +1/+1") — 198 instances

### Tier 4: Remaining Triggers
11. **Equipped/enchanted creature triggers** (attacks, dies, deals combat damage) — ~150 cards
12. **Second spell/card each turn** — 83 cards
13. **Morph (turned face up)** — 82 cards

---

## Quick Reference: All Missing Handler Frequencies

```
4300  Effect:unknown
 490  Effect:choose
 257  Effect:regenerate
 169  Effect:prevent
 159  Effect:cast
 150  Trigger:Whenever chaos ensues
 129  Effect:the
 115  Effect:gain
 102  Effect:get
  91  Trigger:When ~ is turned face up
  88  Effect:become
  82  Trigger:When you set this scheme in motion
  77  Effect:for
  73  Effect:target
  66  Effect:lose
  63  Effect:transform
  62  Trigger:When ~ is put into a graveyard from the battlefield
  59  Effect:creatures
  57  Effect:•
  55  Trigger:When enchanted creature dies
  54  Trigger:Whenever equipped creature attacks
  54  Effect:roll
  54  Effect:add
  49  Trigger:When you cycle this card
  48  Trigger:Whenever you cast your second spell each turn
  47  Effect:up
  44  Effect:put
  43  Trigger:Whenever equipped creature deals combat damage to a player
  43  Effect:remove
  41  Trigger:Whenever you crank this Contraption
  41  Trigger:Whenever ~ or another Ally enters
  40  Trigger:Whenever you draw your second card each turn
  37  Effect:monstrosity
  36  Trigger:When this Siege enters
  36  Effect:each
  35  Effect:pay
  35  Effect:level
  35  Effect:change
  34  Effect:suspend
  33  Effect:shuffle
```

---

## Test Cards by Category

Cards that become fully supported when their single missing handler is implemented.

### Effect:choose / Modal Spells (350 cards unlocked)
- **Run Away Together** — bounce 2 creatures (pauper staple)
- **Balancing Act** — each player sacrifices to match lowest
- **Stolen Uniform** — clone equipment

### Effect:regenerate (230 cards unlocked)
- **Ezuri, Renegade Leader** — elf lord, regenerate all elves
- **Silvos, Rogue Elemental** — regenerate self
- **Trolls / classic green creatures** — core regenerate mechanic

### Effect:prevent (155 cards unlocked)
- **Spore Frog** — fog on a stick (commander staple)
- **Samite Healer** — prevent 1 damage (classic)
- **Festival of the Guildpact** — prevent all combat damage

### Trigger: When ~ is turned face up (82 cards unlocked)
- **Morph creatures** — flip-up trigger effects

### Trigger: When enchanted creature dies (46 cards unlocked)
- **Aura-based death triggers** — return, create tokens

### Trigger: Whenever equipped creature attacks (48 cards unlocked)
- **Equipment attack bonuses** — auto-equip, pump effects
