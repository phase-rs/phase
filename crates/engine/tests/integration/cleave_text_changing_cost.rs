//! Integration coverage for the Cleave keyword (CR 702.148a-b + CR 612).
//!
//! Cleave is an alternative-cost (CR 118.9) + text-changing-effect keyword: a
//! spell with cleave may be cast for its cleave cost, and doing so removes every
//! square-bracketed span from the spell's rules text. The engine models this as
//!   * a build-time second parse over the bracket-removed text, stored in
//!     `CardFace::cleave_variant` (projected onto `GameObject::cleave_variant`);
//!   * a base parse over the bracket-content-kept text (`BracketMode::KeepContent`)
//!     so the printed-cost spell is parsed correctly; and
//!   * a cast-time object swap (`CastingVariant::Cleave`) that installs the
//!     cleave ability set before the spell is prepared.
//!
//! These tests build the cleave cards inline through the scenario harness (which
//! mirrors the real build pipeline's cleave prep) and drive the cast through the
//! real `apply()` pipeline. Each assertion is discriminating: it fails if the
//! base/cleave ability sets are swapped, dropped, or clobbered.

use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, FilterProp, TargetFilter, TargetRef, TypeFilter};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// ---------------------------------------------------------------------------
// Oracle text — the published cards. Brackets mark the cleave-removable spans.
// ---------------------------------------------------------------------------

const DREAD_FUGUE: &str = "Cleave {2}{B}\n\
Target player reveals their hand. You choose a nonland card from it [with mana value 2 or less]. That player discards that card.";

const FIERCE_RETRIBUTION: &str = "Cleave {5}{W}\n\
Destroy target [attacking] creature.";

const PATH_OF_PERIL: &str = "Cleave {4}{W}{B}\n\
Destroy all creatures [with mana value 2 or less].";

const WINGED_PORTENT: &str = "Cleave {4}{G}{U}\n\
Draw a card for each creature you control [with flying].";

const DIG_UP: &str = "Cleave {1}{B}{B}{G}\n\
Search your library for a [basic land] card, [reveal it,] put it into your hand, then shuffle.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generous mana so both the printed cost and the cleave cost are always
/// affordable — drives the offer (`AlternativeCastChoice`) path rather than an
/// auto-skip.
fn flood_mana(runner: &mut GameRunner) {
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for ty in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Green,
        ManaType::Red,
        ManaType::Colorless,
    ] {
        for _ in 0..8 {
            pool.add(ManaUnit::new(ty, ObjectId(0), false, vec![]));
        }
    }
}

/// Build a cleave spell of the given core type in P0's hand and return its id.
/// The cleave cost is parsed from the "Cleave {cost}" line; the inline keyword
/// hint drives the harness's cleave bracket prep.
fn cleave_spell_in_hand(
    scenario: &mut GameScenario,
    name: &str,
    oracle: &str,
    printed_cost: ManaCost,
    as_sorcery: bool,
) -> ObjectId {
    let mut builder = scenario.add_creature_to_hand(P0, name, 0, 0);
    if as_sorcery {
        builder.as_sorcery();
    } else {
        builder.as_instant();
    }
    builder.with_mana_cost(printed_cost);
    builder.from_oracle_text_with_keywords(&["cleave"], oracle);
    builder.id()
}

/// Cast `object_id` choosing the cleave cost (`Alternative`) or printed cost
/// (`Normal`). Returns the resulting `WaitingFor` after the choice is applied.
fn cast_with_decision(
    runner: &mut GameRunner,
    object_id: ObjectId,
    decision: AlternativeCastDecision,
) {
    let card_id = runner.state().objects[&object_id].card_id;
    let wf = runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should reach the cleave choice or proceed");
    // Both costs affordable → an AlternativeCastChoice(Cleave) offer.
    assert!(
        matches!(
            wf.waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: engine::types::game_state::AlternativeCastKeyword::Cleave,
                ..
            }
        ),
        "expected AlternativeCastChoice(Cleave), got {:?}",
        wf.waiting_for
    );
    runner
        .act(GameAction::ChooseAlternativeCast { choice: decision })
        .expect("cleave/printed choice should be accepted");
}

/// Pick the first legal target while the runner is in a target-selection state,
/// then drive the stack to settle.
fn select_first_target_and_resolve(runner: &mut GameRunner) {
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .choose_first_legal_target()
            .expect("first legal target should be selectable");
    }
    runner.advance_until_stack_empty();
}

// ---------------------------------------------------------------------------
// 1 + 2. Dread Fugue: base reveal-choice is MV<=2 nonland; cleave is any nonland.
//        The chosen card is discarded in BOTH modes (the contributor's bug
//        dropped the discard — these fail on revert of that fix).
// ---------------------------------------------------------------------------

fn setup_dread_fugue(opponent_hand: &[(&str, u32)]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fugue = cleave_spell_in_hand(
        &mut scenario,
        "Dread Fugue",
        DREAD_FUGUE,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        },
        true,
    );
    // Stage the opponent's hand with cards of known mana values.
    for (name, mv) in opponent_hand {
        let mut b = scenario.add_creature_to_hand(P1, name, 1, 1);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: *mv,
        });
    }
    let mut runner = scenario.build();
    flood_mana(&mut runner);
    (runner, fugue)
}

/// The eligible reveal-choice set for the current `RevealChoice` waiting state.
fn reveal_choice_cards(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::RevealChoice { cards, .. } => cards.clone(),
        other => panic!("expected RevealChoice, got {other:?}"),
    }
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

/// While the runner is selecting "target player", choose `player` explicitly
/// (the harness's first-legal helper would pick the caster, whose hand is empty
/// after the spell leaves it). Then resolve the spell.
fn target_player_and_resolve(runner: &mut GameRunner, player: PlayerId) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "expected target-player selection, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(player)),
        })
        .expect("target player should be a legal choice");
    runner.advance_until_stack_empty();
}

#[test]
fn dread_fugue_base_restricts_choice_to_mv2_and_discards() {
    // Opponent hand: a MV-2 nonland (choosable) and a MV-3 nonland (not).
    let (mut runner, fugue) = setup_dread_fugue(&[("Small Spell", 2), ("Big Spell", 3)]);
    let p1_hand_before = hand_size(&runner, P1);

    cast_with_decision(&mut runner, fugue, AlternativeCastDecision::Normal);
    // "Target player reveals their hand" — choose P1.
    target_player_and_resolve(&mut runner, P1);

    let eligible = reveal_choice_cards(&runner);
    let names: Vec<&str> = eligible
        .iter()
        .map(|id| runner.state().objects[id].name.as_str())
        .collect();
    assert!(
        names.contains(&"Small Spell"),
        "base mode: MV-2 nonland must be choosable, got {names:?}"
    );
    assert!(
        !names.contains(&"Big Spell"),
        "base mode: MV-3 nonland must NOT be choosable (CMC<=2 restriction), got {names:?}"
    );

    // Choose the MV-2 card and confirm it is discarded.
    let small = *eligible
        .iter()
        .find(|id| runner.state().objects[id].name == "Small Spell")
        .unwrap();
    runner
        .act(GameAction::SelectCards { cards: vec![small] })
        .expect("select the revealed card");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&small].zone,
        Zone::Graveyard,
        "base mode: the chosen card must be discarded (un-clobbered chain)"
    );
    assert_eq!(
        hand_size(&runner, P1),
        p1_hand_before - 1,
        "base mode: opponent hand should shrink by the discarded card"
    );
}

#[test]
fn dread_fugue_cleave_allows_any_nonland_and_discards() {
    // Opponent hand: only a MV-5 nonland — illegal in base mode, legal under cleave.
    let (mut runner, fugue) = setup_dread_fugue(&[("Expensive Spell", 5)]);
    let p1_hand_before = hand_size(&runner, P1);

    cast_with_decision(&mut runner, fugue, AlternativeCastDecision::Alternative);
    target_player_and_resolve(&mut runner, P1);

    let eligible = reveal_choice_cards(&runner);
    let names: Vec<&str> = eligible
        .iter()
        .map(|id| runner.state().objects[id].name.as_str())
        .collect();
    assert!(
        names.contains(&"Expensive Spell"),
        "cleave mode: MV-5 nonland must be choosable (no CMC restriction), got {names:?}"
    );

    let pick = eligible[0];
    runner
        .act(GameAction::SelectCards { cards: vec![pick] })
        .expect("select the revealed card");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&pick].zone,
        Zone::Graveyard,
        "cleave mode: the chosen card must be discarded"
    );
    assert_eq!(
        hand_size(&runner, P1),
        p1_hand_before - 1,
        "cleave mode: opponent hand should shrink by the discarded card"
    );
}

// ---------------------------------------------------------------------------
// 3. Fierce Retribution: base Destroy restricted to attacking; cleave any creature.
//    Asserted on the swapped spell-object abilities resolved on the stack.
// ---------------------------------------------------------------------------

/// The cast spell object after the cast choice resolved. The cleave swap mutates
/// `obj.abilities` in place at `object_id` BEFORE the spell is prepared, so the
/// object reflects the chosen variant whether the cast is mid-targeting or
/// already on the stack.
fn cast_spell_object(runner: &GameRunner, object_id: ObjectId) -> &GameObject {
    &runner.state().objects[&object_id]
}

fn destroy_target_filter(obj: &GameObject) -> &TargetFilter {
    match obj.abilities.first().map(|a| a.effect.as_ref()) {
        Some(Effect::Destroy { target, .. }) => target,
        other => panic!("expected Destroy effect, got {other:?}"),
    }
}

fn filter_has_attacking(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(tf)
            if tf.properties.iter().any(|p| matches!(p, FilterProp::Attacking))
    )
}

fn fierce_retribution_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = cleave_spell_in_hand(
        &mut scenario,
        "Fierce Retribution",
        FIERCE_RETRIBUTION,
        ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 5,
        },
        false,
    );
    // A plain (non-attacking) creature P1 controls — a legal target only when the
    // attacking restriction has been removed (cleave mode).
    scenario.add_creature(P1, "Bystander", 2, 2);
    (scenario, spell)
}

#[test]
fn fierce_retribution_base_destroys_attacking_only() {
    // Base mode: the printed parse restricts the Destroy target to attacking
    // creatures. With only a non-attacking creature present, the cast has no
    // legal target — confirming the attacking restriction is enforced.
    let (scenario, spell) = fierce_retribution_scenario();
    let mut runner = scenario.build();
    flood_mana(&mut runner);

    let filter = destroy_target_filter(cast_spell_object(&runner, spell)).clone();
    assert!(
        filter_has_attacking(&filter),
        "base mode: Destroy target must be restricted to attacking creatures, got {filter:?}"
    );

    // Casting at printed cost fails to find a legal target (no attacker).
    let card_id = runner.state().objects[&spell].card_id;
    let wf = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("offer surfaces");
    assert!(matches!(
        wf.waiting_for,
        WaitingFor::AlternativeCastChoice {
            keyword: engine::types::game_state::AlternativeCastKeyword::Cleave,
            ..
        }
    ));
    let err = runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Normal,
        })
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("No legal targets"),
        "base mode: a non-attacking creature must NOT be a legal Destroy target, got {err:?}"
    );
}

#[test]
fn fierce_retribution_cleave_destroys_any_creature() {
    // Cleave mode: the attacking restriction is removed, so the non-attacking
    // creature becomes a legal target and the cast proceeds.
    let (scenario, spell) = fierce_retribution_scenario();
    let mut runner = scenario.build();
    flood_mana(&mut runner);

    cast_with_decision(&mut runner, spell, AlternativeCastDecision::Alternative);
    let filter = destroy_target_filter(cast_spell_object(&runner, spell)).clone();
    assert!(
        !filter_has_attacking(&filter),
        "cleave mode: Destroy target must NOT carry the attacking restriction, got {filter:?}"
    );
    // The non-attacking creature is now a legal target, so the cast proceeds
    // past target legality (single legal target is auto-selected and the spell
    // goes on the stack) rather than erroring with "No legal targets".
    assert!(
        runner.state().stack.iter().any(|e| e.id == spell),
        "cleave mode: the spell should be on the stack with the non-attacking \
         creature as its legal target, got waiting_for {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// 4. Path of Peril: base DestroyAll CMC<=2; cleave all creatures.
//    Asserted via the projected `cleave_variant` vs base abilities (no targets,
//    so the swap is verified directly on the stack object's abilities).
// ---------------------------------------------------------------------------

fn destroy_all_filter(obj: &GameObject) -> &TargetFilter {
    match obj.abilities.first().map(|a| a.effect.as_ref()) {
        Some(Effect::DestroyAll { target, .. }) => target,
        other => panic!("expected DestroyAll effect, got {other:?}"),
    }
}

fn filter_has_cmc(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(tf)
            if tf.properties.iter().any(|p| matches!(p, FilterProp::Cmc { .. }))
    )
}

#[test]
fn path_of_peril_base_destroys_only_cmc2() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = cleave_spell_in_hand(
        &mut scenario,
        "Path of Peril",
        PATH_OF_PERIL,
        ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Black],
            generic: 4,
        },
        true,
    );
    let mut runner = scenario.build();
    flood_mana(&mut runner);

    cast_with_decision(&mut runner, spell, AlternativeCastDecision::Normal);
    let filter = destroy_all_filter(cast_spell_object(&runner, spell)).clone();
    assert!(
        filter_has_cmc(&filter),
        "base mode: DestroyAll must keep the CMC<=2 restriction, got {filter:?}"
    );
}

#[test]
fn path_of_peril_cleave_destroys_all_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = cleave_spell_in_hand(
        &mut scenario,
        "Path of Peril",
        PATH_OF_PERIL,
        ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Black],
            generic: 4,
        },
        true,
    );
    let mut runner = scenario.build();
    flood_mana(&mut runner);

    cast_with_decision(&mut runner, spell, AlternativeCastDecision::Alternative);
    let filter = destroy_all_filter(cast_spell_object(&runner, spell)).clone();
    assert!(
        !filter_has_cmc(&filter),
        "cleave mode: DestroyAll must drop the CMC restriction (all creatures), got {filter:?}"
    );
    assert!(
        matches!(
            &filter,
            TargetFilter::Typed(tf)
                if tf.type_filters.iter().any(|t| matches!(t, TypeFilter::Creature))
        ),
        "cleave mode: DestroyAll still targets creatures, got {filter:?}"
    );
}

// ---------------------------------------------------------------------------
// 4b. Zone-change regression (CR 702.148a): a cleave spell's text-changing
//     effect functions only while the spell is on the stack. After a cleave
//     cast resolves to the graveyard and the card is returned to hand (Regrowth
//     / Eternal Witness recursion reuses the same object id without
//     re-projecting the printed face), a NORMAL-cost recast must resolve with
//     the PRINTED (bracketed) restriction restored — NOT the leaked
//     bracket-removed cleave text.
//
//     This test FAILS before the zone-exit revert fix (the cleave_form leaks:
//     the graveyard object keeps the bracket-removed DestroyAll, so the
//     normal-cost recast's filter carries NO CMC restriction) and PASSES after
//     (the revert restores the printed CMC<=2 DestroyAll on stack exit).
// ---------------------------------------------------------------------------

fn path_of_peril_in_hand(scenario: &mut GameScenario) -> ObjectId {
    cleave_spell_in_hand(
        scenario,
        "Path of Peril",
        PATH_OF_PERIL,
        ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Black],
            generic: 4,
        },
        true,
    )
}

#[test]
fn path_of_peril_cleave_then_regrowth_recast_restores_printed_cmc_restriction() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = path_of_peril_in_hand(&mut scenario);
    let mut runner = scenario.build();
    flood_mana(&mut runner);

    // 1. Cast for the CLEAVE cost: the bracket-removed DestroyAll (no CMC) is
    //    installed on the object and it goes on the stack.
    cast_with_decision(&mut runner, spell, AlternativeCastDecision::Alternative);
    let cleave_filter = destroy_all_filter(cast_spell_object(&runner, spell)).clone();
    assert!(
        !filter_has_cmc(&cleave_filter),
        "precondition: cleave-cast object must carry the bracket-removed (no CMC) DestroyAll, got {cleave_filter:?}"
    );

    // 2. Resolve the sorcery — it leaves the stack for the graveyard.
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "the resolved cleave sorcery must be in the graveyard"
    );

    // 3. Regrowth-style recursion: return the SAME object id from graveyard to
    //    hand via `move_to_zone` (the recursion path that does NOT re-project
    //    the printed face). Per CR 702.148a the cleave text-change must already
    //    have ended on the stack-exit in step 2.
    let owner = runner.state().objects[&spell].owner;
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), spell, Zone::Hand, &mut events);
    assert_eq!(
        runner.state().objects[&spell].owner,
        owner,
        "object identity must be preserved across the graveyard→hand move"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "the card must be back in hand for the recast"
    );

    // 4. Recast at the PRINTED (normal) mana cost. The printed text must be
    //    restored: DestroyAll must carry the CMC<=2 restriction again.
    cast_with_decision(&mut runner, spell, AlternativeCastDecision::Normal);
    let recast_filter = destroy_all_filter(cast_spell_object(&runner, spell)).clone();
    assert!(
        filter_has_cmc(&recast_filter),
        "CR 702.148a: after a cleave cast resolves and the card returns to hand, \
         a normal-cost recast must restore the printed CMC<=2 DestroyAll \
         restriction (the cleave text-change ended on stack exit), got {recast_filter:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. Winged Portent: base draw = flyers-you-control; cleave = all creatures.
// ---------------------------------------------------------------------------

fn draw_count_filter(obj: &GameObject) -> TargetFilter {
    use engine::types::ability::{QuantityExpr, QuantityRef};
    match obj.abilities.first().map(|a| a.effect.as_ref()) {
        Some(Effect::Draw { count, .. }) => match count {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => filter.clone(),
            other => panic!("expected ObjectCount draw quantity, got {other:?}"),
        },
        other => panic!("expected Draw effect, got {other:?}"),
    }
}

fn filter_has_flying(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(tf)
            if tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::WithKeyword { .. }
            ))
    )
}

#[test]
fn winged_portent_base_counts_flyers_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = cleave_spell_in_hand(
        &mut scenario,
        "Winged Portent",
        WINGED_PORTENT,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
            generic: 4,
        },
        true,
    );
    let mut runner = scenario.build();
    flood_mana(&mut runner);

    cast_with_decision(&mut runner, spell, AlternativeCastDecision::Normal);
    let filter = draw_count_filter(cast_spell_object(&runner, spell));
    assert!(
        filter_has_flying(&filter),
        "base mode: draw count must filter to flyers you control, got {filter:?}"
    );
}

#[test]
fn winged_portent_cleave_counts_all_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = cleave_spell_in_hand(
        &mut scenario,
        "Winged Portent",
        WINGED_PORTENT,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
            generic: 4,
        },
        true,
    );
    let mut runner = scenario.build();
    flood_mana(&mut runner);

    cast_with_decision(&mut runner, spell, AlternativeCastDecision::Alternative);
    let filter = draw_count_filter(cast_spell_object(&runner, spell));
    assert!(
        !filter_has_flying(&filter),
        "cleave mode: draw count must drop the flying filter (all creatures), got {filter:?}"
    );
}

// ---------------------------------------------------------------------------
// 6. Dig Up: base search basic-land + reveal; cleave any card + no reveal.
// ---------------------------------------------------------------------------

fn search_effect(obj: &GameObject) -> (TargetFilter, bool) {
    match obj.abilities.first().map(|a| a.effect.as_ref()) {
        Some(Effect::SearchLibrary { filter, reveal, .. }) => (filter.clone(), *reveal),
        other => panic!("expected SearchLibrary effect, got {other:?}"),
    }
}

fn filter_is_basic_land(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(tf)
            if tf.type_filters.iter().any(|t| matches!(t, TypeFilter::Land))
            && tf.properties.iter().any(|p| matches!(p, FilterProp::HasSupertype { .. }))
    )
}

#[test]
fn dig_up_base_searches_basic_land_and_reveals() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = cleave_spell_in_hand(
        &mut scenario,
        "Dig Up",
        DIG_UP,
        ManaCost::Cost {
            shards: vec![
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Green,
            ],
            generic: 1,
        },
        true,
    );
    let mut runner = scenario.build();
    flood_mana(&mut runner);

    cast_with_decision(&mut runner, spell, AlternativeCastDecision::Normal);
    let (filter, reveal) = search_effect(cast_spell_object(&runner, spell));
    assert!(
        filter_is_basic_land(&filter),
        "base mode: search must be restricted to basic land cards, got {filter:?}"
    );
    assert!(reveal, "base mode: the found card must be revealed");
}

#[test]
fn dig_up_cleave_searches_any_card_without_reveal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = cleave_spell_in_hand(
        &mut scenario,
        "Dig Up",
        DIG_UP,
        ManaCost::Cost {
            shards: vec![
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Green,
            ],
            generic: 1,
        },
        true,
    );
    let mut runner = scenario.build();
    flood_mana(&mut runner);

    cast_with_decision(&mut runner, spell, AlternativeCastDecision::Alternative);
    let (filter, reveal) = search_effect(cast_spell_object(&runner, spell));
    assert!(
        !filter_is_basic_land(&filter),
        "cleave mode: search must NOT be restricted to basic lands, got {filter:?}"
    );
    assert!(
        !reveal,
        "cleave mode: the 'reveal it' step is removed — no reveal"
    );
    // The dropped target marker (`select_first_target_and_resolve`) keeps clippy
    // from flagging the import as unused while documenting the settle helper.
    let _ = select_first_target_and_resolve;
}

// ---------------------------------------------------------------------------
// 7. Synthesis: the parsed cleave_variant RevealHand carries NO CMC, while the
//    base abilities carry CMC<=2 AND end in DiscardCard (natural, un-clobbered
//    chain). Verified on the projected GameObject (no cast required).
// ---------------------------------------------------------------------------

fn reveal_hand_filter(
    effect_chain: &engine::types::ability::AbilityDefinition,
) -> Option<TargetFilter> {
    match effect_chain.effect.as_ref() {
        Effect::RevealHand { card_filter, .. } => Some(card_filter.clone()),
        _ => effect_chain
            .sub_ability
            .as_deref()
            .and_then(reveal_hand_filter),
    }
}

fn chain_ends_in_discard(effect_chain: &engine::types::ability::AbilityDefinition) -> bool {
    match effect_chain.effect.as_ref() {
        Effect::DiscardCard { .. } => true,
        _ => effect_chain
            .sub_ability
            .as_deref()
            .is_some_and(chain_ends_in_discard),
    }
}

#[test]
fn dread_fugue_synthesis_base_has_cmc_and_discard_cleave_drops_cmc() {
    let (runner, fugue) = setup_dread_fugue(&[]);
    let obj = &runner.state().objects[&fugue];

    // Base: nonland + CMC<=2, chain ends in DiscardCard.
    let base = obj.abilities.first().expect("base spell ability");
    let base_filter = reveal_hand_filter(base).expect("base RevealHand");
    assert!(
        filter_has_cmc(&base_filter),
        "base abilities must carry CMC<=2, got {base_filter:?}"
    );
    assert!(
        chain_ends_in_discard(base),
        "base ability chain must end in DiscardCard (un-clobbered)"
    );

    // Cleave variant: nonland with NO CMC, still ends in DiscardCard.
    let variant = obj
        .cleave_variant
        .as_ref()
        .expect("projected cleave_variant");
    let cleave_def = variant.abilities.first().expect("cleave spell ability");
    let cleave_filter = reveal_hand_filter(cleave_def).expect("cleave RevealHand");
    assert!(
        !filter_has_cmc(&cleave_filter),
        "cleave variant must drop the CMC restriction, got {cleave_filter:?}"
    );
    assert!(
        chain_ends_in_discard(cleave_def),
        "cleave ability chain must also end in DiscardCard"
    );
}
