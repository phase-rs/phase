//! CR 608.2c: a leading "For each of those <type>," / "For each of them,"
//! clause iterates its body once per member of the population its antecedent
//! published. The antecedent is found by one walk back over the chain's
//! earlier instructions; this phase admits exactly one producer shape — the
//! reveal-until set kept onto the battlefield (CR 701.20a).
//!
//! `REVEAL_MUSTER` is a SYNTHETIC building-block instrument, not a printed
//! card. Its first two sentences are Dack Fayden, Helping Hand's own producer
//! wording with a fixed count of two; the third is an already-parsed anaphoric
//! grant over "them" (Random Encounter's "They gain haste"); the fourth names
//! the iterated member in its head ("on that creature"). Phase 3 reuses the instrument
//! and its board builder.
//!
//! Derived reading (CR 701.20a, CR 701.20b, CR 110.2a, CR 701.24a, CR 608.2c,
//! CR 611.2a, CR 122.1a, CR 609.3): reveal until two creature cards are
//! revealed; those two enter under the caster's control; the library is
//! shuffled; exactly those two gain haste until end of turn; each of exactly
//! those two gets one +1/+1 counter — never a revealed miss, a creature card
//! beyond the second, or any other permanent. With fewer than two creature
//! cards in the library, there are as many iterations as were found.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ChoiceType, Effect, PlayerChoiceDistinctness, QuantityExpr, QuantityRef,
    RevealUntilDisposition, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// The synthetic A2.2 instrument (see the module doc).
pub(crate) const REVEAL_MUSTER: &str = "Reveal cards from the top of your library until you \
     reveal two creature cards. Put those creature cards onto the battlefield, then shuffle. \
     They gain haste until end of turn. For each of those creatures, put a +1/+1 counter on \
     that creature.";

const REVEAL_MUSTER_NAME: &str = "Reveal Muster";

/// One staged library card for the instrument's board.
#[derive(Clone, Copy)]
pub(crate) enum MusterCard {
    /// A noncreature sorcery card (a reveal miss).
    Sorcery(&'static str),
    /// A land card (a reveal miss).
    Land(&'static str),
    /// A 2/2 creature card (a reveal hit).
    Creature(&'static str),
}

impl MusterCard {
    fn name(self) -> &'static str {
        match self {
            MusterCard::Sorcery(name) | MusterCard::Land(name) | MusterCard::Creature(name) => name,
        }
    }
}

/// The A2.2 library, top to bottom: two misses interleaved with the two kept
/// creature cards, then a matching creature card beyond the count and a final
/// miss.
pub(crate) const MUSTER_LIBRARY: &[MusterCard] = &[
    MusterCard::Sorcery("Miss One"),
    MusterCard::Creature("Creature A"),
    MusterCard::Land("Miss Two"),
    MusterCard::Creature("Creature B"),
    MusterCard::Creature("Creature C"),
    MusterCard::Sorcery("Miss Three"),
];

pub(crate) struct MusterBoard {
    pub(crate) runner: GameRunner,
    pub(crate) spell: ObjectId,
    pub(crate) library: Vec<(&'static str, ObjectId)>,
    pub(crate) bystander_p0: ObjectId,
    pub(crate) bystander_p1: ObjectId,
}

impl MusterBoard {
    pub(crate) fn card(&self, name: &str) -> ObjectId {
        self.library
            .iter()
            .find(|(card, _)| *card == name)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no staged library card named {name}"))
    }
}

/// Four players; P0 holds the {0} sorcery `oracle` and a library staged from
/// `library_top_first`; P0 and P1 each control one bystander creature.
pub(crate) fn muster_board(oracle: &str, library_top_first: &[MusterCard]) -> MusterBoard {
    let mut scenario = GameScenario::new_n_player(4, 2026);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, REVEAL_MUSTER_NAME, false, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut library = Vec::new();
    for card in library_top_first.iter().rev() {
        let id = match card {
            MusterCard::Sorcery(name) => scenario.add_spell_to_library_top(P0, name, false).id(),
            MusterCard::Land(name) => scenario.add_land_to_library_top(P0, name).id(),
            MusterCard::Creature(name) => scenario.add_card_to_library_top(P0, name),
        };
        library.push((card.name(), id));
    }
    library.reverse();
    let bystander_p0 = scenario.add_creature(P0, "Bystander Zero", 1, 1).id();
    let bystander_p1 = scenario.add_creature(P1, "Bystander One", 1, 1).id();
    let mut runner = scenario.build();
    for (card, (_, id)) in library_top_first.iter().zip(library.iter()) {
        if let MusterCard::Creature(_) = card {
            let obj = runner
                .state_mut()
                .objects
                .get_mut(id)
                .expect("staged creature card");
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
    }
    MusterBoard {
        runner,
        spell,
        library,
        bystander_p0,
        bystander_p1,
    }
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn total_p1p1(runner: &GameRunner) -> u32 {
    runner
        .state()
        .objects
        .values()
        .filter_map(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .sum()
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).expect("object exists").zone
}

fn has_haste(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .is_some_and(|obj| obj.has_keyword(&Keyword::Haste))
}

fn in_p0_library(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .library
        .contains(&id)
}

/// The spell's printed abilities as the parser lowered them.
fn lowered(oracle: &str) -> Vec<AbilityDefinition> {
    parse_oracle_text(
        oracle,
        REVEAL_MUSTER_NAME,
        &[],
        &["Sorcery".to_string()],
        &[],
    )
    .abilities
}

/// The chain's nodes in `sub_ability` order.
fn chain_nodes(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut nodes = vec![def];
    let mut cursor = def.sub_ability.as_deref();
    while let Some(node) = cursor {
        nodes.push(node);
        cursor = node.sub_ability.as_deref();
    }
    nodes
}

fn tracked_set_repeat() -> Option<QuantityExpr> {
    Some(QuantityExpr::Ref {
        qty: QuantityRef::TrackedSetSize,
    })
}

fn has_unimplemented(def: &AbilityDefinition) -> bool {
    chain_nodes(def)
        .iter()
        .any(|node| matches!(*node.effect, Effect::Unimplemented { .. }))
}

/// `REVEAL_MUSTER` with its final sentence's head swapped.
fn with_for_each_head(head: &str) -> String {
    REVEAL_MUSTER.replace("For each of those creatures,", head)
}

/// `REVEAL_MUSTER` with the sentence between the shuffle and the for-each
/// clause replaced.
fn with_intervening(sentence: &str) -> String {
    REVEAL_MUSTER.replace("They gain haste until end of turn.", sentence)
}

/// V2.1a's admitted shape: reveal-until kept onto the battlefield, a shuffle,
/// the anaphoric grant, then the body repeated over the tracked set.
fn assert_admitted_shape(oracle: &str) {
    let abilities = lowered(oracle);
    assert_eq!(abilities.len(), 1, "{oracle}");
    let nodes = chain_nodes(&abilities[0]);
    assert_eq!(nodes.len(), 4, "{oracle}: {nodes:?}");
    assert!(
        matches!(
            &*nodes[0].effect,
            Effect::RevealUntil {
                kept_destination: Zone::Battlefield,
                count: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ),
        "{oracle}: {:?}",
        nodes[0].effect
    );
    // Reach-guard: the walk passed real transparent instructions.
    assert!(matches!(&*nodes[1].effect, Effect::Shuffle { .. }));
    assert!(matches!(&*nodes[2].effect, Effect::GenericEffect { .. }));
    assert_eq!(nodes[3].repeat_for, tracked_set_repeat(), "{oracle}");
    assert!(
        matches!(
            &*nodes[3].effect,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "{oracle}: {:?}",
        nodes[3].effect
    );
    assert!(!has_unimplemented(&abilities[0]), "{oracle}");
}

/// The last node of a single-ability chain, asserted to be the for-each
/// clause's prior honest gap.
fn assert_for_each_stays_unparsed(oracle: &str) {
    let abilities = lowered(oracle);
    let nodes = chain_nodes(&abilities[0]);
    let last = nodes.last().expect("non-empty chain");
    assert!(
        matches!(&*last.effect, Effect::Unimplemented { name, .. } if name == "unparsed_quantity"),
        "{oracle}: the for-each clause keeps its prior parse, got {:?}",
        last.effect
    );
    assert_eq!(last.repeat_for, None);
}

/// V2.1a (A2.1, SHAPE): the admitted chain lowers to a `TrackedSetSize`
/// repeat over the for-each body with no gap.
#[test]
fn reveal_muster_parses_to_a_tracked_set_iteration() {
    assert_admitted_shape(REVEAL_MUSTER);
}

/// V2.1b (A2.1, SHAPE): the pronoun and every restating noun are claimed;
/// a noun naming a different population is not.
#[test]
fn population_heads_restating_the_kept_set_are_claimed() {
    for head in [
        "For each of those permanents,",
        "For each of those cards,",
        "For each of them,",
        "For each of these creatures,",
    ] {
        assert_admitted_shape(&with_for_each_head(head));
    }
    assert_for_each_stays_unparsed(&with_for_each_head("For each of those artifacts,"));
}

/// V2.1c (A2.1): the walk stops at an `Unimplemented` instruction, at a
/// producer of a different object population, and at a player-population
/// instruction; the for-each clause then keeps its prior parse.
#[test]
fn population_walk_stops_at_unimplemented_and_other_producers() {
    // (ii) An Unimplemented grant. A later unit converts this row into its
    // positive look-through once the plural goad grant parses.
    let goaded = with_intervening("They're goaded for the rest of the game.");
    assert!(
        any_node(&lowered(&goaded), |node| matches!(
            &*node.effect,
            Effect::Unimplemented { name, .. } if name == "unrecognized_clause_head"
        )),
        "reach-guard: the goaded clause is the intervening gap"
    );
    assert_for_each_stays_unparsed(&goaded);

    // (iii) A producer of a different object population in between.
    let exile = with_intervening("Exile target artifact.");
    assert!(any_node(&lowered(&exile), |node| matches!(
        &*node.effect,
        Effect::ChangeZone { .. }
    )));
    assert_for_each_stays_unparsed(&exile);

    // (iv) A player population in between.
    let lose_life = with_intervening("Each opponent loses 2 life.");
    assert!(any_node(&lowered(&lose_life), |node| matches!(
        &*node.effect,
        Effect::LoseLife { .. }
    )));
    assert_for_each_stays_unparsed(&lose_life);
}

/// V2.1d (C2.2): the producer must put each of its matches onto the
/// battlefield — a hand destination or a chosen subset is not admitted.
#[test]
fn reveal_until_shapes_other_than_kept_onto_battlefield_are_declined() {
    let hand = REVEAL_MUSTER
        .replace("onto the battlefield", "into your hand")
        .replace(" They gain haste until end of turn.", "");
    assert!(
        matches!(&*lowered(&hand)[0].effect, Effect::RevealUntil { .. }),
        "reach-guard: the reveal-until producer is present"
    );
    assert_for_each_stays_unparsed(&hand);

    let any_number = REVEAL_MUSTER
        .replace(
            "Put those creature cards",
            "Put any number of those creature cards",
        )
        .replace(" They gain haste until end of turn.", "");
    assert!(
        matches!(
            &*lowered(&any_number)[0].effect,
            Effect::RevealUntil {
                matched_disposition: RevealUntilDisposition::ChooseAnyNumber,
                ..
            }
        ),
        "reach-guard: the producer chooses a subset"
    );
    assert_for_each_stays_unparsed(&any_number);
}

fn card_abilities(
    oracle: &str,
    name: &str,
    keywords: &[&str],
    types: &[&str],
) -> Vec<AbilityDefinition> {
    let keywords: Vec<String> = keywords.iter().map(|k| k.to_string()).collect();
    let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &keywords, &types, &[]);
    parsed
        .abilities
        .into_iter()
        .chain(
            parsed
                .triggers
                .into_iter()
                .filter_map(|trigger| trigger.execute.map(|execute| *execute)),
        )
        .collect()
}

fn any_node(defs: &[AbilityDefinition], pred: impl Fn(&AbilityDefinition) -> bool) -> bool {
    defs.iter()
        .any(|def| chain_nodes(def).into_iter().any(&pred))
}

const SOUL_OF_EMANCIPATION: &str = "When this creature enters, destroy up to three other target \
     nonland permanents. For each of those permanents, its controller creates a 3/3 white Angel \
     creature token with flying.";
const DIVERGENT_TRANSFORMATIONS: &str = "Undaunted (This spell costs {1} less to cast for each \
     opponent.)\nExile two target creatures. For each of those creatures, its controller reveals \
     cards from the top of their library until they reveal a creature card, puts that card onto \
     the battlefield, then shuffles the rest into their library.";
const HOLLOW_MARAUDER: &str = "This spell costs {1} less to cast for each creature card in your \
     graveyard.\nFlying\nWhen this creature enters, any number of target opponents each discard a \
     card. For each of those opponents who didn't discard a card with mana value 4 or greater, \
     draw a card.";
const SANAR: &str = "Vivid — At the beginning of your first main phase, reveal cards from the \
     top of your library until you reveal X nonland cards, where X is the number of colors among \
     permanents you control. For each of those colors, you may exile a card of that color from \
     among the revealed cards. Then shuffle. You may cast the exiled cards this turn.";
const KABOOM: &str = "Choose any number of target players or planeswalkers. For each of them, \
     reveal cards from the top of your library until you reveal a nonland card, Kaboom! deals \
     damage equal to that card's mana value to that player or planeswalker, then you put the \
     revealed cards on the bottom of your library in any order.";
const TWINFLAME: &str = "Strive — This spell costs {2}{R} more to cast for each target beyond \
     the first.\nChoose any number of target creatures you control. For each of them, create a \
     token that's a copy of that creature, except it has haste. Exile those tokens at the \
     beginning of the next end step.";
const BOUNDING_FELIDAR: &str = "Whenever this creature attacks while saddled, put a +1/+1 counter \
     on each other creature you control. You gain 1 life for each of those creatures.\nSaddle 2 \
     (Tap any number of other creatures you control with total power 2 or more: This Mount \
     becomes saddled until end of turn. Saddle only as a sorcery.)";

/// V2.1e (C2.2, A2.1): real cards (verbatim Oracle text) whose for-each
/// clause names an unadmitted producer's population, a player population, a
/// non-object population, or a target set keep their prior parse; the generic
/// for-each path is still reached after the new arm declines.
#[test]
fn real_card_for_each_clauses_over_unadmitted_producers_keep_their_parse() {
    let unparsed_for_each = |def: &AbilityDefinition| {
        matches!(&*def.effect, Effect::Unimplemented { name, description }
            if name == "unparsed_quantity"
                && description.as_deref().is_some_and(|d| d.starts_with("For each of those")))
    };
    let no_tracked_repeat =
        |defs: &[AbilityDefinition]| !any_node(defs, |d| d.repeat_for == tracked_set_repeat());

    let soul = card_abilities(
        SOUL_OF_EMANCIPATION,
        "Soul of Emancipation",
        &[],
        &["Creature"],
    );
    assert!(any_node(&soul, |d| matches!(
        &*d.effect,
        Effect::Destroy { .. }
    )));
    assert!(
        any_node(&soul, unparsed_for_each),
        "Soul of Emancipation (Destroy over its targets) is not admitted"
    );

    let divergent = card_abilities(
        DIVERGENT_TRANSFORMATIONS,
        "Divergent Transformations",
        &["Undaunted"],
        &["Instant"],
    );
    assert!(any_node(&divergent, |d| matches!(
        &*d.effect,
        Effect::ChangeZone { .. }
    )));
    assert!(
        any_node(&divergent, unparsed_for_each),
        "ChangeZone to exile is not admitted"
    );

    let hollow = card_abilities(
        HOLLOW_MARAUDER,
        "Hollow Marauder",
        &["Flying"],
        &["Creature"],
    );
    assert!(any_node(&hollow, |d| matches!(
        &*d.effect,
        Effect::Discard { .. }
    )));
    assert!(
        any_node(&hollow, unparsed_for_each),
        "a player population is not admitted"
    );

    let sanar = card_abilities(SANAR, "Sanar, Innovative First-Year", &[], &["Creature"]);
    assert!(any_node(&sanar, |d| matches!(
        &*d.effect,
        Effect::RevealUntil { .. }
    )));
    assert!(
        no_tracked_repeat(&sanar),
        "\"of those colors\" is not an object population"
    );

    let kaboom = card_abilities(KABOOM, "Kaboom!", &[], &["Sorcery"]);
    assert!(any_node(&kaboom, |d| matches!(
        &*d.effect,
        Effect::RevealUntil { .. }
    )));
    assert!(
        no_tracked_repeat(&kaboom),
        "Kaboom!'s \"them\" precedes its reveal"
    );

    let twinflame = card_abilities(TWINFLAME, "Twinflame", &["Strive"], &["Sorcery"]);
    assert!(
        any_node(&twinflame, |d| matches!(
            &*d.effect,
            Effect::CopyTokenOf { .. }
        )),
        "Twinflame keeps its in-clause per-target binding"
    );
    assert!(no_tracked_repeat(&twinflame));

    let felidar = card_abilities(
        BOUNDING_FELIDAR,
        "Bounding Felidar",
        &["Saddle"],
        &["Creature"],
    );
    assert!(any_node(&felidar, |d| matches!(
        &*d.effect,
        Effect::GainLife { .. }
    )));
    assert!(
        no_tracked_repeat(&felidar),
        "a trailing multiplier is not a leading prefix"
    );

    // Positive reach-guard: the generic for-each arm is still reached.
    let generic = lowered("For each creature you control, put a +1/+1 counter on that creature.");
    assert!(matches!(
        &generic[0].repeat_for,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { .. }
        })
    ));
}

/// V2.1f (SHAPE): the population iteration composes with "choose a
/// different opponent". The runtime of this shape (and its member-bound
/// continuation) belongs to a later unit.
#[test]
fn population_iteration_composes_with_a_different_opponent_choice() {
    let oracle = REVEAL_MUSTER.replace(
        "put a +1/+1 counter on that creature.",
        "choose a different opponent.",
    );
    let abilities = lowered(&oracle);
    let nodes = chain_nodes(&abilities[0]);
    assert!(matches!(&*nodes[1].effect, Effect::Shuffle { .. }));
    assert!(matches!(&*nodes[2].effect, Effect::GenericEffect { .. }));
    let last = nodes.last().expect("chain");
    assert_eq!(last.repeat_for, tracked_set_repeat());
    assert!(
        matches!(
            &*last.effect,
            Effect::Choose {
                choice_type: ChoiceType::Opponent {
                    restriction: None,
                    distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices,
                },
                ..
            }
        ),
        "{:?}",
        last.effect
    );
    assert!(!has_unimplemented(&abilities[0]));
}

/// V2.2a (A2.2; C2.7, revert-failing): the body runs once for each kept
/// creature — through the shuffle and the haste grant — and never for a
/// revealed miss, a matching card beyond the count, or another permanent.
#[test]
fn reveal_muster_counters_exactly_the_kept_creatures() {
    let mut board = muster_board(REVEAL_MUSTER, MUSTER_LIBRARY);
    board.runner.cast(board.spell).resolve();
    let runner = &board.runner;
    let a = board.card("Creature A");
    let b = board.card("Creature B");

    // Runtime reach-guard: the kept creatures entered under the caster and
    // the grant applied to exactly them.
    for kept in [a, b] {
        assert_eq!(zone_of(runner, kept), Zone::Battlefield);
        assert_eq!(runner.state().objects[&kept].controller, P0);
        assert!(
            has_haste(runner, kept),
            "CR 611.2a: the kept creature gained haste"
        );
    }
    assert!(!has_haste(runner, board.bystander_p0));

    // CR 608.2c + CR 122.1a: one counter on each kept creature, nothing else.
    assert_eq!(p1p1(runner, a), 1, "Creature A gets exactly one counter");
    assert_eq!(p1p1(runner, b), 1, "Creature B gets exactly one counter");
    for name in ["Miss One", "Miss Two", "Creature C", "Miss Three"] {
        let id = board.card(name);
        assert!(in_p0_library(runner, id), "{name} stays in the library");
        assert_eq!(p1p1(runner, id), 0, "{name} is unaffected");
    }
    assert_eq!(p1p1(runner, board.bystander_p0), 0);
    assert_eq!(p1p1(runner, board.bystander_p1), 0);
    assert_eq!(total_p1p1(runner), 2);

    // Parse reach-guard: the cast card is the admitted shape.
    assert_admitted_shape(REVEAL_MUSTER);
}

/// V2.2b (CR 609.3): fewer matches than the count — one iteration per
/// creature found.
#[test]
fn reveal_muster_with_one_creature_iterates_once() {
    let library = [
        MusterCard::Sorcery("Miss One"),
        MusterCard::Creature("Creature A"),
        MusterCard::Land("Miss Two"),
    ];
    let mut board = muster_board(REVEAL_MUSTER, &library);
    board.runner.cast(board.spell).resolve();
    let runner = &board.runner;
    let a = board.card("Creature A");
    assert_eq!(zone_of(runner, a), Zone::Battlefield);
    assert!(has_haste(runner, a));
    assert_eq!(p1p1(runner, a), 1);
    assert_eq!(total_p1p1(runner), 1);
}

/// V2.2c (CR 609.3): no creature found — zero iterations, and the
/// resolution still completes.
#[test]
fn reveal_muster_with_no_creature_iterates_zero_times() {
    let library = [
        MusterCard::Sorcery("Miss One"),
        MusterCard::Land("Miss Two"),
        MusterCard::Sorcery("Miss Three"),
    ];
    let mut board = muster_board(REVEAL_MUSTER, &library);
    board.runner.cast(board.spell).resolve();
    let runner = &board.runner;
    for name in ["Miss One", "Miss Two", "Miss Three"] {
        assert!(
            in_p0_library(runner, board.card(name)),
            "{name} was revealed and returned"
        );
    }
    assert_eq!(zone_of(runner, board.spell), Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        engine::types::game_state::WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.battlefield_count(P0), 1, "only the bystander");
    assert_eq!(total_p1p1(runner), 0);
}
