//! Crabomination: "target opponent exiles the top card of their library, a
//! card at random from their graveyard, and a card at random from their hand.
//! You may cast a spell from among cards exiled this way without paying its
//! mana cost."
//!
//! The object list is one exile instruction (CR 608.2f): non-moving selection
//! legs gather the library top card and one random card from each of the
//! graveyard and hand into the chain's tracked set, then a single
//! `ChangeZoneAll` exiles them all. The free cast reads exactly that batch.
//!
//! Every test drives the real enters trigger: the creature is cast, resolves,
//! and its trigger resolves through the scenario runner.

use engine::game::players::linked_exile_cards_for_source;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastOfferKind, ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CRABOMINATION_ORACLE: &str = "Emerge from artifact {5}{B}{B} (You may cast this spell by sacrificing an artifact and paying the emerge cost reduced by that artifact's mana value.)\nWhen this creature enters, target opponent exiles the top card of their library, a card at random from their graveyard, and a card at random from their hand. You may cast a spell from among cards exiled this way without paying its mana cost.";

const LAELIA_ORACLE: &str = "Haste\nWhenever Laelia attacks, exile the top card of your library. You may play that card this turn.\nWhenever one or more cards are put into exile from your library and/or your graveyard, put a +1/+1 counter on Laelia.";

const P2: PlayerId = PlayerId(2);

/// The cards a fixture placed in the targeted opponent's zones.
struct Fixture {
    runner: GameRunner,
    crab: ObjectId,
    library: Vec<ObjectId>,
    graveyard: Vec<ObjectId>,
    hand: Vec<ObjectId>,
}

/// Crabomination in P0's hand; P1 gets `library` castable spells on top of a
/// filler card (top card last), `graveyard` graveyard spells and `hand` hand
/// spells. A zone given 0 is left empty (the library has no filler then).
fn fixture(library: usize, graveyard: usize, hand: usize) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crab = scenario
        .add_creature_to_hand_from_oracle(P0, "Crabomination", 5, 5, CRABOMINATION_ORACLE)
        .id();
    let mut lib = Vec::new();
    if library > 0 {
        scenario.add_spell_to_library_top(P1, "Library Filler", false);
    }
    for i in 0..library {
        lib.push(
            scenario
                .add_spell_to_library_top(P1, &format!("Library Spell {i}"), false)
                .from_oracle_text("Draw a card.")
                .id(),
        );
    }
    let gy = (0..graveyard)
        .map(|i| {
            scenario
                .add_spell_to_graveyard(P1, &format!("Graveyard Spell {i}"), true)
                .from_oracle_text("Draw a card.")
                .id()
        })
        .collect();
    let hd = (0..hand)
        .map(|i| {
            scenario
                .add_spell_to_hand(P1, &format!("Hand Spell {i}"), true)
                .from_oracle_text("Draw a card.")
                .id()
        })
        .collect();
    Fixture {
        runner: scenario.build(),
        crab,
        library: lib,
        graveyard: gy,
        hand: hd,
    }
}

/// Cast Crabomination for its mana cost targeting P1 and resolve the creature
/// and its enters trigger.
fn cast_crab(runner: &mut GameRunner, crab: ObjectId) -> CastOutcome {
    runner
        .cast(crab)
        .alternative_cast(AlternativeCastDecision::Normal)
        .target_player(P1)
        .resolve()
}

fn in_exile(runner: &GameRunner, ids: &[ObjectId]) -> Vec<ObjectId> {
    ids.iter()
        .copied()
        .filter(|id| runner.state().objects[id].zone == Zone::Exile)
        .collect()
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

/// The free-cast window the trigger opened: (player, candidates, remaining casts).
fn free_cast_window(waiting: &WaitingFor) -> (PlayerId, Vec<ObjectId>, Option<u8>) {
    match waiting {
        WaitingFor::CastOffer {
            player,
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    remaining_casts,
                    ..
                },
        } => (*player, candidates.clone(), *remaining_casts),
        other => panic!("expected a FreeCastWindow cast offer, got {other:?}"),
    }
}

/// How many `EffectResolved { kind }` events `source`'s resolutions emitted.
fn resolutions(outcome: &CastOutcome, source: ObjectId, kind: EffectKind) -> usize {
    outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved { kind: k, source_id, .. }
                    if *k == kind && *source_id == source
            )
        })
        .count()
}

/// Reach-guard: the enters trigger's exile instruction resolved — all three
/// selection legs and the single `ChangeZoneAll` ran (at base the trigger was
/// one `ExileTop` and none of these resolved).
fn assert_exile_instruction_resolved(outcome: &CastOutcome, crab: ObjectId) {
    assert_eq!(
        resolutions(outcome, crab, EffectKind::ChooseFromZone),
        3,
        "the library, graveyard and hand legs each resolve"
    );
    assert!(
        resolutions(outcome, crab, EffectKind::ChangeZoneAll) >= 1,
        "the single exile action resolves"
    );
}

fn assert_priority_with_empty_stack(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "expected Priority, got {:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty(), "stack should be empty");
}

/// R1 (AC1, AC4). CR 701.13a + CR 608.2c: the targeted opponent exiles the top
/// card of their library, exactly one random graveyard card and exactly one
/// random hand card. At base only the library card moved.
#[test]
fn crabomination_exiles_one_card_from_each_of_three_zones() {
    let Fixture {
        mut runner,
        crab,
        library,
        graveyard,
        hand,
    } = fixture(1, 3, 3);
    let _ = cast_crab(&mut runner, crab);

    // Reach-guard: the creature resolved and its trigger exiled the library
    // top card, linked to Crabomination.
    assert_eq!(runner.state().objects[&crab].zone, Zone::Battlefield);
    assert_eq!(in_exile(&runner, &library), library);
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == library[0] && link.source_id == crab),
        "the library card must be linked to Crabomination"
    );

    let gy_exiled = in_exile(&runner, &graveyard);
    let hand_exiled = in_exile(&runner, &hand);
    assert_eq!(gy_exiled.len(), 1, "exactly one graveyard card is exiled");
    assert_eq!(hand_exiled.len(), 1, "exactly one hand card is exiled");
    for id in graveyard.iter().filter(|id| !gy_exiled.contains(id)) {
        assert_eq!(runner.state().objects[id].zone, Zone::Graveyard);
    }
    for id in hand.iter().filter(|id| !hand_exiled.contains(id)) {
        assert_eq!(runner.state().objects[id].zone, Zone::Hand);
    }
}

/// Runs one single-empty-zone case and checks the exiled set is exactly the
/// non-empty zones' picks, and the window offers exactly that set.
fn assert_empty_zone_contributes_nothing(library: usize, graveyard: usize, hand: usize) {
    let Fixture {
        mut runner,
        crab,
        library: lib,
        graveyard: gy,
        hand: hd,
    } = fixture(library, graveyard, hand);
    let outcome = cast_crab(&mut runner, crab);
    let waiting = outcome.final_waiting_for();

    let mut exiled = in_exile(&runner, &lib);
    exiled.extend(in_exile(&runner, &gy));
    exiled.extend(in_exile(&runner, &hd));
    let expected = usize::from(library > 0) + usize::from(graveyard > 0) + usize::from(hand > 0);
    // Reach-guard: the non-empty zones did contribute.
    assert!(!exiled.is_empty());
    // CR 609.3: an empty zone contributes nothing; the others still do.
    assert_eq!(exiled.len(), expected);
    let (player, candidates, remaining) = free_cast_window(waiting);
    assert_eq!(player, P0);
    assert_eq!(remaining, Some(1));
    assert_eq!(sorted(candidates), sorted(exiled));
}

/// R2 (AC1): an empty graveyard contributes nothing.
#[test]
fn crabomination_empty_graveyard_contributes_nothing() {
    assert_empty_zone_contributes_nothing(1, 0, 2);
}

/// R2 (AC1): an empty hand contributes nothing.
#[test]
fn crabomination_empty_hand_contributes_nothing() {
    assert_empty_zone_contributes_nothing(1, 2, 0);
}

/// R2 (AC1): an empty library contributes nothing.
#[test]
fn crabomination_empty_library_contributes_nothing() {
    assert_empty_zone_contributes_nothing(0, 2, 2);
}

/// R2 (AC1). CR 609.3: with all three zones empty nothing is exiled and no
/// cast is offered.
#[test]
fn crabomination_all_zones_empty_exiles_nothing() {
    let Fixture {
        mut runner, crab, ..
    } = fixture(0, 0, 0);
    let outcome = cast_crab(&mut runner, crab);
    assert_eq!(runner.state().objects[&crab].zone, Zone::Battlefield);
    assert_exile_instruction_resolved(&outcome, crab);
    assert!(runner.state().exile.is_empty());
    assert_priority_with_empty_stack(&runner);
}

/// R3 (AC2). CR 608.2g: the controller may cast one spell from among all three
/// cards exiled this way, during the resolution, without paying its mana cost.
#[test]
fn crabomination_offers_all_three_and_casts_one_free() {
    let Fixture {
        mut runner,
        crab,
        library,
        graveyard,
        hand,
    } = fixture(1, 1, 1);
    let outcome = cast_crab(&mut runner, crab);
    let (player, candidates, remaining) = free_cast_window(outcome.final_waiting_for());
    assert_eq!(player, P0);
    assert_eq!(remaining, Some(1));
    assert_eq!(
        sorted(candidates),
        sorted(vec![library[0], graveyard[0], hand[0]])
    );

    let mana_before = runner.state().players[0].mana_pool.total();
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(graveyard[0]),
        })
        .expect("casting an offered card is legal");
    let state = runner.state();
    assert_eq!(state.objects[&graveyard[0]].zone, Zone::Stack);
    assert_eq!(state.players[0].mana_pool.total(), mana_before);
    assert_eq!(state.objects[&library[0]].zone, Zone::Exile);
    assert_eq!(state.objects[&hand[0]].zone, Zone::Exile);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "one cast only: no re-offer, got {:?}",
        state.waiting_for
    );
}

/// R4 (AC2): declining leaves every exiled card in exile.
#[test]
fn crabomination_decline_leaves_cards_exiled() {
    let Fixture {
        mut runner,
        crab,
        library,
        graveyard,
        hand,
    } = fixture(1, 1, 1);
    let outcome = cast_crab(&mut runner, crab);
    let (_, candidates, _) = free_cast_window(outcome.final_waiting_for());
    assert_eq!(candidates.len(), 3);

    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the window is legal");
    for id in [library[0], graveyard[0], hand[0]] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }
    assert_priority_with_empty_stack(&runner);
}

/// R5 (ruling). CR 305.1 + CR 608.2g: a land card is never a spell, so the
/// cast-a-spell permission can't cast the exiled land; it stays in exile.
#[test]
fn crabomination_exiled_land_is_not_offered_and_stays_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crab = scenario
        .add_creature_to_hand_from_oracle(P0, "Crabomination", 5, 5, CRABOMINATION_ORACLE)
        .id();
    scenario.add_spell_to_library_top(P1, "Library Filler", false);
    let forest = scenario.add_land_to_library_top(P1, "Forest").id();
    let gy = scenario
        .add_spell_to_graveyard(P1, "Graveyard Spell", true)
        .from_oracle_text("Draw a card.")
        .id();
    let hand = scenario
        .add_spell_to_hand(P1, "Hand Spell", true)
        .from_oracle_text("Draw a card.")
        .id();
    let mut runner = scenario.build();
    let outcome = cast_crab(&mut runner, crab);

    assert_eq!(runner.state().objects[&forest].zone, Zone::Exile);
    let (_, candidates, _) = free_cast_window(outcome.final_waiting_for());
    // Reach-guard: the nonland cards are offered.
    assert_eq!(sorted(candidates.clone()), sorted(vec![gy, hand]));
    assert!(!candidates.contains(&forest));

    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the window is legal");
    assert_eq!(runner.state().objects[&forest].zone, Zone::Exile);
    assert_priority_with_empty_stack(&runner);
}

/// R6 (multi-authority). CR 115.1d + CR 608.2c: "their" names the targeted
/// opponent only; another opponent's and the controller's zones are untouched.
#[test]
fn crabomination_three_player_touches_only_target() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let crab = scenario
        .add_creature_to_hand_from_oracle(P0, "Crabomination", 5, 5, CRABOMINATION_ORACLE)
        .id();
    scenario.add_spell_to_library_top(P1, "Library Filler", false);
    let p1_lib = scenario
        .add_spell_to_library_top(P1, "P1 Library", false)
        .from_oracle_text("Draw a card.")
        .id();
    let p1_gy = scenario
        .add_spell_to_graveyard(P1, "P1 Graveyard", true)
        .from_oracle_text("Draw a card.")
        .id();
    let p1_hand = scenario
        .add_spell_to_hand(P1, "P1 Hand", true)
        .from_oracle_text("Draw a card.")
        .id();
    let p2_gy = scenario
        .add_spell_to_graveyard(P2, "P2 Graveyard", true)
        .id();
    let p2_hand = scenario.add_spell_to_hand(P2, "P2 Hand", true).id();
    let p0_gy = scenario
        .add_spell_to_graveyard(P0, "P0 Graveyard", true)
        .id();
    let p0_hand = scenario.add_spell_to_hand(P0, "P0 Hand", true).id();
    let mut runner = scenario.build();
    let _ = cast_crab(&mut runner, crab);

    let state = runner.state();
    for id in [p1_lib, p1_gy, p1_hand] {
        assert_eq!(state.objects[&id].zone, Zone::Exile);
    }
    assert_eq!(state.objects[&p2_gy].zone, Zone::Graveyard);
    assert_eq!(state.objects[&p2_hand].zone, Zone::Hand);
    assert_eq!(state.objects[&p0_gy].zone, Zone::Graveyard);
    assert_eq!(state.objects[&p0_hand].zone, Zone::Hand);
}

/// R7 (multi-authority). CR 608.2c: "cards exiled this way" is this
/// resolution's batch, not every card linked to Crabomination. A castable card
/// already linked to the same source is not offered.
#[test]
fn crabomination_prior_exile_linked_to_it_is_not_offered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crab = scenario
        .add_creature_to_hand_from_oracle(P0, "Crabomination", 5, 5, CRABOMINATION_ORACLE)
        .id();
    scenario.add_spell_to_library_top(P1, "Library Filler", false);
    let lib = scenario
        .add_spell_to_library_top(P1, "Library Spell", false)
        .from_oracle_text("Draw a card.")
        .id();
    let gy = scenario
        .add_spell_to_graveyard(P1, "Graveyard Spell", true)
        .from_oracle_text("Draw a card.")
        .id();
    let hand = scenario
        .add_spell_to_hand(P1, "Hand Spell", true)
        .from_oracle_text("Draw a card.")
        .id();
    let prior = scenario
        .add_spell_to_exile(P1, "Prior Exile", true)
        .from_oracle_text("Draw a card.")
        .id();
    let mut runner = scenario.build();
    drop(
        runner
            .cast(crab)
            .alternative_cast(AlternativeCastDecision::Normal)
            .target_player(P1)
            .commit(),
    );
    for _ in 0..4 {
        if runner.state().objects[&crab].zone == Zone::Battlefield {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    assert_eq!(runner.state().objects[&crab].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the enters trigger is on the stack"
    );

    // A prior producer linked to the same source.
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: prior,
        source_id: crab,
        kind: ExileLinkKind::TrackedBySource,
    });
    let linked = |runner: &GameRunner| -> Vec<ObjectId> {
        linked_exile_cards_for_source(runner.state(), crab)
            .iter()
            .map(|link| link.exiled_id)
            .collect()
    };
    // Reach-guard: the prior card is linked before the trigger resolves.
    assert!(linked(&runner).contains(&prior));

    runner.resolve_top();
    // Reach-guard: and is still linked after it.
    assert!(linked(&runner).contains(&prior));
    let (player, candidates, _) = free_cast_window(&runner.state().waiting_for);
    assert_eq!(player, P0);
    assert!(!candidates.contains(&prior));
    assert_eq!(sorted(candidates), sorted(vec![lib, gy, hand]));
}

/// R8. CR 608.2f + CR 603.2c: the three objects are exiled by one action, so
/// a "one or more cards are put into exile" trigger fires once.
#[test]
fn crabomination_exiles_are_one_batch_for_laelia() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crab = scenario
        .add_creature_to_hand_from_oracle(P0, "Crabomination", 5, 5, CRABOMINATION_ORACLE)
        .id();
    let laelia = scenario
        .add_creature_from_oracle(P1, "Laelia, the Blade Reforged", 2, 2, LAELIA_ORACLE)
        .id();
    scenario.add_spell_to_library_top(P1, "Library Filler", false);
    let lib = scenario
        .add_spell_to_library_top(P1, "Library Spell", false)
        .id();
    let gy = scenario
        .add_spell_to_graveyard(P1, "Graveyard Spell", true)
        .id();
    let hand = scenario.add_spell_to_hand(P1, "Hand Spell", true).id();
    let mut runner = scenario.build();
    let outcome = cast_crab(&mut runner, crab);
    let _ = free_cast_window(outcome.final_waiting_for());
    for id in [lib, gy, hand] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }

    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the window is legal");
    let laelia_entries = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == laelia)
        .count();
    // Reach-guard: Laelia's batched trigger fired.
    assert!(laelia_entries >= 1);
    assert_eq!(laelia_entries, 1, "one exile action triggers Laelia once");

    runner.advance_until_stack_empty();
    let counters = runner.state().objects[&laelia]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(counters, 1);
}

/// R10 (U3). CR 401.2: only the top card of the library is exiled, and the
/// cards below it keep their order.
#[test]
fn crabomination_exiles_only_the_library_top_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crab = scenario
        .add_creature_to_hand_from_oracle(P0, "Crabomination", 5, 5, CRABOMINATION_ORACLE)
        .id();
    let library: Vec<ObjectId> = (0..8)
        .map(|i| {
            scenario
                .add_spell_to_library_top(P1, &format!("Library {i}"), false)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    let before: Vec<ObjectId> = runner.state().library_of(P1).iter().copied().collect();
    // Reach-guard: a deep library, top card last added.
    assert!(before.len() >= 2);
    assert_eq!(before.first(), library.last());

    let draws_before = runner.state().rng.outcome_draws();
    let _ = cast_crab(&mut runner, crab);
    assert_eq!(in_exile(&runner, &library), vec![before[0]]);
    // CR 401.2: the library leg takes the top card by order; with the
    // graveyard and hand empty no leg makes a random draw.
    assert_eq!(runner.state().rng.outcome_draws(), draws_before);
    let after: Vec<ObjectId> = runner.state().library_of(P1).iter().copied().collect();
    assert_eq!(after, before[1..].to_vec());
}

/// R13 (multi-authority). CR 608.2c: a stale tracked set from earlier in the
/// game is not what this instruction exiles; with the opponent's zones empty
/// nothing moves.
#[test]
fn crabomination_stale_tracked_set_is_not_exiled_when_zones_empty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crab = scenario
        .add_creature_to_hand_from_oracle(P0, "Crabomination", 5, 5, CRABOMINATION_ORACLE)
        .id();
    let bystander_gy = scenario.add_spell_to_graveyard(P0, "Bystander", true).id();
    let bystander_hand = scenario.add_spell_to_hand(P0, "Bystander Hand", true).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state
            .tracked_object_sets
            .insert(TrackedSetId(50), vec![bystander_gy, bystander_hand]);
        state.next_tracked_set_id = state.next_tracked_set_id.max(51);
    }
    let outcome = cast_crab(&mut runner, crab);

    assert_eq!(runner.state().objects[&crab].zone, Zone::Battlefield);
    assert_exile_instruction_resolved(&outcome, crab);
    assert_eq!(runner.state().objects[&bystander_gy].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&bystander_hand].zone, Zone::Hand);
    assert!(runner.state().exile.is_empty());
    // The guard: the exile action read the chain set this instruction bound —
    // empty, since nothing was exiled — and consumed it (a consumed set is
    // dropped); the stale set is neither read nor consumed.
    let state = runner.state();
    let read = state
        .chain_tracked_set_id
        .expect("the exile action's tracked set is bound");
    assert_ne!(read, TrackedSetId(50));
    assert_eq!(
        state.tracked_object_sets.get(&read),
        None,
        "the exile action consumed the bound set"
    );
    assert_eq!(
        state.tracked_object_sets.get(&TrackedSetId(50)),
        Some(&vec![bystander_gy, bystander_hand]),
        "the stale set is untouched"
    );
    assert_priority_with_empty_stack(&runner);
}

/// Hostile (two player targets). CR 115.1 + CR 608.2c: every object of the
/// list reads the zones of the instruction's own subject — the opponent its
/// own "target opponent" announced — even when an earlier instruction of the
/// same spell announced another player first. A random leg's pick must not
/// redirect the next leg to that earlier player.
#[test]
fn subject_exile_list_reads_its_own_subject_after_an_earlier_player_target() {
    const ORACLE: &str = "Target player draws a card. Target opponent exiles the top card of their library, a card at random from their graveyard, and a card at random from their hand.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Two Target Probe", false, ORACLE)
        .id();
    let p0_draw = scenario.add_spell_to_library_top(P0, "P0 Draw", false).id();
    let p0_hand: Vec<ObjectId> = (0..4)
        .map(|i| {
            scenario
                .add_spell_to_hand(P0, &format!("P0 Hand {i}"), true)
                .id()
        })
        .collect();
    let p0_gy = scenario
        .add_spell_to_graveyard(P0, "P0 Graveyard", true)
        .id();
    scenario.add_spell_to_library_top(P1, "P1 Filler", false);
    let p1_lib = scenario
        .add_spell_to_library_top(P1, "P1 Library", false)
        .id();
    let p1_gy = scenario
        .add_spell_to_graveyard(P1, "P1 Graveyard", true)
        .id();
    let p1_hand = scenario.add_spell_to_hand(P1, "P1 Hand", true).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_players(&[P0, P1]).resolve();

    // Reach-guard: both instructions resolved — P0 drew its library card, and
    // all three legs of the exile instruction ran.
    assert_ne!(runner.state().objects[&p0_draw].zone, Zone::Library);
    assert_eq!(
        resolutions(&outcome, spell, EffectKind::ChooseFromZone),
        3,
        "the library, graveyard and hand legs each resolve"
    );
    let state = runner.state();
    for id in [p1_lib, p1_gy, p1_hand] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Exile,
            "the targeted opponent's card is exiled"
        );
    }
    let p0_exiled: Vec<ObjectId> = p0_hand
        .iter()
        .chain([&p0_draw])
        .copied()
        .filter(|id| state.objects[id].zone == Zone::Exile)
        .collect();
    assert!(
        p0_exiled.is_empty(),
        "no card of the first target's hand is exiled: {p0_exiled:?}"
    );
    assert_eq!(state.objects[&p0_gy].zone, Zone::Graveyard);
    assert_priority_with_empty_stack(&runner);
}

/// Hostile (earlier exile instruction). CR 608.2c + CR 608.2f: the list
/// gathers only its own objects. A card an earlier instruction of the same
/// resolution exiled is neither moved again by the list's single exile action
/// nor offered among the cards "exiled this way".
#[test]
fn subject_exile_list_gathers_only_its_own_objects_after_an_earlier_exile() {
    const ORACLE: &str = "Exile target creature an opponent controls. Target opponent exiles the top card of their library and a card at random from their hand. You may cast a spell from among cards exiled this way without paying its mana cost.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Earlier Exile Probe", false, ORACLE)
        .id();
    let creature = scenario.add_creature(P1, "Earlier Creature", 2, 2).id();
    scenario.add_spell_to_library_top(P1, "P1 Filler", false);
    let p1_lib = scenario
        .add_spell_to_library_top(P1, "P1 Library", false)
        .from_oracle_text("Draw a card.")
        .id();
    let p1_hand = scenario
        .add_spell_to_hand(P1, "P1 Hand", true)
        .from_oracle_text("Draw a card.")
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_object(creature)
        .target_player(P1)
        .resolve();

    // Reach-guard: the earlier instruction exiled the creature, and both legs
    // of the list ran and exiled their cards.
    assert_eq!(runner.state().objects[&creature].zone, Zone::Exile);
    assert_eq!(
        resolutions(&outcome, spell, EffectKind::ChooseFromZone),
        2,
        "the library and hand legs each resolve"
    );
    assert_eq!(runner.state().objects[&p1_lib].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&p1_hand].zone, Zone::Exile);
    let creature_moves = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged { object_id, .. } if *object_id == creature
            )
        })
        .count();
    assert_eq!(
        creature_moves, 1,
        "the earlier card is exiled once, not again"
    );
    let (player, candidates, remaining) = free_cast_window(outcome.final_waiting_for());
    assert_eq!(player, P0);
    assert_eq!(remaining, Some(1));
    assert_eq!(sorted(candidates), sorted(vec![p1_lib, p1_hand]));
}

/// Hostile (illegal subject, MED-1). CR 608.2b: when the list's subject is no
/// longer a legal target at resolution, the part of the effect that needs that
/// player doesn't happen, so the list exiles nothing — and its single exile
/// action still reads only the list's own (empty) gathered set, never the set
/// the earlier instruction published. The earlier card is moved once and is
/// not offered among the cards "exiled this way".
#[test]
fn subject_exile_list_with_an_illegal_subject_gathers_nothing_from_an_earlier_exile() {
    const ORACLE: &str = "Exile target creature an opponent controls. Target opponent exiles the top card of their library and a card at random from their hand. You may cast a spell from among cards exiled this way without paying its mana cost.";
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Illegal Subject Probe", false, ORACLE)
        .id();
    let creature = scenario.add_creature(P2, "P2 Creature", 2, 2).id();
    scenario.add_spell_to_library_top(P1, "P1 Filler", false);
    scenario.add_spell_to_library_top(P1, "P1 Library", false);
    scenario.add_spell_to_hand(P1, "P1 Hand", true);
    let mut runner = scenario.build();
    drop(
        runner
            .cast(spell)
            .target_object(creature)
            .target_player(P1)
            .commit(),
    );
    assert_eq!(runner.state().stack.len(), 1, "the spell is on the stack");
    // The subject leaves the game before the spell resolves (CR 800.4a: its
    // cards leave with it), so it is no longer a legal target (CR 608.2b); the
    // creature target stays legal.
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("a player may concede");

    let mut events = Vec::new();
    for _ in 0..12 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority is legal")
                .events,
        );
    }

    // Reach-guard: the first instruction exiled the creature.
    let state = runner.state();
    assert_eq!(state.objects[&creature].zone, Zone::Exile);

    let creature_moves = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged { object_id, .. } if *object_id == creature
            )
        })
        .count();
    assert_eq!(
        creature_moves, 1,
        "the earlier card is exiled once, not again"
    );
    if let WaitingFor::CastOffer {
        kind: CastOfferKind::FreeCastWindow { candidates, .. },
        ..
    } = &state.waiting_for
    {
        assert!(
            !candidates.contains(&creature),
            "the earlier card is not one of the cards exiled this way: {candidates:?}"
        );
    }

    // Reach-guard: both legs of the list ran, and the spell finished resolving.
    let list_legs = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved { kind: EffectKind::ChooseFromZone, source_id, .. }
                    if *source_id == spell
            )
        })
        .count();
    assert_eq!(list_legs, 2, "the library and hand legs each resolve");
    assert!(state.stack.is_empty(), "the spell finished resolving");
    assert_ne!(state.objects[&spell].zone, Zone::Stack);
}

/// Hostile (at-random object first, MED-2a). CR 608.2c + CR 608.2f: the
/// objects may be listed in any order. With the at-random object first, both
/// objects are still exiled by the list's one action, and the card an earlier
/// instruction of the same resolution exiled is neither moved again nor
/// offered among the cards "exiled this way".
#[test]
fn subject_exile_list_at_random_first_gathers_only_its_own_objects() {
    const ORACLE: &str = "Exile target creature an opponent controls. Target opponent exiles a card at random from their hand and the top card of their library. You may cast a spell from among cards exiled this way without paying its mana cost.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "At Random First Probe", false, ORACLE)
        .id();
    let creature = scenario.add_creature(P1, "Earlier Creature", 2, 2).id();
    scenario.add_spell_to_library_top(P1, "P1 Filler", false);
    let p1_lib = scenario
        .add_spell_to_library_top(P1, "P1 Library", false)
        .from_oracle_text("Draw a card.")
        .id();
    let p1_hand = scenario
        .add_spell_to_hand(P1, "P1 Hand", true)
        .from_oracle_text("Draw a card.")
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_object(creature)
        .target_player(P1)
        .resolve();

    // Reach-guard: the earlier instruction exiled the creature, and both legs
    // of the list ran.
    assert_eq!(runner.state().objects[&creature].zone, Zone::Exile);
    assert_eq!(
        resolutions(&outcome, spell, EffectKind::ChooseFromZone),
        2,
        "the hand and library legs each resolve"
    );
    // CR 608.2f: the legs only select; the list's single exile action moves
    // both objects.
    assert_eq!(runner.state().objects[&p1_hand].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&p1_lib].zone, Zone::Exile);
    assert_eq!(
        resolutions(&outcome, spell, EffectKind::ChangeZoneAll),
        1,
        "one exile action moves the list's objects"
    );
    let creature_moves = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged { object_id, .. } if *object_id == creature
            )
        })
        .count();
    assert_eq!(
        creature_moves, 1,
        "the earlier card is exiled once, not again"
    );
    let (player, candidates, remaining) = free_cast_window(outcome.final_waiting_for());
    assert_eq!(player, P0);
    assert_eq!(remaining, Some(1));
    assert_eq!(sorted(candidates), sorted(vec![p1_lib, p1_hand]));
}
