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
    AbilityDefinition, ChoiceType, ContinuousModification, Duration, Effect, FilterProp,
    PlayerChoiceDistinctness, QuantityExpr, QuantityRef, RevealUntilDisposition, TargetFilter,
    TypedFilter,
};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// The synthetic A2.2 instrument (see the module doc).
pub(crate) const REVEAL_MUSTER: &str = "Reveal cards from the top of your library until you \
     reveal two creature cards. Put those creature cards onto the battlefield, then shuffle. \
     They gain haste until end of turn. For each of those creatures, put a +1/+1 counter on \
     that creature.";

const REVEAL_MUSTER_NAME: &str = "Reveal Muster";

/// V2.1d: the instrument with its kept cards sent to hand by a kept-destination
/// patch ("Put that card into your hand").
const REVEAL_MUSTER_HAND_PATCH: &str = "Reveal cards from the top of your library until you \
     reveal two creature cards. Put that card into your hand, then shuffle. For each of those \
     creatures, put a +1/+1 counter on that creature.";

/// V2.1d: the instrument with no kept-destination patch — the reveal's own
/// destination (hand) stands, and only the rest pile is patched.
const REVEAL_MUSTER_HAND_OWN: &str = "Reveal cards from the top of your library until you \
     reveal two creature cards. Put those cards into your hand and the rest on the bottom of \
     your library in a random order. For each of those creatures, put a +1/+1 counter on that \
     creature.";

/// One staged library card for the instrument's board.
#[derive(Clone, Copy)]
pub(crate) enum MusterCard {
    /// A noncreature sorcery card (a reveal miss).
    Sorcery(&'static str),
    /// A land card (a reveal miss).
    Land(&'static str),
    /// A 2/2 creature card (a reveal hit).
    Creature(&'static str),
    /// A 2/2 legendary creature card (a reveal hit sharing its name with
    /// another staged card — the legend rule's subject, CR 704.5j).
    Legendary(&'static str),
    /// A creature card carrying its verbatim Oracle text (and the MTGJSON
    /// keyword names its keyword lines need), e.g. True-Name Nemesis's
    /// "As this creature enters, choose a player." replacement.
    OracleCreature {
        name: &'static str,
        oracle: &'static str,
        keywords: &'static [&'static str],
        power: i32,
        toughness: i32,
    },
}

impl MusterCard {
    fn name(self) -> &'static str {
        match self {
            MusterCard::Sorcery(name)
            | MusterCard::Land(name)
            | MusterCard::Creature(name)
            | MusterCard::Legendary(name) => name,
            MusterCard::OracleCreature { name, .. } => name,
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
    let library = stage_muster_library(&mut scenario, P0, library_top_first);
    let bystander_p0 = scenario.add_creature(P0, "Bystander Zero", 1, 1).id();
    let bystander_p1 = scenario.add_creature(P1, "Bystander One", 1, 1).id();
    let mut runner = scenario.build();
    finish_muster_library(&mut runner, P0, library_top_first, &library);
    MusterBoard {
        runner,
        spell,
        library,
        bystander_p0,
        bystander_p1,
    }
}

/// Stages `cards` (top first) into `owner`'s library and returns each card's
/// name and id in the same order. An `OracleCreature` is built in the
/// graveyard (the only Oracle-text creature builder) and relocated into its
/// library position by [`finish_muster_library`].
pub(crate) fn stage_muster_library(
    scenario: &mut GameScenario,
    owner: PlayerId,
    cards: &[MusterCard],
) -> Vec<(&'static str, ObjectId)> {
    let mut library = Vec::new();
    for card in cards.iter().rev() {
        let id = match *card {
            MusterCard::Sorcery(name) => scenario.add_spell_to_library_top(owner, name, false).id(),
            MusterCard::Land(name) => scenario.add_land_to_library_top(owner, name).id(),
            MusterCard::Creature(name) | MusterCard::Legendary(name) => {
                scenario.add_card_to_library_top(owner, name)
            }
            MusterCard::OracleCreature {
                name,
                oracle,
                keywords,
                power,
                toughness,
            } => scenario
                .add_creature_to_graveyard(owner, name, power, toughness)
                .from_oracle_text_with_keywords(keywords, oracle)
                .id(),
        };
        library.push((card.name(), id));
    }
    library.reverse();
    library
}

/// Completes [`stage_muster_library`] after the build: a `Creature` card
/// becomes a 2/2 creature card, and an `OracleCreature` moves from the
/// graveyard to its staged library index. Reach-guard: every staged card is
/// in `owner`'s library at its staged index.
pub(crate) fn finish_muster_library(
    runner: &mut GameRunner,
    owner: PlayerId,
    cards: &[MusterCard],
    library: &[(&'static str, ObjectId)],
) {
    for (index, (card, (_, id))) in cards.iter().zip(library.iter()).enumerate() {
        match card {
            MusterCard::Creature(_) | MusterCard::Legendary(_) => {
                let obj = runner
                    .state_mut()
                    .objects
                    .get_mut(id)
                    .expect("staged creature card");
                if let MusterCard::Legendary(_) = card {
                    obj.card_types.supertypes.push(Supertype::Legendary);
                }
                obj.card_types.core_types.push(CoreType::Creature);
                obj.base_card_types = obj.card_types.clone();
                obj.power = Some(2);
                obj.toughness = Some(2);
                obj.base_power = Some(2);
                obj.base_toughness = Some(2);
            }
            MusterCard::OracleCreature { .. } => {
                // Insertion in increasing staged index lands every relocated
                // card at its staged position among the already-placed cards.
                let state = runner.state_mut();
                let player = state
                    .players
                    .iter_mut()
                    .find(|p| p.id == owner)
                    .expect("library owner exists");
                player.graveyard.retain(|card| card != id);
                player.library.insert(index, *id);
                state
                    .objects
                    .get_mut(id)
                    .expect("staged Oracle creature")
                    .zone = Zone::Library;
            }
            MusterCard::Sorcery(_) | MusterCard::Land(_) => {}
        }
    }
    let staged = &runner
        .state()
        .players
        .iter()
        .find(|p| p.id == owner)
        .expect("library owner exists")
        .library;
    for (index, (name, id)) in library.iter().enumerate() {
        assert_eq!(
            staged.get(index),
            Some(id),
            "reach-guard: {name} is in the library at its staged index"
        );
        assert_eq!(runner.state().objects[id].zone, Zone::Library, "{name}");
    }
}

pub(crate) fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

pub(crate) fn total_p1p1(runner: &GameRunner) -> u32 {
    runner
        .state()
        .objects
        .values()
        .filter_map(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .sum()
}

pub(crate) fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).expect("object exists").zone
}

/// The members a paused per-object repetition iterates, in iteration order:
/// the repeat driver's member snapshot while further repetitions are parked,
/// otherwise the single member bound to the pending answer on the parked
/// continuation. Read at the first per-object prompt. Panics if neither is
/// present (reach-guard: the resolution is paused inside the repetition).
pub(crate) fn iterated_members(state: &GameState) -> Vec<ObjectId> {
    if let Some(repeat) = state.active_repeat_for() {
        assert!(
            !repeat.tracked_members.is_empty(),
            "reach-guard: the parked repetition carries its member snapshot"
        );
        return repeat.tracked_members.clone();
    }
    let member = state
        .active_ability_continuation()
        .and_then(|pending| pending.chain.context.pending_choice_member)
        .expect("reach-guard: a per-object answer is pending for a bound member");
    vec![member]
}

/// The set the reveal published (CR 608.2c + CR 701.20a, "revealed this
/// way"), located by content: the highest-id tracked set holding every one of
/// `members`. Not the chain's current set — a nested as-enters child's
/// top-level reset can leave the outer chain bound to a later, empty set.
/// Panics if no set holds them (reach-guard: the reveal published its cards).
pub(crate) fn reveal_published_set(
    state: &GameState,
    members: &[ObjectId],
) -> (TrackedSetId, Vec<ObjectId>) {
    assert!(!members.is_empty(), "reach-guard: a non-empty population");
    state
        .tracked_object_sets
        .iter()
        .filter(|(_, set)| members.iter().all(|id| set.contains(id)))
        .max_by_key(|(id, _)| id.0)
        .map(|(id, set)| (*id, set.clone()))
        .unwrap_or_else(|| {
            panic!(
                "reach-guard: some published set holds the population {members:?}: {:?}",
                state.tracked_object_sets
            )
        })
}

/// CR 608.2c + CR 701.20a: the reveal's published set holds the revealed
/// misses as well as the kept cards — so it is the narrowing, not the
/// publication, that keeps a miss out of "those permanents". The set is
/// located by content over members and misses together: only the reveal
/// publishes a library miss, while a later grant over the kept permanents may
/// publish them alone into a later set.
pub(crate) fn assert_reveal_published_misses(
    state: &GameState,
    members: &[ObjectId],
    misses: &[ObjectId],
) {
    assert!(!misses.is_empty(), "reach-guard: staged misses to check");
    for miss in misses {
        assert!(
            !members.contains(miss),
            "the revealed miss {miss:?} is not iterated: {members:?}"
        );
    }
    let revealed: Vec<ObjectId> = members.iter().chain(misses).copied().collect();
    let (id, set) = reveal_published_set(state, &revealed);
    assert!(
        members.iter().all(|member| set.contains(member)),
        "the reveal's published set {id:?} = {set:?} holds every member {members:?}"
    );
}

pub(crate) fn has_haste(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .is_some_and(|obj| obj.has_keyword(&Keyword::Haste))
}

pub(crate) fn in_p0_library(runner: &GameRunner, id: ObjectId) -> bool {
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

/// CR 608.2c + CR 110.1: the lowered population of an admitted reveal-until —
/// the members of the reveal's published set (`TrackedSetId(0)`, the chain
/// sentinel) that are permanents.
pub(crate) fn population_form() -> TargetFilter {
    TargetFilter::TrackedSetFiltered {
        id: TrackedSetId(0),
        filter: Box::new(TargetFilter::Typed(TypedFilter::default().properties(
            vec![FilterProp::InZone {
                zone: Zone::Battlefield,
            }],
        ))),
        caused_by: None,
    }
}

/// The repeat count of "for each of those <noun>," over that population.
pub(crate) fn tracked_set_repeat() -> Option<QuantityExpr> {
    Some(QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: population_form(),
        },
    })
}

/// Whether `def` repeats over a published population in either form: the
/// whole tracked set, or an object count over it.
fn repeats_over_a_tracked_set(def: &AbilityDefinition) -> bool {
    match &def.repeat_for {
        Some(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetSize,
        }) => true,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        }) => matches!(
            filter,
            TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. }
        ),
        _ => false,
    }
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

/// `REVEAL_MUSTER` with its for-each body replaced.
fn with_body(body: &str) -> String {
    REVEAL_MUSTER.replace("put a +1/+1 counter on that creature.", body)
}

/// `REVEAL_MUSTER` with the sentence between the shuffle and the for-each
/// clause replaced.
fn with_intervening(sentence: &str) -> String {
    REVEAL_MUSTER.replace("They gain haste until end of turn.", sentence)
}

/// V2.1a's admitted shape: reveal-until kept onto the battlefield, a shuffle,
/// the anaphoric grant over the reveal's kept permanents, then the body
/// repeated over those permanents (the population form, not the raw published
/// set, which also holds the revealed misses).
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
    assert!(
        matches!(
            &*nodes[2].effect,
            Effect::GenericEffect { static_abilities, target: None, .. }
                if !static_abilities.is_empty()
                    && static_abilities
                        .iter()
                        .all(|grant| grant.affected == Some(population_form()))
        ),
        "{oracle}: the grant reads the kept permanents: {:?}",
        nodes[2].effect
    );
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

/// V2.1a (A2.1, SHAPE): the admitted chain lowers to an object-count repeat
/// over the reveal's kept permanents for the for-each body, with no gap.
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
    // (ii) An Unimplemented grant: the plural goad with no stated duration
    // keeps its prior parse (A3.1-i; its positive look-through is
    // `population_walk_looks_through_the_plural_goad_grant`).
    let goaded = with_intervening("They're goaded.");
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

/// The instrument with its haste grant replaced by "They're goaded for the
/// rest of the game." (A3.1).
fn reveal_muster_goaded() -> String {
    with_intervening("They're goaded for the rest of the game.")
}

/// A3.1-i (U2 + C3.6, SHAPE): CR 608.2c + CR 701.15a — with the plural goad
/// parsed (every copula spelling), the walk looks through the grant to the
/// reveal-until producer: the chain is RevealUntil → Shuffle → the `Goaded`
/// graft over the reveal's kept permanents for the rest of the game → the body
/// repeated over those permanents, with no gap. Reach-guard: the haste instrument itself has
/// the admitted shape.
#[test]
fn population_walk_looks_through_the_plural_goad_grant() {
    assert_admitted_shape(REVEAL_MUSTER);
    for sentence in [
        "They're goaded for the rest of the game.",
        "They\u{2019}re goaded for the rest of the game.",
        "They are goaded for the rest of the game.",
    ] {
        let oracle = with_intervening(sentence);
        assert_admitted_shape(&oracle);
        let abilities = lowered(&oracle);
        let grant = chain_nodes(&abilities[0])[2];
        assert!(
            matches!(
                &*grant.effect,
                Effect::GenericEffect {
                    static_abilities,
                    duration: Some(Duration::Permanent),
                    target: None,
                    ..
                } if static_abilities.len() == 1
                    && static_abilities[0].affected == Some(population_form())
                    && static_abilities[0].modifications
                        == vec![ContinuousModification::AddStaticMode {
                            mode: StaticMode::Goaded,
                        }]
            ),
            "{sentence}: {:?}",
            grant.effect
        );
    }
}

/// V2.1d (C2.2): the producer must put each of its matches onto the
/// battlefield — a hand destination or a chosen subset is not admitted.
/// The two hand variants are the shapes the walk classifies as a reveal-until
/// (the kept-destination patch, and the reveal's own destination under a
/// rest-pile patch); `population_walk_declines_only_for_the_kept_destination`
/// (oracle_effect/mod.rs) is their IR reach-guard.
#[test]
fn reveal_until_shapes_other_than_kept_onto_battlefield_are_declined() {
    for hand in [
        REVEAL_MUSTER_HAND_PATCH.to_string(),
        REVEAL_MUSTER_HAND_OWN.to_string(),
    ] {
        assert!(
            matches!(
                &*lowered(&hand)[0].effect,
                Effect::RevealUntil {
                    kept_destination: Zone::Hand,
                    ..
                }
            ),
            "reach-guard: the reveal-until producer keeps its matches in hand: {hand}"
        );
        assert_for_each_stays_unparsed(&hand);
    }

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
    // Neither population form: not the lowered one, and no other repeat over a
    // published set.
    let no_tracked_repeat = |defs: &[AbilityDefinition]| {
        !any_node(defs, |d| {
            d.repeat_for == tracked_set_repeat() || repeats_over_a_tracked_set(d)
        })
    };

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
/// resolution still completes. A non-empty tracked set from an earlier
/// resolution is already in the game (its member is a permanent), so the zero
/// holds only if the reveal publishes its own fresh set — holding only the
/// revealed misses, none of them a permanent — rather than nothing.
#[test]
fn reveal_muster_with_no_creature_iterates_zero_times() {
    let library = [
        MusterCard::Sorcery("Miss One"),
        MusterCard::Land("Miss Two"),
        MusterCard::Sorcery("Miss Three"),
    ];
    let mut board = muster_board(REVEAL_MUSTER, &library);
    // An earlier resolution's published set, allocated the way
    // `publish_tracked_set` allocates one: the next id, then the counter bump.
    let earlier = {
        let state = board.runner.state_mut();
        assert_eq!(state.chain_tracked_set_id, None, "no resolution in flight");
        let id = TrackedSetId(state.next_tracked_set_id);
        state.next_tracked_set_id += 1;
        state
            .tracked_object_sets
            .insert(id, vec![board.bystander_p0]);
        id
    };
    board.runner.cast(board.spell).resolve();
    let runner = &board.runner;
    // Reach-guard: the earlier non-empty set is still in the game, and the
    // reveal published a newer, empty one — the set the iteration counts.
    assert_eq!(
        runner.state().tracked_object_sets.get(&earlier),
        Some(&vec![board.bystander_p0]),
        "the earlier non-empty set persists"
    );
    let (latest, members) = runner
        .state()
        .tracked_object_sets
        .iter()
        .max_by_key(|(id, _)| id.0)
        .expect("a tracked set exists");
    assert!(latest.0 > earlier.0, "the reveal published a fresh set");
    // CR 608.2c + CR 701.20a: the fresh set holds exactly the three revealed
    // misses ("revealed this way"); none is a permanent, so the iteration runs
    // zero times.
    let mut revealed = members.clone();
    revealed.sort();
    let mut misses: Vec<ObjectId> = ["Miss One", "Miss Two", "Miss Three"]
        .into_iter()
        .map(|name| board.card(name))
        .collect();
    misses.sort();
    assert_eq!(
        revealed, misses,
        "the fresh set holds exactly Miss One, Miss Two, Miss Three"
    );
    assert_eq!(
        p1p1(runner, board.bystander_p0),
        0,
        "the earlier set's member is not iterated"
    );
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

/// The trigger-body variant of the instrument (V2.1g(e)): the same chain under
/// an enters trigger, the frame Dack Fayden's for-each clause sits in.
const MUSTER_HERALD: &str = "When this creature enters, reveal cards from the top of your \
     library until you reveal two creature cards. Put those creature cards onto the battlefield, \
     then shuffle. They gain haste until end of turn. For each of those creatures, put a +1/+1 \
     counter on it.";

/// The for-each node (node[3]) of the instrument's single lowered chain.
fn for_each_node(abilities: &[AbilityDefinition]) -> &AbilityDefinition {
    chain_nodes(&abilities[0])
        .get(3)
        .copied()
        .unwrap_or_else(|| panic!("chain has a fourth node: {abilities:?}"))
}

/// V2.1g(a) (A2.1, SHAPE): CR 608.2c — within one iteration the body's bare
/// "it" names that iteration's member, the nearest singular antecedent, so the
/// counter recipient is the member the repeat driver binds (`ParentTarget`),
/// never the resolving spell.
#[test]
fn population_body_it_binds_the_iterated_member() {
    assert_admitted_shape(&with_body("put a +1/+1 counter on it."));
}

/// V2.1g(b) (CR 201.5): a body that names the card itself keeps the source as
/// its recipient — the member binding answers only bare anaphors.
#[test]
fn population_body_naming_the_card_keeps_the_source() {
    let abilities = lowered(&with_body("put a +1/+1 counter on Reveal Muster."));
    let node = for_each_node(&abilities);
    // Reach-guard: the population arm claimed the clause, so the member
    // binding was live in this chunk.
    assert_eq!(node.repeat_for, tracked_set_repeat(), "{:?}", node.effect);
    assert!(
        matches!(
            &*node.effect,
            Effect::PutCounter {
                target: TargetFilter::SelfRef,
                ..
            }
        ),
        "the card's own name stays the source: {:?}",
        node.effect
    );
}

/// V2.1g(c) (CR 608.2c, fail-closed): each iteration binds one member, so a
/// plural object anaphor in the body names a set, not that member — the arm
/// does not claim the clause and it keeps its prior parse.
#[test]
fn population_body_with_a_plural_anaphor_is_not_claimed() {
    let them = with_body("put a +1/+1 counter on them.");
    let abilities = lowered(&them);
    let nodes = chain_nodes(&abilities[0]);
    // Reach-guard: the walk reached the for-each clause through the real
    // producer, shuffle and grant.
    assert!(matches!(
        &*nodes[0].effect,
        Effect::RevealUntil {
            kept_destination: Zone::Battlefield,
            ..
        }
    ));
    assert!(nodes
        .iter()
        .any(|node| matches!(&*node.effect, Effect::Shuffle { .. })));
    assert!(nodes
        .iter()
        .any(|node| matches!(&*node.effect, Effect::GenericEffect { .. })));
    assert_for_each_stays_unparsed(&them);
    // Reach-guard: the decline is pronoun-specific — the singular body under
    // the same head is claimed.
    assert_admitted_shape(&with_body("put a +1/+1 counter on it."));
}

/// V2.1g(d): the generic for-each arm seeds no member binding — its body's
/// recipient is whatever the same sentence without the prefix lowers to.
/// (This pins only that the generic arm is unchanged, not its reading.)
#[test]
fn generic_for_each_body_gets_no_member_binding() {
    let for_each = lowered("For each creature you control, put a +1/+1 counter on it.");
    let bare = lowered("Put a +1/+1 counter on it.");
    // Reach-guard: the generic arm claimed the for-each text.
    assert!(
        matches!(
            &for_each[0].repeat_for,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { .. }
            })
        ),
        "{:?}",
        for_each[0]
    );
    let recipient = |def: &AbilityDefinition| match &*def.effect {
        Effect::PutCounter { target, .. } => target.clone(),
        other => panic!("expected PutCounter, got {other:?}"),
    };
    assert_eq!(recipient(&for_each[0]), recipient(&bare[0]));
}

/// V2.1g(e): the instrument's chain under an enters trigger (Dack Fayden's
/// frame) — the iterated member outranks the trigger-level object antecedent.
#[test]
fn population_body_it_binds_the_member_under_a_trigger() {
    let abilities = card_abilities(MUSTER_HERALD, "Muster Herald", &[], &["Creature"]);
    assert_eq!(abilities.len(), 1, "{abilities:?}");
    let nodes = chain_nodes(&abilities[0]);
    // Reach-guard: the trigger's producer, shuffle and grant are present.
    assert!(matches!(
        &*nodes[0].effect,
        Effect::RevealUntil {
            kept_destination: Zone::Battlefield,
            ..
        }
    ));
    assert!(matches!(&*nodes[1].effect, Effect::Shuffle { .. }));
    assert!(matches!(&*nodes[2].effect, Effect::GenericEffect { .. }));
    let node = for_each_node(&abilities);
    assert_eq!(node.repeat_for, tracked_set_repeat(), "{:?}", node.effect);
    assert!(
        matches!(
            &*node.effect,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "{:?}",
        node.effect
    );
    assert!(!has_unimplemented(&abilities[0]));
}

/// V2.1g(f) (A2.1, SHAPE): a subject-position bare "it" in the body names the
/// iterated member too. Parse only; this phase asserts no runtime for a
/// non-counter body.
#[test]
fn population_body_subject_it_binds_the_iterated_member() {
    let abilities = lowered(&with_body("it gets +2/+2 until end of turn."));
    let nodes = chain_nodes(&abilities[0]);
    assert!(matches!(
        &*nodes[0].effect,
        Effect::RevealUntil {
            kept_destination: Zone::Battlefield,
            ..
        }
    ));
    assert!(matches!(&*nodes[1].effect, Effect::Shuffle { .. }));
    assert!(matches!(&*nodes[2].effect, Effect::GenericEffect { .. }));
    let node = for_each_node(&abilities);
    // Reach-guard: the population arm claimed the clause.
    assert_eq!(node.repeat_for, tracked_set_repeat(), "{:?}", node.effect);
    assert!(
        matches!(
            &*node.effect,
            Effect::Pump {
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "{:?}",
        node.effect
    );
    assert!(!has_unimplemented(&abilities[0]));
}

/// V2.2d (A2.2; C2.7(b) on the charter's own example form, revert-failing):
/// "put a +1/+1 counter on it" counters exactly the kept creatures — the
/// member each iteration binds — and never the resolving spell.
#[test]
fn reveal_muster_it_body_counters_exactly_the_kept_creatures() {
    let oracle = with_body("put a +1/+1 counter on it.");
    let mut board = muster_board(&oracle, MUSTER_LIBRARY);
    board.runner.cast(board.spell).resolve();
    let runner = &board.runner;
    let a = board.card("Creature A");
    let b = board.card("Creature B");

    // Runtime reach-guard: the kept creatures entered under the caster and
    // the grant applied to exactly them.
    for kept in [a, b] {
        assert_eq!(zone_of(runner, kept), Zone::Battlefield);
        assert_eq!(runner.state().objects[&kept].controller, P0);
        assert!(has_haste(runner, kept), "the kept creature gained haste");
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
    assert_admitted_shape(&oracle);
}

/// The resolution-installed goad effects on `id`: `(effect controller,
/// duration)` of every transient effect adding `StaticMode::Goaded` to exactly
/// that object.
pub(crate) fn goad_tces(state: &GameState, id: ObjectId) -> Vec<(PlayerId, Duration)> {
    state
        .transient_continuous_effects
        .iter()
        .filter(|tce| {
            tce.affected == TargetFilter::SpecificObject { id }
                && tce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::Goaded
                        }
                    )
                })
        })
        .map(|tce| (tce.controller, tce.duration.clone()))
        .collect()
}

/// A3.1-r (U2 + C3.1, runtime building block): CR 701.15a + CR 701.15b +
/// CR 608.2c + CR 611.2a — "They're goaded for the rest of the game" goads
/// exactly the kept creatures, by the caster, for the rest of the game; the
/// per-member body still runs once for each of them.
#[test]
fn reveal_muster_plural_goad_reaches_exactly_the_kept_creatures() {
    let oracle = reveal_muster_goaded();
    let mut board = muster_board(&oracle, MUSTER_LIBRARY);
    board.runner.cast(board.spell).resolve();
    let runner = &board.runner;
    let a = board.card("Creature A");
    let b = board.card("Creature B");

    // Reach-guard: the kept creatures entered under the caster.
    for kept in [a, b] {
        assert_eq!(zone_of(runner, kept), Zone::Battlefield);
        assert_eq!(runner.state().objects[&kept].controller, P0);
    }
    for kept in [a, b] {
        assert_eq!(
            goad_tces(runner.state(), kept),
            vec![(P0, Duration::Permanent)],
            "CR 701.15b + CR 611.2a: one goad by the caster, for the rest of the game"
        );
        assert_eq!(p1p1(runner, kept), 1, "the body ran once for the member");
    }
    for name in ["Miss One", "Miss Two", "Creature C", "Miss Three"] {
        let id = board.card(name);
        assert!(in_p0_library(runner, id), "{name} stays in the library");
        assert!(
            goad_tces(runner.state(), id).is_empty(),
            "{name} is not goaded"
        );
    }
    for bystander in [board.bystander_p0, board.bystander_p1] {
        assert!(goad_tces(runner.state(), bystander).is_empty());
    }
    assert_eq!(total_p1p1(runner), 2);
}

/// V4.7 (Phase 4, S2c): CR 608.2c — an iteration over the reveal's kept
/// permanents is one instruction; the following sentence ("You gain 1 life.")
/// is a separate instruction and runs once, after the loop — not once per
/// member, as the member-driven full-chain heuristic for battlefield-query
/// loops would run it. Reach-guard: the body ran for each kept creature.
#[test]
fn reveal_muster_following_sentence_runs_once() {
    let oracle = format!("{REVEAL_MUSTER} You gain 1 life.");
    let mut board = muster_board(&oracle, MUSTER_LIBRARY);
    let start = board.runner.life(P0);
    board.runner.cast(board.spell).resolve();
    let runner = &board.runner;
    let a = board.card("Creature A");
    let b = board.card("Creature B");
    assert_eq!(p1p1(runner, a), 1, "Creature A gets exactly one counter");
    assert_eq!(p1p1(runner, b), 1, "Creature B gets exactly one counter");
    assert_eq!(total_p1p1(runner), 2);
    assert_eq!(
        runner.life(P0),
        start + 1,
        "CR 608.2c: the following sentence runs exactly once"
    );
    // Parse reach-guard: the for-each node repeats over the kept permanents and
    // the life gain is its following sibling.
    let abilities = lowered(&oracle);
    let node = for_each_node(&abilities);
    assert_eq!(node.repeat_for, tracked_set_repeat(), "{:?}", node.effect);
    assert!(
        matches!(
            node.sub_ability.as_deref().map(|sub| &*sub.effect),
            Some(Effect::GainLife { .. })
        ),
        "{:?}",
        node.sub_ability
    );
}

/// V4.17 (Phase 4, per-member optionality): CR 608.2c + CR 608.2d — in "For
/// each of those creatures, you may put a +1/+1 counter on that creature",
/// the "you may" lies inside the scope of "for each", so each repetition
/// carries its own choice, announced while that repetition is applied: one
/// prompt per kept creature, each answered on its own. Accepting the first
/// and declining the second puts exactly one counter, on exactly one of the
/// two kept creatures. Reach-guard: the for-each node parses optional and
/// repeats over the kept permanents, so the prompt count measures the driver.
#[test]
fn reveal_muster_optional_body_prompts_once_per_kept_creature() {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;

    let oracle = with_body("you may put a +1/+1 counter on that creature.");
    let abilities = lowered(&oracle);
    let node = for_each_node(&abilities);
    assert!(node.optional, "parse reach-guard: the body is optional");
    assert_eq!(node.repeat_for, tracked_set_repeat(), "{:?}", node.effect);

    let mut board = muster_board(&oracle, MUSTER_LIBRARY);
    // Drive the resolution by hand: the cast driver answers optional prompts
    // itself, and this row answers each one differently.
    drop(board.runner.cast(board.spell).commit());
    for _ in 0..8 {
        if board.runner.state().stack.is_empty()
            || matches!(
                board.runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            )
        {
            break;
        }
        board
            .runner
            .act(GameAction::PassPriority)
            .expect("passing priority resolves the spell");
    }
    let mut prompts = 0;
    for accept in [true, false] {
        assert!(
            matches!(
                board.runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player, .. } if player == P0
            ),
            "prompt {}: expected P0's optional-effect choice, got {:?}",
            prompts + 1,
            board.runner.state().waiting_for
        );
        prompts += 1;
        board
            .runner
            .act(GameAction::DecideOptionalEffect { accept })
            .expect("answering the optional choice must succeed");
    }
    assert!(
        !matches!(
            board.runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "exactly two optional prompts, got a further {:?}",
        board.runner.state().waiting_for
    );
    assert_eq!(prompts, 2);

    let runner = &board.runner;
    let a = board.card("Creature A");
    let b = board.card("Creature B");
    // Which of the two carries the counter is the engine-defined member order
    // (the card does not order its choices, CR 608.2d), so it is not asserted.
    let mut counters = [p1p1(runner, a), p1p1(runner, b)];
    counters.sort();
    assert_eq!(
        counters,
        [0, 1],
        "exactly one kept creature got the counter, the other none"
    );
    assert_eq!(total_p1p1(runner), 1);
    for name in ["Miss One", "Miss Two"] {
        assert_eq!(p1p1(runner, board.card(name)), 0, "{name} is unaffected");
    }
    for id in [a, b] {
        assert_eq!(zone_of(runner, id), Zone::Battlefield);
        assert!(has_haste(runner, id));
    }
    for name in ["Miss One", "Miss Two", "Creature C", "Miss Three"] {
        assert!(
            in_p0_library(runner, board.card(name)),
            "{name} stays in the library"
        );
    }
}
