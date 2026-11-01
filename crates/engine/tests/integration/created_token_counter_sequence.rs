//! Imperative created-token counter placement, end to end.
//!
//! The class: every imperative "create <token>[,] then/and put[s] <count>
//! <+1/+1|-1/-1> counter(s) on it" sentence (Zaxara, Fractal Summoning,
//! G'raha Tia, Oversimplify, Zimone, …).
//!
//! - CR 608.2c: the controller follows the instructions in the order written,
//!   so the token is created first and the counters are put on it by a later
//!   instruction. "It" is the token (or tokens) that create instruction made.
//! - CR 122.1: those counters are placed on a permanent already on the
//!   battlefield; the token does not "enter with" them.
//!
//! The class therefore lowers to `Effect::Token` followed by a
//! `ContinuationStep` `Effect::PutCounter { target: LastCreated }` sub directly
//! under it. Only the declarative "The token enters with …" form (CR 614.1c +
//! CR 122.6a, Printlifter Ooze) keeps `Token.enter_with_counters`.

use engine::game::coverage::card_face_gaps;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ChooseOneOfKind, ConjureCard, ConjureSource, Effect,
    PlayerFilter, PtValue, QuantityExpr, ResolvedAbility, SubAbilityLink, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// ─── V2b: the class parse table over verbatim Oracle ─────────────────────────

/// One card of the imperative class (or the declarative negative), with its
/// type line and verbatim MTGJSON Oracle text.
struct ClassRow {
    name: &'static str,
    keywords: &'static [&'static str],
    types: &'static [&'static str],
    subtypes: &'static [&'static str],
    oracle: &'static str,
    /// The number of class `Token` definitions the face carries.
    tokens: usize,
    /// Coverage gaps the face has for reasons unrelated to the class.
    preexisting_gaps: &'static [&'static str],
}

/// The 27-card imperative class, Oracle text copied verbatim from MTGJSON
/// `AtomicCards.json`.
const CLASS: &[ClassRow] = &[
    ClassRow {
        name: "Additive Evolution",
        keywords: &[],
        types: &["Enchantment"],
        subtypes: &[],
        oracle: "When this enchantment enters, create a 0/0 green and blue Fractal creature token. Put three +1/+1 counters on it.\nAt the beginning of combat on your turn, put a +1/+1 counter on target creature you control. It gains vigilance until end of turn.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Berta, Wise Extrapolator",
        keywords: &["Increment"],
        types: &["Creature"],
        subtypes: &["Frog", "Druid"],
        oracle: "Increment (Whenever you cast a spell, if the amount of mana you spent is greater than this creature's power or toughness, put a +1/+1 counter on this creature.)\nWhenever one or more +1/+1 counters are put on Berta, add one mana of any color.\n{X}, {T}: Create a 0/0 green and blue Fractal creature token and put X +1/+1 counters on it.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Body of Research",
        keywords: &[],
        types: &["Sorcery"],
        subtypes: &[],
        oracle: "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is the number of cards in your library.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Deekah, Fractal Theorist",
        keywords: &["Magecraft"],
        types: &["Creature"],
        subtypes: &["Human", "Wizard"],
        oracle: "Magecraft — Whenever you cast or copy an instant or sorcery spell, create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is that spell's mana value.\n{3}{U}: Target creature token can't be blocked this turn.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Emil, Vastlands Roamer",
        keywords: &[],
        types: &["Creature"],
        subtypes: &["Elf", "Druid"],
        oracle: "Creatures you control with +1/+1 counters on them have trample.\n{4}{G}, {T}: Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is the number of differently named lands you control.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Fractal Anomaly",
        keywords: &[],
        types: &["Instant"],
        subtypes: &[],
        oracle: "Create a 0/0 green and blue Fractal creature token and put X +1/+1 counters on it, where X is the number of cards you've drawn this turn.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Fractal Summoning",
        keywords: &[],
        types: &["Sorcery"],
        subtypes: &["Lesson"],
        oracle: "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Fractal Tender",
        keywords: &["Increment", "Ward"],
        types: &["Creature"],
        subtypes: &["Elf", "Wizard"],
        oracle: "Ward {2}\nIncrement (Whenever you cast a spell, if the amount of mana you spent is greater than this creature's power or toughness, put a +1/+1 counter on this creature.)\nAt the beginning of each end step, if you put a counter on this creature this turn, create a 0/0 green and blue Fractal creature token and put three +1/+1 counters on it.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "G'raha Tia, Scion Reborn",
        keywords: &["Lifelink", "Throw Wide the Gates"],
        types: &["Creature"],
        subtypes: &["Cat", "Wizard"],
        oracle: GRAHA,
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Geometric Nexus",
        keywords: &[],
        types: &["Artifact"],
        subtypes: &[],
        oracle: "Whenever a player casts an instant or sorcery spell, put a number of charge counters on this artifact equal to that spell's mana value.\n{6}, {T}, Remove all charge counters from this artifact: Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is the number of charge counters removed this way.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Gimbal, Gremlin Prodigy",
        keywords: &[],
        types: &["Creature"],
        subtypes: &["Gremlin", "Artificer"],
        oracle: "Artifact creatures you control have trample.\nAt the beginning of your end step, create a 0/0 red Gremlin artifact creature token. Put X +1/+1 counters on it, where X is the number of differently named artifact tokens you control.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Hiveheart Shaman",
        keywords: &[],
        types: &["Creature"],
        subtypes: &["Human", "Shaman"],
        oracle: "Whenever this creature attacks, you may search your library for a basic land card that doesn't share a land type with a land you control, put that card onto the battlefield, then shuffle.\n{5}{G}: Create a 1/1 green Insect creature token. Put X +1/+1 counters on it, where X is the number of basic land types among lands you control. Activate only as a sorcery.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Kasmina, Enigma Sage",
        keywords: &["Scry"],
        types: &["Planeswalker"],
        subtypes: &["Kasmina"],
        oracle: "Each other planeswalker you control has the loyalty abilities of Kasmina.\n[+2]: Scry 1.\n[\u{2212}X]: Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it.\n[\u{2212}8]: Search your library for an instant or sorcery card that shares a color with this planeswalker, exile that card, then shuffle. You may cast that card without paying its mana cost.",
        tokens: 1,
        // The loyalty-ability-sharing static is an unrelated, pre-existing gap.
        preexisting_gaps: &["Effect:static_structure"],
    },
    ClassRow {
        name: "Lattice Library",
        keywords: &[],
        types: &["Enchantment"],
        subtypes: &[],
        oracle: "This enchantment enters with X study counters on it.\nWhen this enchantment enters and whenever you cast your first spell with {X} in its mana cost each turn, create a 0/0 green and blue Fractal creature token. Put a number of +1/+1 counters on it equal to the number of study counters on this enchantment.",
        tokens: 2,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Leyline Invocation",
        keywords: &[],
        types: &["Sorcery"],
        subtypes: &[],
        oracle: "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is the number of lands you control.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Manifestation Sage",
        keywords: &[],
        types: &["Creature"],
        subtypes: &["Human", "Wizard"],
        oracle: "When this creature enters, create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is the number of cards in your hand.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Oversimplify",
        keywords: &[],
        types: &["Sorcery"],
        subtypes: &[],
        oracle: "Exile all creatures. Each player creates a 0/0 green and blue Fractal creature token and puts a number of +1/+1 counters on it equal to the total power of creatures they controlled that were exiled this way.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Paradox Zone",
        keywords: &["Double"],
        types: &["Enchantment"],
        subtypes: &[],
        oracle: "This enchantment enters with a growth counter on it.\nAt the beginning of your end step, double the number of growth counters on this enchantment. Then create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is the number of growth counters on this enchantment.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Primo, the Unbounded",
        keywords: &["Trample"],
        types: &["Creature"],
        subtypes: &["Fractal", "Wolf"],
        oracle: "Trample\nPrimo enters with twice X +1/+1 counters on it.\nWhenever one or more creatures you control with base power 0 deal combat damage to a player, create a 0/0 green and blue Fractal creature token. Put a number of +1/+1 counters on it equal to the damage dealt.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Recon Craft Theta",
        keywords: &["Crew", "Flying", "Proliferate"],
        types: &["Artifact"],
        subtypes: &["Vehicle"],
        oracle: "Flying\nWhen this Vehicle enters, create a 0/0 blue Alien creature token. Put a +1/+1 counter on it.\nWhenever this Vehicle attacks, proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)\nCrew 2",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Rot Like the Scum You Are",
        keywords: &[],
        types: &["Scheme"],
        subtypes: &[],
        oracle: "When you set this scheme in motion, create a 2/2 green Ooze creature token, then put a number of +1/+1 counters on it equal to the number of lands your opponents control.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Sequence Engine",
        keywords: &[],
        types: &["Artifact"],
        subtypes: &[],
        oracle: "{X}, {T}: Exile target creature card with mana value X from a graveyard. Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it. Activate only as a sorcery.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Serpentine Curve",
        keywords: &[],
        types: &["Sorcery"],
        subtypes: &[],
        oracle: "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it, where X is one plus the total number of instant and sorcery cards you own in exile and in your graveyard.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Slime Against Humanity",
        keywords: &[],
        types: &["Sorcery"],
        subtypes: &[],
        oracle: "Create a 0/0 green Ooze creature token with trample. Put X +1/+1 counters on it, where X is two plus the total number of cards you own in exile and in your graveyard that are Oozes or are named Slime Against Humanity.\nA deck can have any number of cards named Slime Against Humanity.",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Wild Hypothesis",
        keywords: &["Surveil"],
        types: &["Sorcery"],
        subtypes: &[],
        oracle: "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it.\nSurveil 2. (Look at the top two cards of your library, then put any number of them into your graveyard and the rest on top of your library in any order.)",
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Zaxara, the Exemplary",
        keywords: &["Deathtouch"],
        types: &["Creature"],
        subtypes: &["Nightmare", "Hydra"],
        oracle: ZAXARA,
        tokens: 1,
        preexisting_gaps: &[],
    },
    ClassRow {
        name: "Zimone, All-Questioning",
        keywords: &[],
        types: &["Creature"],
        subtypes: &["Human", "Wizard"],
        oracle: "At the beginning of your end step, if a land entered the battlefield under your control this turn and you control a prime number of lands, create Primo, the Indivisible, a legendary 0/0 green and blue Fractal creature token, then put that many +1/+1 counters on it. (2, 3, 5, 7, 11, 13, 17, 19, 23, 29, and 31 are prime numbers.)",
        tokens: 1,
        preexisting_gaps: &[],
    },
];

/// The declarative negative: "The token enters with …" keeps entry counters.
const PRINTLIFTER_OOZE: ClassRow = ClassRow {
    name: "Printlifter Ooze",
    keywords: &["Deathtouch", "Disguise"],
    types: &["Creature"],
    subtypes: &["Ooze"],
    oracle: "Deathtouch\nWhenever this creature or another creature you control is turned face up, create a 0/0 green Ooze creature token with trample. The token enters with X +1/+1 counters on it, where X is the number of other creatures you control.\nDisguise {3}{G}",
    tokens: 1,
    preexisting_gaps: &[],
};

fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

fn parsed_face(row: &ClassRow) -> CardFace {
    let parsed = parse_oracle_text(
        row.oracle,
        row.name,
        &owned(row.keywords),
        &owned(row.types),
        &owned(row.subtypes),
    );
    CardFace {
        name: row.name.to_string(),
        card_type: CardType {
            supertypes: vec![],
            core_types: row
                .types
                .iter()
                .map(|t| t.parse::<CoreType>().expect("core type"))
                .collect(),
            subtypes: owned(row.subtypes),
        },
        oracle_text: Some(row.oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

/// How one `Effect::Token` definition carries its counters.
#[derive(Debug, PartialEq)]
enum TokenShape {
    /// No entry counters, and a direct `ContinuationStep` / `Spell`
    /// `PutCounter { target: LastCreated }` sub (the imperative class).
    Placement,
    /// Entry counters and no such sub (the declarative form).
    EntryCounters,
    /// Anything else, with its rendering for the failure message.
    Other(String),
}

fn is_last_created_placement(def: &AbilityDefinition) -> bool {
    matches!(
        &*def.effect,
        Effect::PutCounter {
            target: TargetFilter::LastCreated,
            ..
        }
    )
}

fn token_shape(def: &AbilityDefinition) -> Option<TokenShape> {
    let Effect::Token {
        enter_with_counters,
        ..
    } = &*def.effect
    else {
        return None;
    };
    let direct_placement = def.sub_ability.as_deref().filter(|sub| {
        is_last_created_placement(sub)
            && sub.sub_link == SubAbilityLink::ContinuationStep
            && sub.kind == AbilityKind::Spell
    });
    Some(match (enter_with_counters.is_empty(), direct_placement) {
        (true, Some(_)) => TokenShape::Placement,
        (false, None) => TokenShape::EntryCounters,
        _ => TokenShape::Other(format!("{def:?}")),
    })
}

/// Every `Token` definition in `def` (sub, else and mode branches), and the
/// number of `PutCounter { target: LastCreated }` definitions anywhere in it.
fn walk(def: &AbilityDefinition, shapes: &mut Vec<TokenShape>, placements: &mut usize) {
    shapes.extend(token_shape(def));
    if is_last_created_placement(def) {
        *placements += 1;
    }
    for branch in [def.sub_ability.as_deref(), def.else_ability.as_deref()]
        .into_iter()
        .flatten()
        .chain(def.mode_abilities.iter())
    {
        walk(branch, shapes, placements);
    }
}

fn face_shapes(face: &CardFace) -> (Vec<TokenShape>, usize) {
    let mut shapes = Vec::new();
    let mut placements = 0;
    let executes = face.triggers.iter().filter_map(|t| t.execute.as_deref());
    for def in face.abilities.iter().chain(executes) {
        walk(def, &mut shapes, &mut placements);
    }
    (shapes, placements)
}

/// SHAPE (V2b): every class member, parsed from its verbatim Oracle text, lowers
/// each of its token definitions to `Token` with no entry counters followed
/// directly by a `ContinuationStep` `PutCounter { LastCreated }` (CR 608.2c +
/// CR 122.1), with no other `LastCreated` placement anywhere. Printlifter Ooze's
/// declarative "enters with" keeps its entry counters (CR 614.1c + CR 122.6a).
#[test]
fn class_members_lower_to_token_then_last_created_placement() {
    assert_eq!(CLASS.len(), 27, "the planned class");
    for row in CLASS {
        let face = parsed_face(row);
        assert_eq!(
            card_face_gaps(&face),
            row.preexisting_gaps
                .iter()
                .map(|gap| gap.to_string())
                .collect::<Vec<_>>(),
            "{}: coverage gaps",
            row.name
        );
        let (shapes, placements) = face_shapes(&face);
        // Reach-guard: the class token definitions were visited.
        assert_eq!(
            shapes,
            (0..row.tokens)
                .map(|_| TokenShape::Placement)
                .collect::<Vec<_>>(),
            "{}: every token must be followed directly by its counter placement",
            row.name
        );
        assert_eq!(
            placements, row.tokens,
            "{}: one LastCreated placement per token, none elsewhere",
            row.name
        );
    }

    let face = parsed_face(&PRINTLIFTER_OOZE);
    assert_eq!(card_face_gaps(&face), Vec::<String>::new());
    let (shapes, placements) = face_shapes(&face);
    assert_eq!(shapes, vec![TokenShape::EntryCounters]);
    assert_eq!(placements, 0, "the declarative form has no placement step");
}

// ─── Runtime rows ────────────────────────────────────────────────────────────

const GRAHA: &str = "Lifelink\nThrow Wide the Gates \u{2014} Whenever you cast a noncreature spell, you may pay X life, where X is that spell's mana value. If you do, create a 1/1 colorless Hero creature token and put X +1/+1 counters on it. Do this only once each turn.";
const ZAXARA: &str = "Deathtouch\n{T}: Add two mana of any one color.\nWhenever you cast a spell with {X} in its mana cost, create a 0/0 green Hydra creature token, then put X +1/+1 counters on it.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const FRACTAL_SUMMONING: &str =
    "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it.";
const BERTA: &str = "Increment (Whenever you cast a spell, if the amount of mana you spent is greater than this creature's power or toughness, put a +1/+1 counter on this creature.)\nWhenever one or more +1/+1 counters are put on Berta, add one mana of any color.\n{X}, {T}: Create a 0/0 green and blue Fractal creature token and put X +1/+1 counters on it.";

fn pool(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .map(|&kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn battlefield_named(state: &engine::types::game_state::GameState, name: &str) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].name == name)
        .collect()
}

/// V8 fixture: G'raha Tia, a stale Hero token already in the `LastCreated`
/// ledger, P0 at `life`, and a two-mana "Draw a card." in hand. Returns, after
/// the cast resolves with the optional payment `accepted` or declined, each
/// Hero as `(is_the_stale_hero, +1/+1 counters)` and P0's final life.
fn graha_trigger(life: i32, accepted: bool) -> (Vec<(bool, u32)>, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "G'raha Tia, Scion Reborn", 2, 3, GRAHA);
    let stale = scenario.add_creature(P0, "Hero", 1, 1).id();
    scenario.with_life(P0, life);
    scenario.with_mana_pool(P0, pool(&[ManaType::Blue, ManaType::Blue]));
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&stale).unwrap().is_token = true;
    // An earlier resolution's token, still in the game-lifetime ledger.
    runner.state_mut().last_created_token_ids = vec![stale];

    let cast = runner.cast(spell);
    let outcome = if accepted {
        cast.accept_optional().resolve()
    } else {
        cast.decline_optional().resolve()
    };
    let state = outcome.state();
    let heroes = battlefield_named(state, "Hero")
        .into_iter()
        .map(|hero| {
            (
                hero == stale,
                outcome.counters(hero, CounterType::Plus1Plus1),
            )
        })
        .collect();
    (heroes, state.players[0].life)
}

/// V8 (CR 608.2c + CR 119.4): G'raha Tia's placement depends on its creating
/// instruction. When the life can't be paid, no Hero is created, "it" refers to
/// nothing, and the stale Hero still in the `LastCreated` ledger gets nothing.
#[test]
fn graha_tia_unpaid_life_skips_the_placement_and_never_reads_a_stale_token() {
    // (a) Reach-guard: paid, the new Hero receives X = 2 and the stale one 0.
    let (heroes, life) = graha_trigger(20, true);
    assert_eq!(heroes, vec![(true, 0), (false, 2)]);
    assert_eq!(life, 18, "paid X = 2 life");

    // (b) CR 119.4: life 1 cannot pay 2, so the "if you do" gate is false: no
    // Hero is created and the placement is skipped, not applied to the stale Hero.
    let (heroes, life) = graha_trigger(1, true);
    assert_eq!(heroes, vec![(true, 0)]);
    assert_eq!(life, 1, "the payment did not happen");

    // (c) Declined: nothing happens.
    let (heroes, life) = graha_trigger(20, false);
    assert_eq!(heroes, vec![(true, 0)]);
    assert_eq!(life, 20);
}

/// Zaxara on the battlefield (± Parallel Lives) and P0 casting "You gain X
/// life." for {X}{G} with X = `x`. Returns each Hydra's +1/+1 counters, P0's
/// life delta and the resolution events.
fn zaxara_trigger(parallel_lives: bool, x: u32) -> (Vec<u32>, i32, Vec<GameEvent>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Zaxara, the Exemplary", 2, 3, ZAXARA);
    if parallel_lives {
        scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    }
    scenario.with_mana_pool(P0, pool(&[ManaType::Green; 3]));
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Gain", false, "You gain X life.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).x(x).resolve();
    let hydras = battlefield_named(outcome.state(), "Hydra");
    let counters = hydras
        .iter()
        .map(|hydra| outcome.counters(*hydra, CounterType::Plus1Plus1))
        .collect();
    (
        counters,
        outcome.life_delta(P0),
        outcome.events().to_vec(),
        hydras,
    )
}

/// V14 (Zaxara ruling 2; CR 614.1a for Parallel Lives): when the create
/// instruction makes two Hydras, the placement puts X counters on each of them,
/// and each enters before any counter is put on it (CR 608.2c).
#[test]
fn zaxara_with_parallel_lives_puts_x_counters_on_each_hydra() {
    let (counters, life, events, hydras) = zaxara_trigger(true, 2);
    // Reach-guard: X = 2 (the life gained) and two Hydras were created.
    assert_eq!(life, 2);
    assert_eq!(counters, vec![2, 2]);
    let last_entry = events
        .iter()
        .rposition(|e| {
            matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. }
                if hydras.contains(object_id))
        })
        .expect("the Hydras enter");
    let first_counter = events
        .iter()
        .position(|e| {
            matches!(e, GameEvent::CounterAdded { object_id, .. } if hydras.contains(object_id))
        })
        .expect("the Hydras receive counters");
    assert!(
        last_entry < first_counter,
        "both Hydras enter before either receives a counter"
    );

    // Sibling: without Parallel Lives, one Hydra with X counters.
    let (counters, life, _, _) = zaxara_trigger(false, 2);
    assert_eq!(life, 2);
    assert_eq!(counters, vec![2]);
}

/// V19 (CR 107.3 + CR 608.2h): the moved placement keeps its X binding.
/// Fractal Summoning cast with X = 3 makes one Fractal with three counters;
/// Zaxara's trigger from an X = 2 spell makes one Hydra with two. Geometric
/// Nexus's cost-removed count is not asserted here (follow-up F-GN).
#[test]
fn moved_placement_keeps_its_x_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(&[ManaType::Green; 5]));
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Fractal Summoning", false, FRACTAL_SUMMONING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::X,
                ManaCostShard::GreenBlue,
                ManaCostShard::GreenBlue,
            ],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).x(3).resolve();
    let fractals = battlefield_named(outcome.state(), "Fractal");
    assert_eq!(fractals.len(), 1, "exactly one Fractal");
    assert_eq!(outcome.counters(fractals[0], CounterType::Plus1Plus1), 3);

    let (counters, life, _, _) = zaxara_trigger(false, 2);
    assert_eq!(life, 2, "reach-guard: X = 2");
    assert_eq!(counters, vec![2]);
}

/// V19 (activated X; CR 602.2b + CR 107.3): Berta's "{X}, {T}: Create … and put
/// X +1/+1 counters on it" activated with X = 2 makes one Fractal with two.
#[test]
fn activated_x_reaches_the_moved_placement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let berta = scenario
        .add_creature_from_oracle(P0, "Berta, Wise Extrapolator", 1, 4, BERTA)
        .id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless; 2]));
    let mut runner = scenario.build();
    let index = runner.state().objects[&berta]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .expect("Berta's activated ability");
    let outcome = runner.activate(berta, index).x(2).resolve();
    let fractals = battlefield_named(outcome.state(), "Fractal");
    assert_eq!(fractals.len(), 1, "exactly one Fractal");
    assert_eq!(outcome.counters(fractals[0], CounterType::Plus1Plus1), 2);
}

// ─── D1.6: Wild Hypothesis (create, put counters on it, then surveil) ────────

const WILD_HYPOTHESIS: &str = "Create a 0/0 green and blue Fractal creature token. Put X +1/+1 counters on it.\nSurveil 2. (Look at the top two cards of your library, then put any number of them into your graveyard and the rest on top of your library in any order.)";

/// Wild Hypothesis ({X}{G}) cast with `x` and resolved up to its surveil
/// prompt. The library holds three cards.
fn wild_hypothesis_at_surveil(x: u32) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(&[ManaType::Green; 3]));
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Wild Hypothesis", false, WILD_HYPOTHESIS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let mut runner = scenario.build();
    let _ = runner.cast(spell).x(x).commit();
    runner.resolve_top();
    // Reach-guard: resolution stopped at the surveil prompt over two cards.
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::SurveilChoice { cards, .. } if cards.len() == 2
        ),
        "expected a two-card surveil prompt, got {:?}",
        runner.state().waiting_for
    );
    runner
}

/// D1.6 (CR 608.2c + CR 122.1 + CR 701.25a): the Fractal is created and gets
/// its X counters before the surveil instruction; Surveil is an independent
/// later instruction and still resolves. Both looked-at cards go to the
/// graveyard, then the sorcery does.
#[test]
fn wild_hypothesis_counters_land_before_surveil_and_surveil_resolves() {
    let mut runner = wild_hypothesis_at_surveil(2);
    let fractals = battlefield_named(runner.state(), "Fractal");
    assert_eq!(fractals.len(), 1, "one Fractal at the surveil prompt");
    let fractal = fractals[0];
    assert_eq!(plus_one_counters(runner.state(), fractal), 2);

    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("surveil choice accepted");
    let state = runner.state();
    assert_eq!(battlefield_named(state, "Fractal"), vec![fractal]);
    assert_eq!(plus_one_counters(state, fractal), 2);
    assert_eq!(state.objects[&fractal].power, Some(2));
    assert_eq!(state.objects[&fractal].toughness, Some(2));
    assert_eq!(
        state.players[0].graveyard.len(),
        3,
        "two surveilled cards and the sorcery"
    );
    assert_eq!(state.players[0].library.len(), 1);
}

/// D1.6 sibling (X = 0): the surveil is independent of the placement and of
/// the token's survival. The 0/0 Fractal dies to CR 704.5f at the next CR 704.3
/// check and ceases to exist (CR 111.7); the surveil still resolves.
#[test]
fn wild_hypothesis_surveil_resolves_when_x_is_zero() {
    let mut runner = wild_hypothesis_at_surveil(0);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("surveil choice accepted");
    let state = runner.state();
    assert!(battlefield_named(state, "Fractal").is_empty());
    assert_eq!(state.players[0].graveyard.len(), 3);
    assert_eq!(state.players[0].library.len(), 1);
}

// ─── D1.1 premise: the nearest publisher's product is the referent ───────────

fn plus_one_counters(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn creature_token(name: &str, size: i32) -> Effect {
    Effect::Token {
        attack_destination: engine::types::ability::TokenAttackDestination::ControllerChooses,
        name: name.to_string(),
        power: PtValue::Fixed(size),
        toughness: PtValue::Fixed(size),
        types: vec!["Creature".to_string(), name.to_string()],
        colors: vec![ManaColor::White],
        keywords: vec![],
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

fn plus_one_on_last_created() -> Effect {
    Effect::PutCounter {
        counter_type: CounterType::Plus1Plus1,
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::LastCreated,
    }
}

/// `effects` as one chain controlled by P0 from `source`, in order.
fn chain(source: ObjectId, effects: Vec<Effect>) -> ResolvedAbility {
    effects
        .into_iter()
        .rev()
        .fold(None, |next: Option<ResolvedAbility>, effect| {
            let node = ResolvedAbility::new(effect, vec![], source, P0);
            Some(match next {
                Some(next) => node.sub_ability(next),
                None => node,
            })
        })
        .expect("a non-empty chain")
}

/// A board with one P0 creature as the chain's source, and the runner.
fn chain_board() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Chain Source", 1, 1).id();
    (scenario.build(), source)
}

/// Resolves `ability` on the runner's board.
fn resolve_chain(runner: &mut GameRunner, ability: &ResolvedAbility) {
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), ability, &mut events, 0).expect("chain resolves");
}

/// The battlefield objects not in `before`, other than the Soldier.
fn new_non_soldiers(state: &GameState, before: &[ObjectId]) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| !before.contains(id) && state.objects[id].name != "Soldier")
        .collect()
}

fn the_soldier(state: &GameState) -> ObjectId {
    let soldiers = battlefield_named(state, "Soldier");
    assert_eq!(soldiers.len(), 1, "the Soldier token exists");
    soldiers[0]
}

/// R1 (CR 608.2c): after `[Token, Conjure, PutCounter { LastCreated }]` the
/// counter goes on the conjured card — the nearest publisher's product — not
/// on the Soldier. Control: `[Token, PutCounter]` puts it on the Soldier.
#[test]
fn a_later_conjure_is_the_placement_referent() {
    let (mut runner, source) = chain_board();
    resolve_chain(
        &mut runner,
        &chain(
            source,
            vec![creature_token("Soldier", 1), plus_one_on_last_created()],
        ),
    );
    let soldier = the_soldier(runner.state());
    assert_eq!(plus_one_counters(runner.state(), soldier), 1);

    let (mut runner, source) = chain_board();
    let before: Vec<ObjectId> = runner.state().battlefield.iter().copied().collect();
    let conjure = Effect::Conjure {
        cards: vec![ConjureCard {
            source: ConjureSource::Named {
                name: "Hangarback Walker".to_string(),
            },
            count: QuantityExpr::Fixed { value: 1 },
        }],
        destination: Zone::Battlefield,
        tapped: false,
        library_position: None,
        library_players: None,
    };
    resolve_chain(
        &mut runner,
        &chain(
            source,
            vec![
                creature_token("Soldier", 1),
                conjure,
                plus_one_on_last_created(),
            ],
        ),
    );
    let state = runner.state();
    let conjured = new_non_soldiers(state, &before);
    assert_eq!(conjured.len(), 1, "the conjured card is on the battlefield");
    assert_eq!(plus_one_counters(state, conjured[0]), 1);
    assert_eq!(plus_one_counters(state, the_soldier(state)), 0);
}

fn spellbook_chain_board(random: bool) -> (GameRunner, ResolvedAbility, Vec<ObjectId>) {
    let (mut runner, source) = chain_board();
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .spellbook = vec!["Lion Sash".to_string()];
    let before = runner.state().battlefield.iter().copied().collect();
    let ability = chain(
        source,
        vec![
            creature_token("Soldier", 1),
            Effect::DraftFromSpellbook {
                destination: Zone::Battlefield,
                tapped: false,
                random,
            },
            plus_one_on_last_created(),
        ],
    );
    (runner, ability, before)
}

fn assert_drafted_card_has_the_counter(state: &GameState, before: &[ObjectId]) {
    let drafted = new_non_soldiers(state, before);
    assert_eq!(drafted.len(), 1, "the drafted card is on the battlefield");
    assert_eq!(state.objects[&drafted[0]].name, "Lion Sash");
    assert_eq!(plus_one_counters(state, drafted[0]), 1);
    assert_eq!(plus_one_counters(state, the_soldier(state)), 0);
}

/// R2 (CR 608.2c; constructed): a spellbook draft publishes the drafted card,
/// so a later `LastCreated` placement goes on it (random path, no pause).
#[test]
fn a_later_random_spellbook_draft_is_the_placement_referent() {
    let (mut runner, ability, before) = spellbook_chain_board(true);
    resolve_chain(&mut runner, &ability);
    assert_drafted_card_has_the_counter(runner.state(), &before);
}

/// R2b (CR 608.2c + CR 608.2d; constructed): the chosen draft pauses with the
/// tail stashed; the drafted card is published at the resume, before the
/// placement runs.
#[test]
fn a_later_chosen_spellbook_draft_is_the_placement_referent() {
    let (mut runner, ability, before) = spellbook_chain_board(false);
    resolve_chain(&mut runner, &ability);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::SpellbookDraft { .. }
        ),
        "expected the draft choice, got {:?}",
        runner.state().waiting_for
    );
    let soldier = the_soldier(runner.state());
    assert_eq!(
        plus_one_counters(runner.state(), soldier),
        0,
        "the placement is stashed behind the draft choice"
    );
    runner
        .act(GameAction::SubmitSpellbookDraft {
            card: "Lion Sash".to_string(),
        })
        .expect("draft accepted");
    assert_drafted_card_has_the_counter(runner.state(), &before);
}

/// Answers the pending `ChooseOneOfBranch` with the branch whose description
/// contains `needle`.
fn choose_branch_describing(runner: &mut GameRunner, needle: &str) {
    let index = match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            branch_descriptions,
            ..
        } => branch_descriptions
            .iter()
            .position(|description| description.contains(needle))
            .unwrap_or_else(|| panic!("no branch describes {needle}: {branch_descriptions:?}")),
        other => panic!("expected a branch choice, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseBranch { index })
        .expect("branch accepted");
}

/// The Spirit token among the new battlefield objects.
fn the_spirit(state: &GameState, before: &[ObjectId]) -> ObjectId {
    let spirits: Vec<ObjectId> = new_non_soldiers(state, before)
        .into_iter()
        .filter(|id| {
            state.objects[id]
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Spirit")
        })
        .collect();
    assert_eq!(spirits.len(), 1, "the Spirit token exists");
    spirits[0]
}

/// R3 (CR 701.63a + CR 608.2c; constructed): endure's token branch creates the
/// N/N Spirit, the nearest created object, so the placement goes on it.
#[test]
fn a_later_endure_token_branch_is_the_placement_referent() {
    let (mut runner, source) = chain_board();
    let before: Vec<ObjectId> = runner.state().battlefield.iter().copied().collect();
    let ability = chain(
        source,
        vec![
            creature_token("Soldier", 1),
            Effect::Endure {
                amount: QuantityExpr::Fixed { value: 2 },
                subject: TargetFilter::SelfRef,
            },
            plus_one_on_last_created(),
        ],
    );
    resolve_chain(&mut runner, &ability);
    choose_branch_describing(&mut runner, "Spirit");
    let state = runner.state();
    let spirit = the_spirit(state, &before);
    assert_eq!(
        state.objects[&spirit].power,
        Some(3),
        "a 2/2 Spirit plus one"
    );
    assert_eq!(plus_one_counters(state, spirit), 1);
    assert_eq!(plus_one_counters(state, the_soldier(state)), 0);
}

/// R4 (CR 608.2c + CR 608.2d; constructed): a choose-one-of's chosen branch is
/// followed inside this resolution, so its token is the placement's referent.
#[test]
fn a_later_choose_one_of_token_branch_is_the_placement_referent() {
    let (mut runner, source) = chain_board();
    let before: Vec<ObjectId> = runner.state().battlefield.iter().copied().collect();
    let mut spirit_branch = AbilityDefinition::new(AbilityKind::Spell, creature_token("Spirit", 1));
    spirit_branch.description = Some("Create a 1/1 Spirit token.".to_string());
    let mut life_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    );
    life_branch.description = Some("You gain 1 life.".to_string());
    let ability = chain(
        source,
        vec![
            creature_token("Soldier", 1),
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![spirit_branch, life_branch],
                kind: ChooseOneOfKind::Ordinary,
            },
            plus_one_on_last_created(),
        ],
    );
    resolve_chain(&mut runner, &ability);
    choose_branch_describing(&mut runner, "Spirit");
    let state = runner.state();
    let spirit = the_spirit(state, &before);
    assert_eq!(plus_one_counters(state, spirit), 1);
    assert_eq!(plus_one_counters(state, the_soldier(state)), 0);
}
