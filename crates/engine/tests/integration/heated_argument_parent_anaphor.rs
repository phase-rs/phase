//! CR 608.2c + CR 115.10a + CR 701.23a: a definite anaphor ("that creature",
//! "that creature's controller") read after a resolution-time object choice
//! names the object an earlier instruction announced as its target, not the
//! object the choice selected (phase-rs/phase#10205, Heated Argument).
//!
//! Heated Argument is the verbatim printed card. Every other spell text here is
//! a synthetic building-block fixture for one axis of the class (owner and
//! object projections, a library search, the choice selection as antecedent,
//! fail-closed shapes); none claims support for a printed card.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, BounceSelection, ControllerRef, Effect, TargetFilter, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text (Scryfall).
const HEATED_ARGUMENT: &str = "Heated Argument deals 6 damage to target creature. You may exile a card from your graveyard. If you do, Heated Argument also deals 2 damage to that creature's controller.";

/// Building-block fixture: owner projection across a graveyard exile.
const OWNER_PROBE: &str = "Owner Probe deals 6 damage to target creature. You may exile a card from your graveyard. If you do, that creature's owner loses 2 life.";

/// Building-block fixture: object projection across a graveyard exile.
const STUN_PROBE: &str = "Tap target creature. You may exile a card from your graveyard. If you do, put a stun counter on that creature.";

/// Building-block fixture: an ungated reader after an optional graveyard exile.
const UNGATED_STUN_PROBE: &str = "Tap target creature. You may exile a card from your graveyard. Put a stun counter on that creature.";

/// Building-block fixture: an intervening sacrifice is not a zone choice.
const SACRIFICE_PROBE: &str = "Sacrifice Probe deals 6 damage to target creature. You may sacrifice an artifact. If you do, Sacrifice Probe also deals 2 damage to that creature's controller.";

/// Building-block fixture: the put creature is the anaphor's antecedent.
const HAND_PUT_PROBE: &str = "Tap target creature. You may put a creature card from your hand onto the battlefield. If you do, put a +1/+1 counter on that creature.";

/// Building-block fixture: a library search between the target and the anaphor.
const SEARCH_PROBE: &str = "Search Probe deals 6 damage to target creature. Search your library for a card, put it into your hand, then shuffle. Search Probe deals 2 damage to that creature's controller.";

/// Building-block fixture: an optional library search gated by "If you do".
const OPTIONAL_SEARCH_PROBE: &str = "Search Probe deals 6 damage to target creature. You may search your library for a card, put it into your hand, then shuffle. If you do, Search Probe deals 2 damage to that creature's controller.";

/// Building-block fixture: the found creature is the anaphor's antecedent.
const SEARCH_BATTLEFIELD_PROBE: &str = "Tap target creature. Search your library for a creature card, put it onto the battlefield, then shuffle. Put a +1/+1 counter on that creature.";

const GAP: &str = "anaphor_shadowed_by_resolution_choice";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn parse_instant(text: &str, name: &str) -> AbilityDefinition {
    let parsed = parse_oracle_text(text, name, &[], &["Instant".to_string()], &[]);
    parsed
        .abilities
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{name} must parse a spell ability"))
}

fn chain_nodes(root: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    std::iter::successors(Some(root), |def| def.sub_ability.as_deref()).collect()
}

fn assert_no_chosen_group_link(root: &AbilityDefinition) {
    for node in chain_nodes(root) {
        assert_eq!(node.reads_chosen_group, None, "{:?}", node.effect);
        assert_eq!(node.declares_chosen_group, None, "{:?}", node.effect);
    }
}

fn is_gap(def: &AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { name, .. } if name == GAP)
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn scenario(players: u8) -> GameScenario {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn add_instant(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, true, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// Cast `spell` at `victim` and stop at the optional instruction's prompt.
fn cast_to_optional(runner: &mut GameRunner, spell: ObjectId, victim: ObjectId) {
    let stopped = runner
        .cast(spell)
        .target_object(victim)
        .commit()
        .stop_at_optional()
        .resolve();
    assert!(
        matches!(
            stopped.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "CR 608.2d: the caster is asked the optional instruction, got {:?}",
        stopped.final_waiting_for()
    );
}

/// Cast `spell` at `victim` and stop at the library search's prompt.
fn cast_to_search(runner: &mut GameRunner, spell: ObjectId, victim: ObjectId) {
    let stopped = runner
        .cast(spell)
        .target_object(victim)
        .commit()
        .stop_at_optional()
        .resolve();
    assert!(
        matches!(
            stopped.final_waiting_for(),
            WaitingFor::SearchChoice { player: P0, .. }
        ),
        "CR 701.23a: the caster searches, got {:?}",
        stopped.final_waiting_for()
    );
}

fn decide_optional(runner: &mut GameRunner, accept: bool) {
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("optional decision accepted");
}

/// Answer the resolution-time zone choice (CR 608.2d) with `chosen`.
fn choose_zone_card(runner: &mut GameRunner, chosen: ObjectId) {
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the caster makes the zone choice");
            assert!(cards.contains(&chosen), "{chosen:?} is offered: {cards:?}");
        }
        other => panic!("expected the zone-choice prompt, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("zone choice accepted");
}

/// Answer the library search (CR 701.23a) with `chosen`.
fn choose_search_card(runner: &mut GameRunner, chosen: ObjectId) {
    match &runner.state().waiting_for {
        WaitingFor::SearchChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the caster searches");
            assert!(cards.contains(&chosen), "{chosen:?} is offered: {cards:?}");
        }
        other => panic!("expected the search prompt, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("search choice accepted");
}

/// Completion boundary: the spell finished resolving.
fn assert_resolved(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution completed to a priority window, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "the stack is empty");
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn counters(runner: &GameRunner, id: ObjectId, counter: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&counter)
        .copied()
        .unwrap_or(0)
}

/// A graveyard-exile fixture: P0 holds `spell`, P0's graveyard holds two cards
/// (so the zone choice is presented), and `victim` is a creature owned by
/// `owner` and controlled by `controller`.
struct GraveyardExileBoard {
    runner: GameRunner,
    spell: ObjectId,
    victim: ObjectId,
    chosen: ObjectId,
    kept: ObjectId,
}

fn graveyard_exile_board(
    players: u8,
    name: &str,
    text: &str,
    owner: PlayerId,
    controller: PlayerId,
    toughness: i32,
) -> GraveyardExileBoard {
    let mut scenario = scenario(players);
    let spell = add_instant(&mut scenario, name, text);
    let victim = scenario
        .add_creature(owner, "Victim", toughness, toughness)
        .controlled_by(controller)
        .id();
    let chosen = scenario
        .add_creature_to_graveyard(P0, "Exiled Memory", 1, 1)
        .id();
    let kept = scenario
        .add_creature_to_graveyard(P0, "Kept Memory", 1, 1)
        .id();
    GraveyardExileBoard {
        runner: scenario.build(),
        spell,
        victim,
        chosen,
        kept,
    }
}

// ---------------------------------------------------------------------------
// Row 1-4: verbatim Heated Argument
// ---------------------------------------------------------------------------

/// Row 1 (red at BASE: P0 -2, P1 0). CR 608.2c: "that creature's controller" is
/// the controller of the creature targeted by the first sentence; the exiled
/// card is chosen while the spell resolves and is not a target (CR 115.10a).
#[test]
fn heated_argument_exile_damages_target_creatures_controller() {
    let mut board = graveyard_exile_board(2, "Heated Argument", HEATED_ARGUMENT, P1, P1, 8);
    let runner = &mut board.runner;
    let (p0_before, p1_before) = (life(runner, P0), life(runner, P1));
    cast_to_optional(runner, board.spell, board.victim);
    decide_optional(runner, true);
    choose_zone_card(runner, board.chosen);
    assert_resolved(runner);

    assert_eq!(runner.state().objects[&board.victim].damage_marked, 6);
    assert_eq!(zone_of(runner, board.chosen), Zone::Exile);
    assert_eq!(zone_of(runner, board.kept), Zone::Graveyard);
    assert_eq!(
        life(runner, P1) - p1_before,
        -2,
        "the target creature's controller takes 2"
    );
    assert_eq!(life(runner, P0) - p0_before, 0, "the caster takes nothing");

    // The parser link that carries the binding: the targeting instruction
    // declares the group the anaphor's instruction reads.
    let root = parse_instant(HEATED_ARGUMENT, "Heated Argument");
    let group = root
        .declares_chosen_group
        .expect("the targeting instruction declares the anaphor's group");
    assert_eq!(
        chain_nodes(&root).last().unwrap().reads_chosen_group,
        Some(group)
    );
}

/// Row 2. CR 608.2c: declining makes "If you do" false, so no 2 damage.
#[test]
fn heated_argument_decline_deals_no_controller_damage() {
    let mut board = graveyard_exile_board(2, "Heated Argument", HEATED_ARGUMENT, P1, P1, 8);
    let runner = &mut board.runner;
    let (p0_before, p1_before) = (life(runner, P0), life(runner, P1));
    cast_to_optional(runner, board.spell, board.victim);
    decide_optional(runner, false);
    assert_resolved(runner);

    assert_eq!(runner.state().objects[&board.victim].damage_marked, 6);
    assert_eq!(zone_of(runner, board.chosen), Zone::Graveyard);
    assert_eq!(zone_of(runner, board.kept), Zone::Graveyard);
    assert_eq!(life(runner, P1) - p1_before, 0);
    assert_eq!(life(runner, P0) - p0_before, 0);
}

/// Row 3 (red at BASE: P0 -2). CR 109.4: the creature's controller (P1), not its
/// owner (P2) nor the caster (P0), takes the 2 damage.
#[test]
fn heated_argument_controller_not_owner_three_players() {
    let mut board = graveyard_exile_board(3, "Heated Argument", HEATED_ARGUMENT, P2, P1, 8);
    let runner = &mut board.runner;
    let before = [life(runner, P0), life(runner, P1), life(runner, P2)];
    cast_to_optional(runner, board.spell, board.victim);
    decide_optional(runner, true);
    choose_zone_card(runner, board.chosen);
    assert_resolved(runner);

    assert_eq!(runner.state().objects[&board.victim].damage_marked, 6);
    assert_eq!(zone_of(runner, board.chosen), Zone::Exile);
    assert_eq!(life(runner, P1) - before[1], -2, "controller takes 2");
    assert_eq!(life(runner, P2) - before[2], 0, "owner takes nothing");
    assert_eq!(life(runner, P0) - before[0], 0, "caster takes nothing");
}

/// Row 4 (red at BASE). CR 704.3: the 6 damage is lethal, but state-based
/// actions are checked only after the spell resolves, so the binding still
/// names the creature's controller; the creature then dies.
#[test]
fn heated_argument_lethal_target_still_names_its_controller() {
    let mut board = graveyard_exile_board(2, "Heated Argument", HEATED_ARGUMENT, P1, P1, 2);
    let runner = &mut board.runner;
    let (p0_before, p1_before) = (life(runner, P0), life(runner, P1));
    cast_to_optional(runner, board.spell, board.victim);
    decide_optional(runner, true);
    choose_zone_card(runner, board.chosen);
    assert_resolved(runner);

    assert_eq!(zone_of(runner, board.victim), Zone::Graveyard);
    assert_eq!(zone_of(runner, board.chosen), Zone::Exile);
    assert_eq!(life(runner, P1) - p1_before, -2);
    assert_eq!(life(runner, P0) - p0_before, 0);
}

// ---------------------------------------------------------------------------
// Rows 5-7: projections and the no-choice sibling
// ---------------------------------------------------------------------------

/// Row 5 (synthetic). CR 108.3: "that creature's owner" across the exile reads
/// the target creature's owner (P2), not the controller (P1) or the caster.
#[test]
fn owner_anaphor_across_graveyard_exile_reads_target_owner() {
    let mut board = graveyard_exile_board(3, "Owner Probe", OWNER_PROBE, P2, P1, 8);
    let runner = &mut board.runner;
    let before = [life(runner, P0), life(runner, P1), life(runner, P2)];
    cast_to_optional(runner, board.spell, board.victim);
    decide_optional(runner, true);
    choose_zone_card(runner, board.chosen);
    assert_resolved(runner);

    assert_eq!(runner.state().objects[&board.victim].damage_marked, 6);
    assert_eq!(zone_of(runner, board.chosen), Zone::Exile);
    assert_eq!(life(runner, P2) - before[2], -2, "owner loses 2");
    assert_eq!(life(runner, P1) - before[1], 0);
    assert_eq!(life(runner, P0) - before[0], 0);
}

/// Row 6 (synthetic). CR 608.2c: "that creature" across the exile is the tapped
/// target, which gets the stun counter.
#[test]
fn object_anaphor_across_graveyard_exile_reads_target() {
    let mut board = graveyard_exile_board(2, "Stun Probe", STUN_PROBE, P1, P1, 8);
    let runner = &mut board.runner;
    cast_to_optional(runner, board.spell, board.victim);
    decide_optional(runner, true);
    choose_zone_card(runner, board.chosen);
    assert_resolved(runner);

    assert!(runner.state().objects[&board.victim].tapped);
    assert_eq!(zone_of(runner, board.chosen), Zone::Exile);
    assert_eq!(counters(runner, board.victim, CounterType::Stun), 1);
    assert_eq!(counters(runner, board.chosen, CounterType::Stun), 0);
}

/// Row 6b (synthetic). CR 608.2c: an instruction printed without "If you do"
/// after the optional exile happens whether the exile is accepted or declined,
/// and "that creature" is the tapped target either way.
#[test]
fn unconditional_object_anaphor_after_optional_exile_reads_target() {
    for accept in [true, false] {
        let mut board = graveyard_exile_board(2, "Stun Probe", UNGATED_STUN_PROBE, P1, P1, 8);
        let runner = &mut board.runner;
        cast_to_optional(runner, board.spell, board.victim);
        decide_optional(runner, accept);
        if accept {
            choose_zone_card(runner, board.chosen);
        }
        assert_resolved(runner);

        assert!(runner.state().objects[&board.victim].tapped);
        let expected_zone = if accept { Zone::Exile } else { Zone::Graveyard };
        assert_eq!(zone_of(runner, board.chosen), expected_zone);
        assert_eq!(
            counters(runner, board.victim, CounterType::Stun),
            1,
            "accept={accept}"
        );
        assert_eq!(counters(runner, board.chosen, CounterType::Stun), 0);
    }
}

/// Row 7 (synthetic). A sacrifice is not a resolution-time zone choice this
/// binder reads; the chain carries no chosen-group link and its reader still
/// names the target creature's controller.
#[test]
fn sacrifice_between_target_and_controller_anaphor_is_unchanged() {
    let root = parse_instant(SACRIFICE_PROBE, "Sacrifice Probe");
    let nodes = chain_nodes(&root);
    assert!(
        matches!(nodes[1].effect.as_ref(), Effect::Sacrifice { .. }),
        "{:?}",
        nodes[1].effect
    );
    assert!(
        matches!(
            nodes[2].effect.as_ref(),
            Effect::DealDamage {
                target: TargetFilter::ParentTargetController,
                ..
            }
        ),
        "{:?}",
        nodes[2].effect
    );
    assert_no_chosen_group_link(&root);

    let mut scenario = scenario(2);
    let spell = add_instant(&mut scenario, "Sacrifice Probe", SACRIFICE_PROBE);
    let victim = scenario.add_creature(P1, "Victim", 8, 8).id();
    scenario.add_artifact_from_oracle(P0, "Relic", "");
    let mut runner = scenario.build();
    let (p0_before, p1_before) = (life(&runner, P0), life(&runner, P1));
    cast_to_optional(&mut runner, spell, victim);
    decide_optional(&mut runner, true);
    assert_resolved(&runner);

    assert_eq!(runner.state().objects[&victim].damage_marked, 6);
    assert_eq!(life(&runner, P1) - p1_before, -2);
    assert_eq!(life(&runner, P0) - p0_before, 0);
}

// ---------------------------------------------------------------------------
// Row 8: the zone choice's result is the antecedent
// ---------------------------------------------------------------------------

/// Row 8 (synthetic). CR 110.1 + CR 608.2c: "that creature" names the creature
/// card just put onto the battlefield, so the binder must decline (the
/// `selection_could_be_anaphor` conjunct is the only one that refuses here).
#[test]
fn hand_to_battlefield_choice_names_that_creature() {
    let root = parse_instant(HAND_PUT_PROBE, "Hand Probe");
    let nodes = chain_nodes(&root);
    assert!(
        matches!(
            nodes[1].effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                ..
            }
        ),
        "{:?}",
        nodes[1].effect
    );
    assert!(
        matches!(
            nodes[2].effect.as_ref(),
            Effect::PutCounter {
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "{:?}",
        nodes[2].effect
    );

    let mut scenario = scenario(2);
    let spell = add_instant(&mut scenario, "Hand Probe", HAND_PUT_PROBE);
    let victim = scenario.add_creature(P1, "Victim", 8, 8).id();
    let hand_bear = scenario.add_creature_to_hand(P0, "Hand Bear", 2, 2).id();
    let hand_wolf = scenario.add_creature_to_hand(P0, "Hand Wolf", 2, 2).id();
    let mut runner = scenario.build();
    cast_to_optional(&mut runner, spell, victim);
    decide_optional(&mut runner, true);
    choose_zone_card(&mut runner, hand_bear);
    assert_resolved(&runner);

    assert!(runner.state().objects[&victim].tapped);
    assert_eq!(zone_of(&runner, hand_bear), Zone::Battlefield);
    assert_eq!(zone_of(&runner, hand_wolf), Zone::Hand);
    assert_eq!(counters(&runner, hand_bear, CounterType::Plus1Plus1), 1);
    assert_eq!(counters(&runner, victim, CounterType::Plus1Plus1), 0);
    assert_no_chosen_group_link(&root);
}

// ---------------------------------------------------------------------------
// Rows 9-11: fail-closed and no-producer shapes (parser)
// ---------------------------------------------------------------------------

/// Row 9 (synthetic). CR 601.2c: two announced creatures both answer "that
/// creature", so the reader fails closed instead of guessing.
#[test]
fn ambiguous_producers_across_resolution_choice_fail_closed() {
    let root = parse_instant(
        "Tap target creature. Tap target creature. You may exile a card from your graveyard. If you do, that creature's controller loses 2 life.",
        "Ambiguity Probe",
    );
    let nodes = chain_nodes(&root);
    assert!(matches!(
        nodes[0].effect.as_ref(),
        Effect::SetTapState { .. }
    ));
    assert!(matches!(
        nodes[1].effect.as_ref(),
        Effect::SetTapState { .. }
    ));
    assert!(matches!(
        nodes[2].effect.as_ref(),
        Effect::ChangeZone { .. }
    ));
    assert!(
        is_gap(nodes.last().unwrap()),
        "{:?}",
        nodes.last().unwrap().effect
    );
}

/// Row 10 (synthetic). CR 608.2b: with more than one announced target the
/// reader does not bind (sole-slot rule) and fails closed.
#[test]
fn multi_slot_chain_across_resolution_choice_fails_closed() {
    let root = parse_instant(
        "Destroy target artifact. Tap target creature. You may exile a card from your graveyard. If you do, that creature's controller loses 2 life.",
        "Multi Probe",
    );
    let nodes = chain_nodes(&root);
    assert!(matches!(nodes[0].effect.as_ref(), Effect::Destroy { .. }));
    assert!(matches!(
        nodes[1].effect.as_ref(),
        Effect::SetTapState { .. }
    ));
    assert!(matches!(
        nodes[2].effect.as_ref(),
        Effect::ChangeZone { .. }
    ));
    assert!(
        is_gap(nodes.last().unwrap()),
        "{:?}",
        nodes.last().unwrap().effect
    );
}

/// Row 11 (synthetic). No earlier instruction announces a creature, so the
/// binder leaves the chain unchanged.
#[test]
fn resolution_choice_without_declared_producer_is_unchanged() {
    let root = parse_instant(
        "You may exile a card from your graveyard. If you do, that creature's controller loses 2 life.",
        "Orphan Probe",
    );
    let nodes = chain_nodes(&root);
    assert!(matches!(
        nodes[0].effect.as_ref(),
        Effect::ChangeZone { .. }
    ));
    let reader = nodes.last().unwrap();
    assert!(
        matches!(reader.effect.as_ref(), Effect::LoseLife { .. }),
        "{:?}",
        reader.effect
    );
    assert!(!is_gap(reader));
    for node in nodes {
        assert_eq!(node.reads_chosen_group, None, "{:?}", node.effect);
    }
}

// ---------------------------------------------------------------------------
// Rows 13-16: library search
// ---------------------------------------------------------------------------

/// Library: `found` on top of non-creature filler, so the search offers several
/// cards.
fn stage_library(scenario: &mut GameScenario, found: &str, found_is_creature: bool) -> ObjectId {
    scenario.with_library_top(P0, &["Filler 1", "Filler 2", "Filler 3"]);
    let mut builder = scenario.add_spell_to_library_top(P0, found, false);
    if found_is_creature {
        builder.as_creature();
    }
    builder.id()
}

/// Row 13 (synthetic; red at BASE: P0 -2). CR 701.23a + CR 115.10a: the found
/// card is not a target; "that creature's controller" is the target
/// creature's controller.
#[test]
fn search_between_target_and_controller_anaphor_reads_target() {
    let mut scenario = scenario(2);
    let spell = add_instant(&mut scenario, "Search Probe", SEARCH_PROBE);
    let victim = scenario.add_creature(P1, "Victim", 8, 8).id();
    let lib_card = stage_library(&mut scenario, "Lib Card", false);
    let mut runner = scenario.build();
    let (p0_before, p1_before) = (life(&runner, P0), life(&runner, P1));
    cast_to_search(&mut runner, spell, victim);
    choose_search_card(&mut runner, lib_card);
    assert_resolved(&runner);

    assert_eq!(runner.state().objects[&victim].damage_marked, 6);
    assert_eq!(zone_of(&runner, lib_card), Zone::Hand);
    assert_eq!(life(&runner, P1) - p1_before, -2);
    assert_eq!(life(&runner, P0) - p0_before, 0);

    let root = parse_instant(SEARCH_PROBE, "Search Probe");
    let group = root
        .declares_chosen_group
        .expect("the targeting instruction declares the anaphor's group");
    assert_eq!(
        chain_nodes(&root).last().unwrap().reads_chosen_group,
        Some(group)
    );
}

/// Row 14 (synthetic). CR 608.2c: declining the search makes "If you do" false.
#[test]
fn optional_search_decline_deals_no_controller_damage() {
    let mut scenario = scenario(2);
    let spell = add_instant(&mut scenario, "Search Probe", OPTIONAL_SEARCH_PROBE);
    let victim = scenario.add_creature(P1, "Victim", 8, 8).id();
    let lib_card = stage_library(&mut scenario, "Lib Card", false);
    let mut runner = scenario.build();
    let (p0_before, p1_before) = (life(&runner, P0), life(&runner, P1));
    cast_to_optional(&mut runner, spell, victim);
    decide_optional(&mut runner, false);
    assert_resolved(&runner);

    assert_eq!(runner.state().objects[&victim].damage_marked, 6);
    assert_eq!(zone_of(&runner, lib_card), Zone::Library);
    assert_eq!(life(&runner, P1) - p1_before, 0);
    assert_eq!(life(&runner, P0) - p0_before, 0);
}

/// Row 15 (synthetic). CR 110.1 + CR 608.2c: "that creature" names the creature
/// card the search put onto the battlefield, so the binder must decline.
#[test]
fn search_to_battlefield_names_that_creature() {
    let mut scenario = scenario(2);
    let spell = add_instant(&mut scenario, "Search Bf Probe", SEARCH_BATTLEFIELD_PROBE);
    let victim = scenario.add_creature(P1, "Victim", 8, 8).id();
    let lib_bear = stage_library(&mut scenario, "Lib Bear", true);
    let mut runner = scenario.build();
    cast_to_search(&mut runner, spell, victim);
    choose_search_card(&mut runner, lib_bear);
    assert_resolved(&runner);

    assert!(runner.state().objects[&victim].tapped);
    assert_eq!(zone_of(&runner, lib_bear), Zone::Battlefield);
    assert_eq!(counters(&runner, lib_bear, CounterType::Plus1Plus1), 1);
    assert_eq!(counters(&runner, victim, CounterType::Plus1Plus1), 0);

    let root = parse_instant(SEARCH_BATTLEFIELD_PROBE, "Search Bf Probe");
    for node in chain_nodes(&root) {
        assert_eq!(node.reads_chosen_group, None, "{:?}", node.effect);
    }
}

/// Row 16 (synthetic). CR 115.1 + CR 608.2b: a search that announces its own
/// player target leaves the chain with two announced slots, so the reader fails
/// closed; the search's declared-player group is untouched.
#[test]
fn search_announcing_player_target_fails_closed() {
    let root = parse_instant(
        "Rival Probe deals 6 damage to target creature. Search target opponent's library for a card and exile it. Then that player shuffles. Rival Probe deals 2 damage to that creature's controller.",
        "Rival Probe",
    );
    let nodes = chain_nodes(&root);
    let Effect::SearchLibrary {
        target_player: Some(TargetFilter::Typed(player_filter)),
        ..
    } = nodes[1].effect.as_ref()
    else {
        panic!("[1] is the targeted search: {:?}", nodes[1].effect);
    };
    assert_eq!(player_filter.controller, Some(ControllerRef::Opponent));
    let player_group = nodes[1]
        .declares_chosen_group
        .expect("the search declares its player group");
    let shuffle = nodes
        .iter()
        .find(|node| matches!(node.effect.as_ref(), Effect::Shuffle { .. }))
        .expect("the shuffle instruction parses");
    assert!(
        matches!(
            shuffle.effect.as_ref(),
            Effect::Shuffle { target: TargetFilter::DeclaredPlayer { group } } if *group == player_group
        ),
        "{:?}",
        shuffle.effect
    );
    assert!(
        is_gap(nodes.last().unwrap()),
        "{:?}",
        nodes.last().unwrap().effect
    );
    for node in &nodes {
        assert_eq!(node.reads_chosen_group, None, "{:?}", node.effect);
    }
}

// ---------------------------------------------------------------------------
// Rows 17-18: single-candidate zone choice; a farther choice as antecedent
// ---------------------------------------------------------------------------

/// Row 17 (verbatim). CR 608.2d: with one card in the graveyard, accepting the
/// exile moves that card without a zone-choice prompt, and the 2 damage still
/// goes to the target creature's controller.
#[test]
fn heated_argument_single_graveyard_card_exiles_without_prompt() {
    let mut scenario = scenario(2);
    let spell = add_instant(&mut scenario, "Heated Argument", HEATED_ARGUMENT);
    let victim = scenario.add_creature(P1, "Victim", 8, 8).id();
    let only = scenario
        .add_creature_to_graveyard(P0, "Only Memory", 1, 1)
        .id();
    let mut runner = scenario.build();
    let (p0_before, p1_before) = (life(&runner, P0), life(&runner, P1));
    cast_to_optional(&mut runner, spell, victim);
    decide_optional(&mut runner, true);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "a single eligible card is moved without a prompt"
    );
    assert_resolved(&runner);

    assert_eq!(runner.state().objects[&victim].damage_marked, 6);
    assert_eq!(zone_of(&runner, only), Zone::Exile);
    assert_eq!(
        life(&runner, P1) - p1_before,
        -2,
        "the target creature's controller takes 2"
    );
    assert_eq!(life(&runner, P0) - p0_before, 0, "the caster takes nothing");
}

/// Row 18 (synthetic). CR 110.1 + CR 608.2c: "that creature" names the creature
/// an earlier resolution-time choice put onto the battlefield, even when a
/// nearer choice (the graveyard exile) cannot be a creature on the battlefield.
/// Every choice between the announced target and the reader is checked, so the
/// binder declines and no chosen-group link is written.
#[test]
fn farther_choice_to_battlefield_names_that_creature() {
    for (text, label) in [
        (
            "Tap target creature. Search your library for a creature card, put it onto the battlefield, then shuffle. You may exile a card from your graveyard. If you do, put a +1/+1 counter on that creature.",
            "search",
        ),
        (
            "Tap target creature. You may put a creature card from your hand onto the battlefield. Exile a card from your graveyard. Put a +1/+1 counter on that creature.",
            "hand put",
        ),
    ] {
        let root = parse_instant(text, "Farther Probe");
        let nodes = chain_nodes(&root);
        assert!(
            matches!(nodes[0].effect.as_ref(), Effect::SetTapState { .. }),
            "{label}: the producer announces a target: {:?}",
            nodes[0].effect
        );
        assert!(
            nodes.iter().any(|node| matches!(
                node.effect.as_ref(),
                Effect::SearchLibrary { .. }
                    | Effect::ChangeZone {
                        origin: Some(Zone::Hand),
                        destination: Zone::Battlefield,
                        ..
                    }
            )),
            "{label}: the battlefield choice parses"
        );
        assert!(
            nodes.iter().any(|node| matches!(
                node.effect.as_ref(),
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    ..
                }
            )),
            "{label}: the graveyard exile parses"
        );
        let reader = nodes.last().unwrap();
        assert!(
            matches!(
                reader.effect.as_ref(),
                Effect::PutCounter {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            ),
            "{label}: {:?}",
            reader.effect
        );
        assert_no_chosen_group_link(&root);
    }
}

// ---------------------------------------------------------------------------
// Row 19: a choice inside the producer's own compound clause
// ---------------------------------------------------------------------------

/// Row 19 (synthetic). CR 110.1 + CR 608.2c: a resolution-time choice joined to
/// the producer's own instruction by "and" is checked like one printed as its
/// own sentence (row 18). The creature it puts onto the battlefield can be
/// "that creature", so the binder declines and no chosen-group link is written.
#[test]
fn compound_choice_in_producer_clause_names_that_creature() {
    let root = parse_instant(
        "Tap target creature and put a creature card from your hand onto the battlefield. You may exile a card from your graveyard. If you do, put a +1/+1 counter on that creature.",
        "Compound Probe",
    );
    let nodes = chain_nodes(&root);
    assert!(
        matches!(nodes[0].effect.as_ref(), Effect::SetTapState { .. }),
        "the producer announces a target: {:?}",
        nodes[0].effect
    );
    assert!(
        nodes.iter().any(|node| matches!(
            node.effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                ..
            }
        )),
        "the compound hand-to-battlefield choice parses"
    );
    assert!(
        nodes.iter().any(|node| matches!(
            node.effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                ..
            }
        )),
        "the graveyard exile parses"
    );
    let reader = nodes.last().unwrap();
    assert!(
        matches!(
            reader.effect.as_ref(),
            Effect::PutCounter {
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "{:?}",
        reader.effect
    );
    assert_no_chosen_group_link(&root);
}

// ---------------------------------------------------------------------------
// Rows 20-21: a selection whose card type could coexist with the noun's
// ---------------------------------------------------------------------------

/// Building-block fixture: a permanent card put onto the battlefield can be a
/// creature.
const PERMANENT_PUT_PROBE: &str = "Tap target creature. You may put a permanent card from your graveyard onto the battlefield. If you do, put a +1/+1 counter on that creature.";

/// Building-block fixture: an artifact card put onto the battlefield can be an
/// artifact creature.
const ARTIFACT_PUT_PROBE: &str = "Tap target creature. You may put an artifact card from your graveyard onto the battlefield. If you do, put a +1/+1 counter on that creature.";

/// Parser shape shared by rows 20-21: the producer taps a target, the choice
/// puts a graveyard card whose filter is typed `selection_type` (never
/// `Creature`) onto the battlefield, and the reader reads its parent target.
/// CR 205.2b: such a card can still be a creature, so the binder declines and
/// no chosen-group link is written.
fn assert_overlapping_selection_declines(text: &str, selection_type: TypeFilter) {
    let root = parse_instant(text, "Overlap Probe");
    let nodes = chain_nodes(&root);
    assert!(
        matches!(nodes[0].effect.as_ref(), Effect::SetTapState { .. }),
        "the producer announces a target: {:?}",
        nodes[0].effect
    );
    assert!(
        matches!(
            nodes[1].effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(tf),
                ..
            } if tf.type_filters.contains(&selection_type)
                && !tf.type_filters.contains(&TypeFilter::Creature)
        ),
        "the {selection_type:?} graveyard-to-battlefield choice parses: {:?}",
        nodes[1].effect
    );
    let reader = nodes.last().unwrap();
    assert!(
        matches!(
            reader.effect.as_ref(),
            Effect::PutCounter {
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "{:?}",
        reader.effect
    );
    assert_no_chosen_group_link(&root);
}

/// Row 20 (synthetic). CR 205.2b: a permanent card is not proven a noncreature
/// by its `Permanent` type, so "that creature" could be the put card.
#[test]
fn permanent_card_selection_could_be_that_creature() {
    assert_overlapping_selection_declines(PERMANENT_PUT_PROBE, TypeFilter::Permanent);
}

/// Row 21 (synthetic). CR 205.2b: an artifact card can be an artifact
/// creature, so "that creature" could be the put card.
#[test]
fn artifact_card_selection_could_be_that_creature() {
    assert_overlapping_selection_declines(ARTIFACT_PUT_PROBE, TypeFilter::Artifact);
}

/// Row 21 at runtime (synthetic). CR 205.2b + CR 608.2c: the artifact creature
/// card put onto the battlefield is "that creature" and gets the counter; the
/// tapped target does not.
#[test]
fn artifact_creature_put_from_graveyard_gets_the_counter() {
    let mut scenario = scenario(2);
    let spell = add_instant(&mut scenario, "Overlap Probe", ARTIFACT_PUT_PROBE);
    let victim = scenario.add_creature(P1, "Victim", 8, 8).id();
    let construct = scenario
        .add_creature_to_graveyard(P0, "Grave Construct", 2, 2)
        .as_artifact_creature()
        .id();
    let golem = scenario
        .add_creature_to_graveyard(P0, "Grave Golem", 2, 2)
        .as_artifact_creature()
        .id();
    let mut runner = scenario.build();
    cast_to_optional(&mut runner, spell, victim);
    decide_optional(&mut runner, true);
    choose_zone_card(&mut runner, construct);
    assert_resolved(&runner);

    assert!(runner.state().objects[&victim].tapped);
    assert_eq!(zone_of(&runner, construct), Zone::Battlefield);
    assert_eq!(zone_of(&runner, golem), Zone::Graveyard);
    assert_eq!(counters(&runner, construct, CounterType::Plus1Plus1), 1);
    assert_eq!(counters(&runner, victim, CounterType::Plus1Plus1), 0);
}

// ---------------------------------------------------------------------------
// Row 22: a selection whose filter explicitly excludes the noun
// ---------------------------------------------------------------------------

/// Building-block fixture: a noncreature permanent card cannot be "that
/// creature".
const NONCREATURE_PUT_PROBE: &str = "Tap target creature. You may put a noncreature permanent card from your graveyard onto the battlefield. If you do, put a +1/+1 counter on that creature.";

/// Row 22 (synthetic). CR 608.2c: a selection filtered as
/// noncreature explicitly excludes the noun, so even on the battlefield it is
/// not "that creature". The binder links the reader to the tapped target, which
/// gets the counter; the put artifact does not.
#[test]
fn noncreature_selection_to_battlefield_reads_target() {
    let root = parse_instant(NONCREATURE_PUT_PROBE, "Noncreature Probe");
    let nodes = chain_nodes(&root);
    assert!(
        matches!(
            nodes[1].effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(tf),
                ..
            } if tf.type_filters.contains(&TypeFilter::Non(Box::new(TypeFilter::Creature)))
        ),
        "the noncreature graveyard-to-battlefield choice parses: {:?}",
        nodes[1].effect
    );
    let group = root
        .declares_chosen_group
        .expect("the targeting instruction declares the anaphor's group");
    assert_eq!(nodes.last().unwrap().reads_chosen_group, Some(group));

    let mut scenario = scenario(2);
    let spell = add_instant(&mut scenario, "Noncreature Probe", NONCREATURE_PUT_PROBE);
    let victim = scenario.add_creature(P1, "Victim", 8, 8).id();
    let relic = scenario
        .add_creature_to_graveyard(P0, "Grave Relic", 0, 0)
        .as_artifact()
        .id();
    let idol = scenario
        .add_creature_to_graveyard(P0, "Grave Idol", 0, 0)
        .as_artifact()
        .id();
    let mut runner = scenario.build();
    cast_to_optional(&mut runner, spell, victim);
    decide_optional(&mut runner, true);
    choose_zone_card(&mut runner, relic);
    assert_resolved(&runner);

    assert!(runner.state().objects[&victim].tapped);
    assert_eq!(zone_of(&runner, relic), Zone::Battlefield);
    assert_eq!(zone_of(&runner, idol), Zone::Graveyard);
    assert_eq!(counters(&runner, victim, CounterType::Plus1Plus1), 1);
    assert_eq!(counters(&runner, relic, CounterType::Plus1Plus1), 0);
}

// ---------------------------------------------------------------------------
// Rows 23-24: an object selected outside a zone choice or library search
// ---------------------------------------------------------------------------

/// Parser shape shared by rows 23-24: the producer announces a creature target,
/// a graveyard exile makes a resolution-time zone choice, another instruction
/// selects an object without the word "target" (`is_selection` finds it), and
/// the reader reads its parent target. CR 115.10a + CR 608.2c: that selected
/// object can be the reader's "that creature", so the binder declines and no
/// chosen-group link is written.
fn assert_intervening_selection_declines(
    text: &str,
    label: &str,
    is_selection: fn(&Effect) -> bool,
    is_reader: fn(&Effect) -> bool,
) {
    let root = parse_instant(text, "Selection Probe");
    let nodes = chain_nodes(&root);
    assert!(
        matches!(
            nodes[0].effect.as_ref(),
            Effect::DealDamage {
                target: TargetFilter::Typed(_),
                ..
            } | Effect::SetTapState {
                target: TargetFilter::Typed(_),
                ..
            }
        ),
        "{label}: the producer announces a creature target: {:?}",
        nodes[0].effect
    );
    assert!(
        nodes.iter().any(|node| matches!(
            node.effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                ..
            }
        )),
        "{label}: the graveyard exile parses"
    );
    assert!(
        nodes.iter().any(|node| is_selection(node.effect.as_ref())),
        "{label}: the intervening selection parses"
    );
    let reader = nodes.last().unwrap();
    assert!(
        is_reader(reader.effect.as_ref()),
        "{label}: {:?}",
        reader.effect
    );
    assert_no_chosen_group_link(&root);
}

fn is_sacrifice(effect: &Effect) -> bool {
    matches!(effect, Effect::Sacrifice { .. })
}

fn is_controller_damage(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::DealDamage {
            target: TargetFilter::ParentTargetController,
            ..
        }
    )
}

fn is_parent_counter(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::PutCounter {
            target: TargetFilter::ParentTarget,
            ..
        }
    )
}

/// Row 23 (synthetic). CR 701.21a + CR 608.2c: a sacrificed creature can be
/// "that creature" (Grab the Reins: "Sacrifice a creature. Grab the Reins deals
/// damage equal to that creature's power to any target."), wherever the
/// sacrifice sits between the announced target and the reader.
#[test]
fn sacrifice_across_resolution_choice_could_be_that_creature() {
    for (text, label) in [
        (
            "Selection Probe deals 6 damage to target creature. Sacrifice a creature. You may exile a card from your graveyard. If you do, Selection Probe deals 2 damage to that creature's controller.",
            "sacrifice before the exile",
        ),
        (
            "Selection Probe deals 6 damage to target creature. You may exile a card from your graveyard. If you do, sacrifice a creature, then Selection Probe deals 2 damage to that creature's controller.",
            "sacrifice after the exile",
        ),
    ] {
        assert_intervening_selection_declines(text, label, is_sacrifice, is_controller_damage);
    }
}

/// Row 24 (synthetic). CR 115.10a + CR 608.2c: a creature returned without the
/// word "target" is chosen while the instruction is applied and can be "that
/// creature", before or after the exile.
#[test]
fn untargeted_return_across_resolution_choice_could_be_that_creature() {
    for (text, label) in [
        (
            "Tap target creature. Return a creature you control to its owner's hand. You may exile a card from your graveyard. If you do, put a stun counter on that creature.",
            "return before the exile",
        ),
        (
            "Tap target creature. You may exile a card from your graveyard. If you do, return a creature you control to its owner's hand. Put a stun counter on that creature.",
            "return after the exile",
        ),
    ] {
        assert_intervening_selection_declines(
            text,
            label,
            |effect| {
                matches!(
                    effect,
                    Effect::Bounce {
                        selection: BounceSelection::AtResolution,
                        ..
                    }
                )
            },
            is_parent_counter,
        );
    }
}

/// Row 24b (synthetic). CR 115.10a + CR 608.2d: the recipient of "put a +1/+1
/// counter on a creature you control" is chosen without the word "target" while
/// the instruction is applied, and can be "that creature". Before this row the
/// reader failed closed; the binder now declines.
#[test]
fn counter_recipient_choice_across_resolution_choice_could_be_that_creature() {
    assert_intervening_selection_declines(
        "Tap target creature. You may exile a card from your graveyard. If you do, put a +1/+1 counter on a creature you control. Put a stun counter on that creature.",
        "counter recipient",
        |effect| {
            matches!(
                effect,
                Effect::PutCounter {
                    target: TargetFilter::Typed(tf),
                    ..
                } if tf.type_filters.contains(&TypeFilter::Creature)
            )
        },
        is_parent_counter,
    );
}

// ---------------------------------------------------------------------------
// Row 25: the selection is moved again before the reader
// ---------------------------------------------------------------------------

/// Row 25 (synthetic). CR 110.1 + CR 608.2c: the exiled creature card is
/// returned to the battlefield before the reader, so it is a creature on the
/// battlefield when "that creature" is read and can be its referent. The zone
/// the exile delivered it to no longer proves its class; the binder declines
/// and no chosen-group link is written.
#[test]
fn selection_moved_after_choice_could_be_that_creature() {
    for (text, label) in [
        (
            "Tap target creature. You may exile a creature card from your graveyard, then return it to the battlefield. If you do, put a +1/+1 counter on that creature.",
            "then return it",
        ),
        (
            "Tap target creature. You may exile a creature card from your graveyard. If you do, return it to the battlefield. Put a +1/+1 counter on that creature.",
            "if you do, return it",
        ),
    ] {
        let root = parse_instant(text, "Return Probe");
        let nodes = chain_nodes(&root);
        assert!(
            matches!(nodes[0].effect.as_ref(), Effect::SetTapState { .. }),
            "{label}: the producer announces a target: {:?}",
            nodes[0].effect
        );
        assert!(
            matches!(
                nodes[1].effect.as_ref(),
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    target: TargetFilter::Typed(tf),
                    ..
                } if tf.type_filters.contains(&TypeFilter::Creature)
            ),
            "{label}: the graveyard exile parses: {:?}",
            nodes[1].effect
        );
        assert!(
            matches!(
                nodes[2].effect.as_ref(),
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Battlefield,
                    target: TargetFilter::ParentTarget | TargetFilter::TrackedSet { .. },
                    ..
                }
            ),
            "{label}: the exiled card is returned: {:?}",
            nodes[2].effect
        );
        let reader = nodes.last().unwrap();
        assert!(
            is_parent_counter(reader.effect.as_ref()),
            "{label}: {:?}",
            reader.effect
        );
        assert_no_chosen_group_link(&root);
    }
}

// ---------------------------------------------------------------------------
// Rows 26-27: an instruction that is not a recognised zone choice or search
// ---------------------------------------------------------------------------

/// Building-block fixture: a dig that may put a creature card onto the
/// battlefield, before the graveyard exile.
const DIG_PUT_PROBE: &str = "Tap target creature. Look at the top four cards of your library. You may put a creature card from among them onto the battlefield. Put the rest on the bottom of your library in a random order. You may exile a card from your graveyard. If you do, put a +1/+1 counter on that creature.";

/// Parser shape shared by rows 26-27: the producer taps a target creature, an
/// instruction that is neither a zone choice nor a library search
/// (`is_selection` finds it) may put a creature onto the battlefield, a
/// graveyard exile makes a resolution-time zone choice, and the reader reads its
/// parent target. CR 608.2c: the binder links a reader only when every
/// instruction between the target and the reader provably hands it no object
/// the noun names, so it declines here and no chosen-group link is written.
fn assert_unrecognised_selection_declines(
    text: &str,
    label: &str,
    is_selection: fn(&Effect) -> bool,
) {
    let root = parse_instant(text, "Selection Probe");
    let nodes = chain_nodes(&root);
    assert!(
        matches!(nodes[0].effect.as_ref(), Effect::SetTapState { .. }),
        "{label}: the producer announces a target: {:?}",
        nodes[0].effect
    );
    assert!(
        nodes.iter().any(|node| is_selection(node.effect.as_ref())),
        "{label}: the intervening instruction parses"
    );
    assert!(
        nodes.iter().any(|node| matches!(
            node.effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                ..
            }
        )),
        "{label}: the graveyard exile parses"
    );
    let reader = nodes.last().unwrap();
    assert!(
        is_parent_counter(reader.effect.as_ref()),
        "{label}: {:?}",
        reader.effect
    );
    assert_no_chosen_group_link(&root);
}

fn is_battlefield_dig(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Dig {
            destination: Some(Zone::Battlefield),
            filter: TargetFilter::Typed(tf),
            ..
        } if tf.type_filters.contains(&TypeFilter::Creature)
    )
}

/// Row 26 (synthetic). CR 608.2c + CR 110.1: the creature card a dig puts onto
/// the battlefield from among the looked-at cards can be "that creature". A dig
/// is not a zone choice or search the binder can rule out, so it declines.
#[test]
fn dig_to_battlefield_across_resolution_choice_could_be_that_creature() {
    assert_unrecognised_selection_declines(DIG_PUT_PROBE, "dig", is_battlefield_dig);
}

/// Row 27 (synthetic). CR 608.2c + CR 115.10a: a card revealed until a match
/// and put onto the battlefield, a card chosen from a hand and put onto the
/// battlefield, and a discovered card cast from exile can each be "that
/// creature". None is a zone choice or search the binder can rule out, so it
/// declines.
#[test]
fn unrecognised_selection_across_resolution_choice_could_be_that_creature() {
    for (text, label, is_selection) in [
        (
            "Tap target creature. Reveal cards from the top of your library until you reveal a creature card. Put that card onto the battlefield and the rest on the bottom of your library in a random order. You may exile a card from your graveyard. If you do, put a +1/+1 counter on that creature.",
            "reveal until",
            (|effect| {
                matches!(
                    effect,
                    Effect::RevealUntil {
                        kept_destination: Zone::Battlefield,
                        ..
                    }
                )
            }) as fn(&Effect) -> bool,
        ),
        (
            "Tap target creature. Choose a creature card in your hand. Put that card onto the battlefield. You may exile a card from your graveyard. If you do, put a +1/+1 counter on that creature.",
            "choose from hand",
            |effect| {
                matches!(
                    effect,
                    Effect::ChooseFromZone {
                        zone: Zone::Hand,
                        ..
                    }
                )
            },
        ),
        (
            "Tap target creature. Discover 3. You may exile a card from your graveyard. If you do, put a +1/+1 counter on that creature.",
            "discover",
            |effect| matches!(effect, Effect::Discover { .. }),
        ),
    ] {
        assert_unrecognised_selection_declines(text, label, is_selection);
    }
}

// ---------------------------------------------------------------------------
// Row 28: the reader clause reads its parent beyond the noun anaphor
// ---------------------------------------------------------------------------

/// Parser shape shared by rows 28 and 28b: the producer taps a target creature,
/// a graveyard exile makes a resolution-time zone choice, and the last node is
/// the reader (`is_reader`). CR 608.2c: the chosen-group link re-stamps every
/// parent-target read of the reader clause, so the binder links it only when
/// the noun anaphor is the clause's one parent read; here it is not, so the
/// binder declines and no chosen-group link is written.
fn assert_extra_parent_read_declines(text: &str, label: &str, is_reader: fn(&Effect) -> bool) {
    let root = parse_instant(text, "Pronoun Probe");
    let nodes = chain_nodes(&root);
    assert!(
        matches!(
            nodes[0].effect.as_ref(),
            Effect::SetTapState {
                target: TargetFilter::Typed(_),
                ..
            }
        ),
        "{label}: the producer announces a creature target: {:?}",
        nodes[0].effect
    );
    assert!(
        matches!(
            nodes[1].effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                ..
            }
        ),
        "{label}: the graveyard exile parses: {:?}",
        nodes[1].effect
    );
    let reader = nodes.last().unwrap();
    assert!(
        is_reader(reader.effect.as_ref()),
        "{label}: {:?}",
        reader.effect
    );
    assert_no_chosen_group_link(&root);
}

/// Row 28 (synthetic). CR 608.2c: a bare object pronoun in the reader clause
/// ("it") can name the card the exile selected, and re-stamping the clause
/// would point that read at the announced target too. Covers a pronoun subject
/// ("it deals", which the parsed node does not carry as a damage source), a
/// pronoun and the anaphor read by one node ("gains control of it": the pronoun
/// is its target, the anaphor its recipient), and a node reading its parent
/// through two target-filter slots ("attach it to that creature").
#[test]
fn bare_pronoun_in_reader_across_resolution_choice_declines() {
    for (text, label, is_reader) in [
        (
            "Tap target creature. You may exile a card from your graveyard. If you do, it deals 2 damage to that creature's controller.",
            "it deals",
            is_controller_damage as fn(&Effect) -> bool,
        ),
        (
            "Tap target creature. You may exile a card from your graveyard. If you do, that creature's controller gains control of it.",
            "gains control of it",
            |effect| {
                matches!(
                    effect,
                    Effect::GiveControl {
                        target: TargetFilter::ParentTarget,
                        recipient: TargetFilter::ParentTargetController,
                    }
                )
            },
        ),
        (
            "Tap target creature. You may exile a card from your graveyard. If you do, attach it to that creature.",
            "attach it",
            |effect| {
                matches!(
                    effect,
                    Effect::Attach {
                        attachment: TargetFilter::ParentTarget,
                        target: TargetFilter::ParentTarget,
                        ..
                    }
                )
            },
        ),
    ] {
        assert_extra_parent_read_declines(text, label, is_reader);
    }
}

/// Row 28b (synthetic, preservation). CR 608.2c + CR 115.10a: "return it to its
/// owner's hand" acts on the object the exile handed the chain and leaves its
/// zone unproven, so the stun counter's "that creature" could name it and the
/// binder declines.
#[test]
fn returned_pronoun_before_reader_across_resolution_choice_declines() {
    assert_extra_parent_read_declines(
        "Tap target creature. You may exile a card from your graveyard. If you do, return it to its owner's hand and put a stun counter on that creature.",
        "return it and put",
        is_parent_counter,
    );
}
