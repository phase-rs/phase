//! Runtime coverage for the reveal-until "that many" class and the matched-set
//! disposition "put all <filter> cards revealed this way <zone>".
//!
//! * Mass Polymorph — "Exile all creatures you control, then reveal cards from
//!   the top of your library until you reveal that many creature cards." The
//!   count is the number of creatures the preceding instruction actually exiled
//!   (CR 608.2c + CR 608.2h), read in the same resolution.
//! * Synthetic Destiny — the same reveal, but inside a delayed triggered ability
//!   created by the spell (CR 603.7a). "That many" is the number determined when
//!   the spell resolved; the end-step resolution must not recount anything.
//! * Old Stickfingers — "Put all creature cards revealed this way into your
//!   graveyard" names the matched set (CR 701.20a), so the creature cards go
//!   straight to the graveyard, never to the hand.

use engine::game::coverage::card_face_has_unimplemented_parts;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::game_state::{AutoPassRequest, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MASS_POLYMORPH: &str = "Exile all creatures you control, then reveal cards from the top of your library until you reveal that many creature cards. Put all creature cards revealed this way onto the battlefield, then shuffle the rest of the revealed cards into your library.";

const SYNTHETIC_DESTINY: &str = "Exile all creatures you control. At the beginning of the next end step, reveal cards from the top of your library until you reveal that many creature cards, put all creature cards revealed this way onto the battlefield, then shuffle the rest of the revealed cards into your library.";

const OLD_STICKFINGERS: &str = "When you cast this spell, reveal cards from the top of your library until you reveal X creature cards. Put all creature cards revealed this way into your graveyard, then put the rest on the bottom of your library in a random order.\nOld Stickfingers's power and toughness are each equal to the number of creature cards in your graveyard.";

/// A batched "one or more other creatures you control enter" observer — the
/// subject phrase Celes, Rune Knight and Frantic Scapegoat print.
const BATCHED_NEWCOMER_OBSERVER: &str =
    "Whenever one or more other creatures you control enter, you gain 1 life.";

/// The staged board shared by the Mass Polymorph / Synthetic Destiny tests.
struct Board {
    /// P0's nontoken creature (exiled, counted).
    a0: ObjectId,
    /// P0's creature token (exiled and counted; CR 111.7 / CR 704.5d it then
    /// ceases to exist).
    t0: ObjectId,
    /// P1's creature — "creatures you control" never touches it.
    o1: ObjectId,
    miss1: ObjectId,
    creature_a: ObjectId,
    miss2: ObjectId,
    creature_b: ObjectId,
    creature_c: ObjectId,
    bottom_marker: ObjectId,
    spell: ObjectId,
}

fn library_creature(scenario: &mut GameScenario, name: &str, oracle: Option<&str>) -> ObjectId {
    let mut builder = scenario.add_spell_to_library_top(P0, name, false);
    builder.as_creature().with_subtypes(vec!["Bear"]);
    if let Some(text) = oracle {
        builder.from_oracle_text(text);
    }
    builder.id()
}

/// Builds P0's library, top to bottom: Miss1 (sorcery), CreatureA, Miss2
/// (land), CreatureB, CreatureC, BottomMarker. Each `add_*_to_library_top`
/// inserts at the top, so the library is built bottom-first.
fn stage_library(
    scenario: &mut GameScenario,
    observer: Option<&str>,
) -> (ObjectId, ObjectId, ObjectId, ObjectId, ObjectId, ObjectId) {
    let bottom_marker = scenario.add_card_to_library_top(P0, "Bottom Marker");
    let creature_c = library_creature(scenario, "Creature C", None);
    let creature_b = library_creature(scenario, "Creature B", observer);
    let miss2 = scenario.add_land_to_library_top(P0, "Miss Two").id();
    let creature_a = library_creature(scenario, "Creature A", observer);
    let miss1 = scenario
        .add_spell_to_library_top(P0, "Miss One", false)
        .id();
    (
        miss1,
        creature_a,
        miss2,
        creature_b,
        creature_c,
        bottom_marker,
    )
}

/// The library builder has no P/T setter, so give the library creatures
/// printed P/T after `build()`.
fn set_library_creature_pt(runner: &mut GameRunner, ids: &[ObjectId]) {
    for id in ids {
        let obj = runner.state_mut().objects.get_mut(id).unwrap();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }
}

fn build_board(
    name: &str,
    is_instant: bool,
    oracle: &str,
    observer: Option<&str>,
) -> (GameRunner, Board) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let a0 = scenario.add_creature(P0, "Bystander", 2, 2).id();
    let t0 = scenario.add_creature(P0, "Soldier Token", 1, 1).id();
    let o1 = scenario.add_creature(P1, "Opposing Creature", 2, 2).id();
    let (miss1, creature_a, miss2, creature_b, creature_c, bottom_marker) =
        stage_library(&mut scenario, observer);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, is_instant, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&t0).unwrap().is_token = true;
    set_library_creature_pt(&mut runner, &[creature_a, creature_b, creature_c]);
    (
        runner,
        Board {
            a0,
            t0,
            o1,
            miss1,
            creature_a,
            miss2,
            creature_b,
            creature_c,
            bottom_marker,
            spell,
        },
    )
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn assert_zone(runner: &GameRunner, ids: &[ObjectId], zone: Zone, what: &str) {
    for id in ids {
        assert_eq!(
            zone_of(runner, *id),
            zone,
            "{what} ({id:?}) must be in {zone:?}"
        );
    }
}

fn library_len(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].library.len()
}

fn token_is_gone(runner: &GameRunner, token: ObjectId) -> bool {
    !runner.state().battlefield.contains(&token)
        && runner
            .state()
            .objects
            .get(&token)
            .is_none_or(|obj| obj.zone != Zone::Battlefield)
}

fn assert_stack_empty_priority(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "every spell and trigger must have resolved; waiting_for={}, stack={:?}",
        runner.waiting_for_kind(),
        runner.stack_names()
    );
}

/// Reach guard: the card's verbatim Oracle text parses with no
/// `Effect::Unimplemented` anywhere (including inside delayed triggers).
fn assert_parses_fully(name: &str, types: &[&str], oracle: &str) {
    let parsed = parse_oracle_text(
        oracle,
        name,
        &[],
        &types.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
        &[],
    );
    let face = CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        additional_cost: parsed.additional_cost,
        ..Default::default()
    };
    assert!(
        !card_face_has_unimplemented_parts(&face),
        "{name} must parse with no Unimplemented part: {face:#?}"
    );
}

/// Advance to the end step and stop once the delayed trigger is on the stack
/// (mirrors `delayed_departure_lookback::pass_to_delayed_trigger`).
pub(crate) fn pass_to_delayed_trigger(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order delayed trigger");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward delayed trigger");
            }
            WaitingFor::Priority { .. } => return,
            // CR 508.1: no attack is declared on the way to the end step.
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            other => panic!("unexpected state while advancing to delayed trigger: {other:?}"),
        }
    }
    panic!("delayed trigger did not surface");
}

/// CR 608.2c + CR 608.2h + CR 701.20a: Mass Polymorph exiles both of P0's
/// creatures (the token is exiled and counts; CR 111.7 / CR 704.5d it then
/// ceases to exist), so "that many" is 2: the first two
/// creature cards revealed enter, the third creature card stays in the library
/// with the misses (CR 701.24a shuffle). CR 603.6a + CR 603.2c: the two creature
/// cards enter in ONE event, so each batched "one or more other creatures you
/// control enter" newcomer sees the other's entry — exactly +2 life.
#[test]
fn mass_polymorph_reveals_until_that_many_and_creatures_enter_together() {
    let (mut runner, board) = build_board(
        "Mass Polymorph",
        false,
        MASS_POLYMORPH,
        Some(BATCHED_NEWCOMER_OBSERVER),
    );
    let initial_library = library_len(&runner);
    let life_before = runner.life(P0);

    runner.cast(board.spell).resolve();
    runner.advance_until_stack_empty();
    assert_stack_empty_priority(&runner);

    // The exile instruction: only P0's creatures, the token included.
    assert_zone(&runner, &[board.a0], Zone::Exile, "P0's nontoken creature");
    assert!(
        token_is_gone(&runner, board.t0),
        "P0's creature token must be exiled"
    );
    assert_zone(
        &runner,
        &[board.o1],
        Zone::Battlefield,
        "P1's creature (not \"you control\")",
    );

    // "that many" = 2: exactly the first two creature cards enter.
    assert_zone(
        &runner,
        &[board.creature_a, board.creature_b],
        Zone::Battlefield,
        "the first two revealed creature cards",
    );
    assert_zone(
        &runner,
        &[
            board.creature_c,
            board.miss1,
            board.miss2,
            board.bottom_marker,
        ],
        Zone::Library,
        "the third creature card, the misses and the unrevealed card",
    );
    assert_eq!(
        library_len(&runner),
        initial_library - 2,
        "exactly two cards left the library"
    );

    // CR 603.6a + CR 603.2c: one simultaneous entry, each newcomer observes the
    // other — exactly +2 (per-card delivery gives +1; double collection > 2).
    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "each co-entering batched observer must see the other creature enter"
    );
}

/// Ruling: if "that many" exceeds the creature cards left, the whole library is
/// revealed, every creature card revealed enters, and the rest is shuffled back.
#[test]
fn mass_polymorph_exhausts_library_when_too_few_creature_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let exiled = [
        scenario.add_creature(P0, "Bystander One", 2, 2).id(),
        scenario.add_creature(P0, "Bystander Two", 2, 2).id(),
        scenario.add_creature(P0, "Bystander Three", 2, 2).id(),
    ];
    let bottom_marker = scenario.add_card_to_library_top(P0, "Bottom Marker");
    let miss2 = scenario.add_land_to_library_top(P0, "Miss Two").id();
    let creature_a = library_creature(&mut scenario, "Creature A", None);
    let miss1 = scenario
        .add_spell_to_library_top(P0, "Miss One", false)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Mass Polymorph", false, MASS_POLYMORPH)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    set_library_creature_pt(&mut runner, &[creature_a]);
    let initial_library = library_len(&runner);

    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();
    assert_stack_empty_priority(&runner);

    assert_zone(&runner, &exiled, Zone::Exile, "P0's exiled creatures");
    assert_zone(
        &runner,
        &[creature_a],
        Zone::Battlefield,
        "the only creature card in the library",
    );
    assert_zone(
        &runner,
        &[miss1, miss2, bottom_marker],
        Zone::Library,
        "every noncreature card revealed",
    );
    assert_eq!(library_len(&runner), initial_library - 1);
}

/// With no creatures to exile, "that many" is 0: nothing is revealed and
/// nothing enters (CR 701.20a).
#[test]
fn mass_polymorph_with_no_creatures_moves_nothing() {
    assert_parses_fully("Mass Polymorph", &["Sorcery"], MASS_POLYMORPH);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let o1 = scenario.add_creature(P1, "Opposing Creature", 2, 2).id();
    let (miss1, creature_a, miss2, creature_b, creature_c, bottom_marker) =
        stage_library(&mut scenario, None);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Mass Polymorph", false, MASS_POLYMORPH)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    set_library_creature_pt(&mut runner, &[creature_a, creature_b, creature_c]);
    let initial_library = library_len(&runner);

    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();
    assert_stack_empty_priority(&runner);

    assert_zone(&runner, &[spell], Zone::Graveyard, "the resolved sorcery");
    assert_zone(&runner, &[o1], Zone::Battlefield, "P1's creature");
    assert_zone(
        &runner,
        &[
            miss1,
            creature_a,
            miss2,
            creature_b,
            creature_c,
            bottom_marker,
        ],
        Zone::Library,
        "every library card",
    );
    assert_eq!(library_len(&runner), initial_library);
}

/// CR 603.7a + CR 608.2c + CR 608.2h: Synthetic Destiny's delayed reveal reads
/// "that many" as the number of creatures the spell exiled (2), fixed when the
/// spell resolved. A creature that arrives later does not change it, and the
/// end-step resolution (whose resolution-local count was reset) does not read 0.
#[test]
fn synthetic_destiny_end_step_reveal_uses_the_count_fixed_at_resolution() {
    let (mut runner, board) = build_board("Synthetic Destiny", true, SYNTHETIC_DESTINY, None);

    runner.cast(board.spell).resolve();
    runner.advance_until_stack_empty();

    assert_zone(&runner, &[board.a0], Zone::Exile, "P0's nontoken creature");
    assert!(
        token_is_gone(&runner, board.t0),
        "P0's creature token must be exiled"
    );
    assert_zone(
        &runner,
        &[board.creature_a],
        Zone::Library,
        "nothing is revealed until the end step",
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the spell must install exactly one delayed trigger"
    );

    // Hostile: a creature arriving after resolution must not change N.
    let latecomer = create_object(
        runner.state_mut(),
        CardId(9_001),
        P0,
        "Latecomer".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&latecomer).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
    }

    pass_to_delayed_trigger(&mut runner);
    runner.advance_until_stack_empty();
    assert_stack_empty_priority(&runner);
    assert_eq!(runner.state().phase, Phase::End);

    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the one-shot delayed trigger must have fired"
    );
    assert_zone(
        &runner,
        &[board.creature_a, board.creature_b],
        Zone::Battlefield,
        "the first two revealed creature cards",
    );
    assert_zone(
        &runner,
        &[
            board.creature_c,
            board.miss1,
            board.miss2,
            board.bottom_marker,
        ],
        Zone::Library,
        "the third creature card and the misses",
    );
    assert_zone(&runner, &[latecomer], Zone::Battlefield, "the latecomer");
}

/// Synthetic Destiny with no creatures: the delayed trigger is installed and
/// consumed, and nothing enters.
#[test]
fn synthetic_destiny_with_no_creatures_reveals_nothing_at_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (miss1, creature_a, miss2, creature_b, creature_c, bottom_marker) =
        stage_library(&mut scenario, None);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Destiny", true, SYNTHETIC_DESTINY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    set_library_creature_pt(&mut runner, &[creature_a, creature_b, creature_c]);
    let initial_library = library_len(&runner);

    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach guard: the delayed trigger is installed"
    );

    pass_to_delayed_trigger(&mut runner);
    runner.advance_until_stack_empty();
    assert_stack_empty_priority(&runner);

    assert!(
        runner.state().delayed_triggers.is_empty(),
        "reach guard: the delayed trigger fired"
    );
    assert_zone(
        &runner,
        &[
            miss1,
            creature_a,
            miss2,
            creature_b,
            creature_c,
            bottom_marker,
        ],
        Zone::Library,
        "every library card",
    );
    assert_eq!(library_len(&runner), initial_library);
}

/// The Arkenstone — its end-step draw stamps a count (`last_effect_count`) when
/// it resolves.
const THE_ARKENSTONE: &str =
    "Creatures you control get +1/+1.\nAt the beginning of your end step, draw a card.";

/// CR 603.7a + CR 608.2h: Synthetic Destiny exiling no creatures fixes "that
/// many" at 0 when the spell resolves. At the end step The Arkenstone's draw
/// trigger resolves first, in the SAME player action (one stack-resolution
/// session), and stamps its own count; the delayed reveal must still reveal for
/// 0 creature cards, not for the draw's count.
#[test]
fn synthetic_destiny_zero_count_is_not_replaced_by_a_count_stamped_earlier_in_the_action() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arkenstone = scenario
        .add_artifact_from_oracle(P0, "The Arkenstone", THE_ARKENSTONE)
        .id();
    let (miss1, creature_a, miss2, creature_b, creature_c, bottom_marker) =
        stage_library(&mut scenario, None);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Destiny", true, SYNTHETIC_DESTINY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    set_library_creature_pt(&mut runner, &[creature_a, creature_b, creature_c]);

    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach guard: the delayed trigger is installed"
    );

    runner.advance_to_end_step();
    let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "both end-step triggers must ask P0 for an order; waiting_for={}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(triggers.len(), 2, "the delayed reveal and The Arkenstone");
    // Index 0 is placed first (bottom): put the delayed reveal under The
    // Arkenstone's draw so the draw resolves first.
    let arkenstone_index = triggers
        .iter()
        .position(|trigger| trigger.source_id == arkenstone)
        .expect("The Arkenstone's end-step trigger is pending");
    let mut order: Vec<usize> = (0..triggers.len())
        .filter(|&index| index != arkenstone_index)
        .collect();
    order.push(arkenstone_index);
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("order the end-step triggers");
    assert_eq!(
        runner.state().stack.len(),
        2,
        "both triggers are on the stack"
    );
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    let library_before = library_len(&runner);

    // One player action resolves both triggers.
    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        })
        .expect("resolve the stack in one stack-resolution session");
    assert_stack_empty_priority(&runner);
    assert_eq!(runner.state().phase, Phase::End);

    // Reach guards: The Arkenstone's draw resolved (one card from the top) and
    // the delayed trigger fired.
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before + 1,
        "The Arkenstone's trigger drew a card"
    );
    assert_zone(&runner, &[miss1], Zone::Hand, "the drawn top card");
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "the one-shot delayed trigger must have fired"
    );

    // "that many" is 0: no creature card enters.
    assert_zone(
        &runner,
        &[creature_a, creature_b, creature_c, miss2, bottom_marker],
        Zone::Library,
        "every card left in the library",
    );
    assert_eq!(library_len(&runner), library_before - 1);
}

fn add_mana(runner: &mut GameRunner, color: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(color, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// CR 701.20a + CR 608.2c: Old Stickfingers' "Put all creature cards revealed
/// this way into your graveyard" is the matched set's disposition — the X
/// creature cards go straight to the graveyard (never through the hand), and
/// the rest go to the bottom of the library in a random order.
#[test]
fn old_stickfingers_puts_revealed_creature_cards_into_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (miss1, creature_a, miss2, creature_b, creature_c, bottom_marker) =
        stage_library(&mut scenario, None);
    let stickfingers = scenario
        .add_creature_to_hand_from_oracle(P0, "Old Stickfingers", 0, 0, OLD_STICKFINGERS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    set_library_creature_pt(&mut runner, &[creature_a, creature_b, creature_c]);
    add_mana(&mut runner, ManaType::Colorless, 2);
    add_mana(&mut runner, ManaType::Black, 1);
    add_mana(&mut runner, ManaType::Green, 1);
    let initial_library = library_len(&runner);

    runner.cast(stickfingers).x(2).resolve();
    runner.advance_until_stack_empty();
    assert_stack_empty_priority(&runner);

    // Reach guards: X = 2 bound, both creature cards left the library.
    assert_zone(
        &runner,
        &[creature_a, creature_b],
        Zone::Graveyard,
        "the two revealed creature cards",
    );
    assert_zone(
        &runner,
        &[creature_c, miss1, miss2, bottom_marker],
        Zone::Library,
        "the unrevealed creature card and the misses",
    );
    assert_eq!(library_len(&runner), initial_library - 2);
    // The misses go to the bottom (random order between them).
    let library = &runner.state().players[P0.0 as usize].library;
    let bottom_two: Vec<ObjectId> = library.iter().rev().take(2).copied().collect();
    assert!(
        bottom_two.contains(&miss1) && bottom_two.contains(&miss2),
        "the revealed misses must be on the bottom; library={library:?}"
    );

    // Discriminating assertion: the creature cards never passed through hand.
    let through_hand: Vec<ObjectId> = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .filter(|record| record.to_zone == Zone::Hand)
        .map(|record| record.object_id)
        .filter(|id| [creature_a, creature_b].contains(id))
        .collect();
    assert!(
        through_hand.is_empty(),
        "the revealed creature cards must go straight to the graveyard, not via hand: {through_hand:?}"
    );
}
