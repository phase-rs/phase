//! Real printed cards the P3 referent-predicate witnesses use. Every field is copied from the Scryfall API
//! (`cards/named?exact=`, fetched 2026-11-17): name, mana cost, type line, printed power/toughness and Oracle text.
//! `results/111/scripts/check_printed.py` re-checks this table against the API; `fixture::add_*` build each card with
//! its printed type line, and the `printed_*` test in `fixture.rs` asserts the built object carries it.
#![allow(dead_code)]

/// One real card as printed.
pub struct Printed {
    pub name: &'static str,
    pub cost: &'static str,
    pub type_line: &'static str,
    /// Printed power/toughness; `None` for a noncreature card.
    pub pt: Option<(i32, i32)>,
    /// The keyword names MTGJSON/Scryfall list for the card (they let the parser read keyword-only lines).
    pub keywords: &'static [&'static str],
    pub oracle: &'static str,
}

pub const ANGELIC_WALL: Printed = Printed {
    name: "Angelic Wall",
    cost: "{1}{W}",
    type_line: "Creature — Wall",
    pt: Some((0, 4)),
    keywords: &["Flying", "Defender"],
    oracle: "Defender (This creature can't attack.)\nFlying",
};

pub const ANOINT_WITH_AFFLICTION: Printed = Printed {
    name: "Anoint with Affliction",
    cost: "{1}{B}",
    type_line: "Instant",
    pt: None,
    keywords: &["Corrupted"],
    oracle: "Exile target creature if it has mana value 3 or less.\nCorrupted — Exile that creature instead if its controller has three or more poison counters.",
};

pub const BRACKISH_BLUNDER: Printed = Printed {
    name: "Brackish Blunder",
    cost: "{1}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &["Explore"],
    oracle: "Return target creature to its owner's hand. If it was tapped, create a Map token. (It's an artifact with \"{1}, {T}, Sacrifice this token: Target creature you control explores. Activate only as a sorcery.\")",
};

pub const BURNING_HANDS: Printed = Printed {
    name: "Burning Hands",
    cost: "{1}{R}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle: "Burning Hands deals 2 damage to target creature or planeswalker. If that permanent is green, Burning Hands deals 6 damage instead.",
};

pub const CARNIVOROUS_CANOPY: Printed = Printed {
    name: "Carnivorous Canopy",
    cost: "{2}{G}",
    type_line: "Sorcery",
    pt: None,
    keywords: &["Proliferate"],
    oracle: "Destroy target artifact, enchantment, or creature with flying. If that permanent's mana value was 3 or less, proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)",
};

pub const CHOKING_SANDS: Printed = Printed {
    name: "Choking Sands",
    cost: "{1}{B}{B}",
    type_line: "Sorcery",
    pt: None,
    keywords: &[],
    oracle: "Destroy target non-Swamp land. If that land was nonbasic, Choking Sands deals 2 damage to the land's controller.",
};

pub const COUNTERSPELL: Printed = Printed {
    name: "Counterspell",
    cost: "{U}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle: "Counter target spell.",
};

pub const DARKSTEEL_INGOT: Printed = Printed {
    name: "Darksteel Ingot",
    cost: "{3}",
    type_line: "Artifact",
    pt: None,
    keywords: &["Indestructible"],
    oracle: "Indestructible (Effects that say \"destroy\" don't destroy this artifact.)\n{T}: Add one mana of any color.",
};

pub const DEEKAH_FRACTAL_THEORIST: Printed = Printed {
    name: "Deekah, Fractal Theorist",
    cost: "{4}{U}",
    type_line: "Legendary Creature — Human Wizard",
    pt: Some((3, 3)),
    keywords: &["Magecraft"],
    oracle: "Magecraft — Whenever you cast or copy an instant or sorcery spell, create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is that spell's mana value.\n{3}{U}: Target creature token can't be blocked this turn.",
};

pub const ENDLESS_SWARM: Printed = Printed {
    name: "Endless Swarm",
    cost: "{5}{G}{G}{G}",
    type_line: "Sorcery",
    pt: None,
    keywords: &["Epic"],
    oracle: "Create a 1/1 green Snake creature token for each card in your hand.\nEpic (For the rest of the game, you can't cast spells. At the beginning of each of your upkeeps, copy this spell except for its epic ability.)",
};

pub const ENTS_FURY: Printed = Printed {
    name: "Ent's Fury",
    cost: "{1}{G}",
    type_line: "Sorcery",
    pt: None,
    keywords: &["Fight"],
    oracle: "Put a +1/+1 counter on target creature you control if its power is 4 or greater. Then that creature gets +1/+1 until end of turn and fights target creature you don't control.",
};

pub const ESSENCE_SCATTER: Printed = Printed {
    name: "Essence Scatter",
    cost: "{1}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle: "Counter target creature spell.",
};

pub const EXTINGUISH_THE_LIGHT: Printed = Printed {
    name: "Extinguish the Light",
    cost: "{2}{B}{B}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle:
        "Destroy target creature or planeswalker. If its mana value was 3 or less, you gain 3 life.",
};

pub const FADING_HOPE: Printed = Printed {
    name: "Fading Hope",
    cost: "{U}",
    type_line: "Instant",
    pt: None,
    keywords: &["Scry"],
    oracle: "Return target creature to its owner's hand. If its mana value was 3 or less, scry 1. (Look at the top card of your library. You may put that card on the bottom.)",
};

pub const FATAL_PUSH: Printed = Printed {
    name: "Fatal Push",
    cost: "{B}",
    type_line: "Instant",
    pt: None,
    keywords: &["Revolt"],
    oracle: "Destroy target creature if it has mana value 2 or less.\nRevolt — Destroy that creature if it has mana value 4 or less instead if a permanent left the battlefield under your control this turn.",
};

pub const GLORIOUS_GALE: Printed = Printed {
    name: "Glorious Gale",
    cost: "{1}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle: "Counter target creature spell. If it was a legendary spell, the Ring tempts you.",
};

pub const HARMONIZE: Printed = Printed {
    name: "Harmonize",
    cost: "{2}{G}{G}",
    type_line: "Sorcery",
    pt: None,
    keywords: &[],
    oracle: "Draw three cards.",
};

pub const HERALD_OF_ILHARG: Printed = Printed {
    name: "Herald of Ilharg",
    cost: "{2}{R}{G}",
    type_line: "Creature — Boar Beast",
    pt: Some((3, 3)),
    keywords: &["Trample"],
    oracle: "Trample\nWhenever you cast a creature spell, put two +1/+1 counters on this creature. If that spell has mana value 5 or greater, this creature deals damage equal to the number of counters on it to each opponent.",
};

pub const LIGHTNING_BOLT: Printed = Printed {
    name: "Lightning Bolt",
    cost: "{R}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle: "Lightning Bolt deals 3 damage to any target.",
};

pub const MOLTEN_RAIN: Printed = Printed {
    name: "Molten Rain",
    cost: "{1}{R}{R}",
    type_line: "Sorcery",
    pt: None,
    keywords: &[],
    oracle: "Destroy target land. If that land was nonbasic, Molten Rain deals 2 damage to the land's controller.",
};

pub const OVERRUN: Printed = Printed {
    name: "Overrun",
    cost: "{2}{G}{G}{G}",
    type_line: "Sorcery",
    pt: None,
    keywords: &[],
    oracle: "Creatures you control get +3/+3 and gain trample until end of turn. (Each of those creatures can deal excess combat damage to the player or planeswalker it's attacking.)",
};

pub const PERILOUS_VOYAGE: Printed = Printed {
    name: "Perilous Voyage",
    cost: "{1}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &["Scry"],
    oracle: "Return target nonland permanent you don't control to its owner's hand. If its mana value was 2 or less, scry 2.",
};

pub const PRISMARI_APPRENTICE: Printed = Printed {
    name: "Prismari Apprentice",
    cost: "{U}{R}",
    type_line: "Creature — Human Shaman",
    pt: Some((2, 2)),
    keywords: &["Magecraft"],
    oracle: "Magecraft — Whenever you cast or copy an instant or sorcery spell, this creature can't be blocked this turn. If that spell has mana value 5 or greater, put a +1/+1 counter on this creature.",
};

pub const RAZE_TO_THE_GROUND: Printed = Printed {
    name: "Raze to the Ground",
    cost: "{2}{R}",
    type_line: "Sorcery",
    pt: None,
    keywords: &[],
    oracle: "This spell can't be countered.\nDestroy target artifact. If its mana value was 1 or less, draw a card.",
};

pub const REJECT_IMPERFECTION: Printed = Printed {
    name: "Reject Imperfection",
    cost: "{1}{U}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &["Proliferate"],
    oracle: "Counter target spell. If that spell's mana value was 3 or less, proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)",
};

pub const REMAND: Printed = Printed {
    name: "Remand",
    cost: "{1}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle: "Counter target spell. If that spell is countered this way, put it into its owner's hand instead of into that player's graveyard.\nDraw a card.",
};

pub const SAVAGE_SWIPE: Printed = Printed {
    name: "Savage Swipe",
    cost: "{G}",
    type_line: "Sorcery",
    pt: None,
    keywords: &["Fight"],
    oracle: "Target creature you control gets +2/+2 until end of turn if its power is 2. Then it fights target creature you don't control. (Each deals damage equal to its power to the other.)",
};

pub const SEEDSHIP_IMPACT: Printed = Printed {
    name: "Seedship Impact",
    cost: "{1}{G}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle: "Destroy target artifact or enchantment. If its mana value was 2 or less, create a Lander token. (It's an artifact with \"{2}, {T}, Sacrifice this token: Search your library for a basic land card, put it onto the battlefield tapped, then shuffle.\")",
};

pub const SERRA_ANGEL: Printed = Printed {
    name: "Serra Angel",
    cost: "{3}{W}{W}",
    type_line: "Creature — Angel",
    pt: Some((4, 4)),
    keywords: &["Flying", "Vigilance"],
    oracle: "Flying\nVigilance (Attacking doesn't cause this creature to tap.)",
};

pub const SERUM_SNARE: Printed = Printed {
    name: "Serum Snare",
    cost: "{1}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &["Proliferate"],
    oracle: "Return target nonland permanent to its owner's hand. If that permanent had mana value 3 or less, proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)",
};

pub const SOUND_THE_TRUMPETS: Printed = Printed {
    name: "Sound the Trumpets",
    cost: "{1}{U}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &["Recruit"],
    oracle: "Counter target spell. If that spell's mana value was 2 or less, recruit. (Draw a card, then discard a card. If you discarded a nonland card, create a 1/1 white Human Soldier creature token.)",
};

pub const STATUTE_OF_DENIAL: Printed = Printed {
    name: "Statute of Denial",
    cost: "{2}{U}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle:
        "Counter target spell. If you control a blue creature, draw a card, then discard a card.",
};

pub const STROKE_OF_GENIUS: Printed = Printed {
    name: "Stroke of Genius",
    cost: "{X}{2}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle: "Target player draws X cards.",
};

pub const TAINTED_TREATS: Printed = Printed {
    name: "Tainted Treats",
    cost: "{1}{B}{G}",
    type_line: "Instant",
    pt: None,
    keywords: &["Food"],
    oracle: "Destroy target artifact or creature. If its mana value was 4 or less, create a Food token. (It's an artifact with \"{2}, {T}, Sacrifice this token: You gain 3 life.\")",
};

pub const TRIBUTE_TO_THE_WORLD_TREE: Printed = Printed {
    name: "Tribute to the World Tree",
    cost: "{G}{G}{G}",
    type_line: "Enchantment",
    pt: None,
    keywords: &[],
    oracle: "Whenever a creature you control enters, draw a card if its power is 3 or greater. Otherwise, put two +1/+1 counters on it.",
};

pub const TWINCAST: Printed = Printed {
    name: "Twincast",
    cost: "{U}{U}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle: "Copy target instant or sorcery spell. You may choose new targets for the copy.",
};

pub const VINDICTIVE_TRIUMPH: Printed = Printed {
    name: "Vindictive Triumph",
    cost: "{W}{B}{B}",
    type_line: "Instant",
    pt: None,
    keywords: &[],
    oracle: "Exile target creature or planeswalker. If that permanent's mana value was 3 or less, return it to the battlefield tapped under your control. Exile it at the beginning of the next end step.",
};

pub const WALKING_BALLISTA: Printed = Printed {
    name: "Walking Ballista",
    cost: "{X}{X}",
    type_line: "Artifact Creature — Construct",
    pt: Some((0, 0)),
    keywords: &[],
    oracle: "This creature enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on this creature.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.",
};

pub const WELCOME_TO_THE_FOLD: Printed = Printed {
    name: "Welcome to the Fold",
    cost: "{2}{U}{U}",
    type_line: "Sorcery",
    pt: None,
    keywords: &["Madness"],
    oracle: "Madness {X}{U}{U} (If you discard this card, discard it into exile. When you do, cast it for its madness cost or put it into your graveyard.)\nGain control of target creature if its toughness is 2 or less. If this spell's madness cost was paid, instead gain control of that creature if its toughness is X or less.",
};

/// Every card above, for the table-wide checks.
pub const ALL: &[&Printed] = &[
    &ANGELIC_WALL,
    &ANOINT_WITH_AFFLICTION,
    &BRACKISH_BLUNDER,
    &BURNING_HANDS,
    &CARNIVOROUS_CANOPY,
    &CHOKING_SANDS,
    &COUNTERSPELL,
    &DARKSTEEL_INGOT,
    &DEEKAH_FRACTAL_THEORIST,
    &ENDLESS_SWARM,
    &ENTS_FURY,
    &ESSENCE_SCATTER,
    &EXTINGUISH_THE_LIGHT,
    &FADING_HOPE,
    &FATAL_PUSH,
    &GLORIOUS_GALE,
    &HARMONIZE,
    &HERALD_OF_ILHARG,
    &LIGHTNING_BOLT,
    &MOLTEN_RAIN,
    &OVERRUN,
    &PERILOUS_VOYAGE,
    &PRISMARI_APPRENTICE,
    &RAZE_TO_THE_GROUND,
    &REJECT_IMPERFECTION,
    &REMAND,
    &SAVAGE_SWIPE,
    &SEEDSHIP_IMPACT,
    &SERRA_ANGEL,
    &SERUM_SNARE,
    &SOUND_THE_TRUMPETS,
    &STATUTE_OF_DENIAL,
    &STROKE_OF_GENIUS,
    &TAINTED_TREATS,
    &TRIBUTE_TO_THE_WORLD_TREE,
    &TWINCAST,
    &VINDICTIVE_TRIUMPH,
    &WALKING_BALLISTA,
    &WELCOME_TO_THE_FOLD,
];
