#![allow(unused_imports)]
use super::*;

use crate::support::shared_card_db;

use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, EffectScope, FilterProp,
    ReplacementCondition, ReplacementDefinition, ResolvedAbility, TapStateChange, TargetFilter,
    TargetRef, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::ReplacementChoiceKind;
use engine::types::identifiers::CardId;
use engine::types::proposed_event::EtbTapState;
use engine::types::replacements::ReplacementEvent;

/// Build a fast land replacement definition matching
/// "This land enters tapped unless you control two or fewer other lands."
fn fast_land_replacement(description: &str) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .description(description.to_string())
        .condition(ReplacementCondition::UnlessControlsOtherLeq {
            count: 2,
            filter: TypedFilter::new(engine::types::ability::TypeFilter::Land)
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Another]),
        })
}

fn replacement_choice_index(runner: &GameRunner, description: &str) -> usize {
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "expected ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };

    candidates
        .iter()
        .position(|candidate| candidate.description.contains(description))
        .unwrap_or_else(|| panic!("replacement choice {description:?} not found in {candidates:?}"))
}

// ── Fast land integration tests ──

/// CR 305.7 + CR 614.1c: Fast land with 0 other lands → enters untapped.
#[test]
fn fast_land_zero_other_lands_enters_untapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut builder = scenario.add_land_to_hand(P0, "Spirebluff Canal");
    builder.with_replacement_definition(fast_land_replacement(
        "This land enters tapped unless you control two or fewer other lands.",
    ));
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        !obj.tapped,
        "Fast land should enter untapped with 0 other lands"
    );
}

/// CR 305.7 + CR 614.1c: Fast land with exactly 2 other lands → enters untapped (boundary).
#[test]
fn fast_land_two_other_lands_enters_untapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two lands already on the battlefield (controlled by P0)
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);

    let mut builder = scenario.add_land_to_hand(P0, "Spirebluff Canal");
    builder.with_replacement_definition(fast_land_replacement(
        "This land enters tapped unless you control two or fewer other lands.",
    ));
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        !obj.tapped,
        "Fast land should enter untapped with exactly 2 other lands (boundary)"
    );
}

/// CR 305.7 + CR 614.1c: Fast land with 3 other lands → enters tapped (boundary).
#[test]
fn fast_land_three_other_lands_enters_tapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Three lands already on the battlefield (controlled by P0)
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::White);

    let mut builder = scenario.add_land_to_hand(P0, "Spirebluff Canal");
    builder.with_replacement_definition(fast_land_replacement(
        "This land enters tapped unless you control two or fewer other lands.",
    ));
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        obj.tapped,
        "Fast land should enter tapped with 3 other lands"
    );
}

/// CR 305.7 + CR 614.1c: Opponent's lands do NOT count for "you control" check.
/// 3 lands total on battlefield but only 2 controlled by P0 → enters untapped.
#[test]
fn fast_land_opponent_lands_not_counted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two lands controlled by P0
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    // One land controlled by P1 (should NOT count)
    scenario.add_basic_land(P1, engine::types::mana::ManaColor::Green);

    let mut builder = scenario.add_land_to_hand(P0, "Spirebluff Canal");
    builder.with_replacement_definition(fast_land_replacement(
        "This land enters tapped unless you control two or fewer other lands.",
    ));
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        !obj.tapped,
        "Fast land should enter untapped — opponent's lands don't count"
    );
}

/// CR 305.7 + CR 614.1c: The entering land itself must NOT be counted
/// in the "other" check.
#[test]
fn fast_land_self_not_counted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Exactly 2 other lands — the entering land makes 3 on the battlefield,
    // but "other" means it must not count itself.
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);

    let mut builder = scenario.add_land_to_hand(P0, "Spirebluff Canal");
    builder.with_replacement_definition(fast_land_replacement(
        "This land enters tapped unless you control two or fewer other lands.",
    ));
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        !obj.tapped,
        "Fast land must not count itself in 'other lands' check — 2 other lands ≤ 2 → untapped"
    );
}

#[test]
fn spelunking_order_can_leave_tapland_tapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Spelunking", 0, 4)
        .as_enchantment()
        .from_oracle_text("Lands you control enter untapped.");

    let land_id = scenario
        .add_land_to_hand(P0, "Guildgate")
        .from_oracle_text("This land enters tapped.")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");
    // #505 (CR 616.1): competing-replacement candidates are labelled by their
    // outcome (`replacement_choice_label`), not raw Oracle text. Spelunking's
    // grant is an `Untap` SelfRef replacement → "Enters untapped".
    let spelunking_first = replacement_choice_index(&runner, "Enters untapped");
    runner
        .act(GameAction::ChooseReplacement {
            index: spelunking_first,
        })
        .expect("replacement choice should resolve");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        obj.tapped,
        "Choosing Spelunking first should leave the tapland tapped"
    );
}

#[test]
fn spelunking_order_can_leave_tapland_untapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Spelunking", 0, 4)
        .as_enchantment()
        .from_oracle_text("Lands you control enter untapped.");

    let land_id = scenario
        .add_land_to_hand(P0, "Guildgate")
        .from_oracle_text("This land enters tapped.")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");
    // #505 (CR 616.1): the tapland's own `Tap` SelfRef replacement is labelled
    // by its outcome → "Enters tapped" (distinct from Spelunking's "Enters
    // untapped" candidate, so the substring uniquely identifies it).
    let tapland_first = replacement_choice_index(&runner, "Enters tapped");
    runner
        .act(GameAction::ChooseReplacement {
            index: tapland_first,
        })
        .expect("replacement choice should resolve");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        !obj.tapped,
        "Choosing the tapland replacement first should let Spelunking untap it"
    );
}

#[test]
fn archelos_untapped_makes_other_taplands_enter_untapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Archelos, Lagoon Mystic", 2, 4)
        .from_oracle_text(
            "As long as ~ is untapped, other permanents enter untapped.\nAs long as ~ is tapped, other permanents enter tapped.",
        );

    let land_id = scenario
        .add_land_to_hand(P0, "Guildgate")
        .from_oracle_text("This land enters tapped.")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");
    // #505 (CR 616.1): the tapland's own `Tap` SelfRef replacement is labelled
    // by its outcome → "Enters tapped" (distinct from Archelos's "Enters
    // untapped" candidate, so the substring uniquely identifies it).
    let tapland_first = replacement_choice_index(&runner, "Enters tapped");
    runner
        .act(GameAction::ChooseReplacement {
            index: tapland_first,
        })
        .expect("replacement choice should resolve");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        !obj.tapped,
        "Untapped Archelos should let other permanents enter untapped"
    );
}

// ── Karoo self-ETB cost land integration tests ──

const KAROO_LOTUS_VALE: &str = "If this land would enter, sacrifice two untapped \
    lands instead. If you do, put this land onto the battlefield. If you don't, \
    put it into its owner's graveyard.";

/// CR 614.12a: declining a Karoo land's `MayCost` cost redirects the ETB to the
/// owner's graveyard — the land never appears on the battlefield.
#[test]
fn karoo_land_decline_routes_to_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let land_id = scenario
        .add_land_to_hand(P0, "Lotus Vale")
        .from_oracle_text(KAROO_LOTUS_VALE)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let decline = replacement_choice_index(&runner, "Decline");
    runner
        .act(GameAction::ChooseReplacement { index: decline })
        .expect("declining the Karoo cost should resolve");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(
        obj.zone,
        Zone::Graveyard,
        "a declined Karoo land must be routed to its owner's graveyard"
    );
}

/// CR 614.12a: accepting a Karoo land's cost when it is unpayable (no untapped
/// lands to sacrifice) falls through to the decline branch — the land still
/// goes to the graveyard, never the battlefield.
#[test]
fn karoo_land_accept_with_unpayable_cost_routes_to_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let land_id = scenario
        .add_land_to_hand(P0, "Lotus Vale")
        .from_oracle_text(KAROO_LOTUS_VALE)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    // Accept (index 0) — but no untapped lands exist to sacrifice.
    let accept = replacement_choice_index(&runner, "Sacrifice");
    runner
        .act(GameAction::ChooseReplacement { index: accept })
        .expect("accepting the Karoo cost should resolve");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(
        obj.zone,
        Zone::Graveyard,
        "an unpayable Karoo cost must fall through to the graveyard redirect"
    );
}

#[test]
fn archelos_tapped_makes_other_lands_enter_tapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archelos_id = scenario
        .add_creature(P0, "Archelos, Lagoon Mystic", 2, 4)
        .from_oracle_text(
            "As long as ~ is untapped, other permanents enter untapped.\nAs long as ~ is tapped, other permanents enter tapped.",
        )
        .id();

    let land_id = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&archelos_id)
        .unwrap()
        .tapped = true;

    let card_id = runner.state().objects[&land_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        obj.tapped,
        "Tapped Archelos should make other permanents enter tapped"
    );
}

// ── Turbulent land cycle integration tests (SOC) ──
// "This land enters tapped unless your opponents control eight or more lands."

/// Build the Turbulent land replacement matching CR 614.1d with
/// `UnlessControlsCountMatching { minimum: 8 }` and `ControllerRef::Opponent`.
fn turbulent_land_replacement(description: &str) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .description(description.to_string())
        .condition(ReplacementCondition::UnlessControlsCountMatching {
            minimum: 8,
            filter: TargetFilter::Typed(
                TypedFilter::new(engine::types::ability::TypeFilter::Land)
                    .controller(ControllerRef::Opponent),
            ),
        })
}

/// CR 614.1d: Turbulent Fen with opponent controlling fewer than 8 lands → enters tapped.
#[test]
fn turbulent_land_opponent_under_threshold_enters_tapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Opponent controls 7 lands — threshold not met → replacement applies, land enters tapped.
    for _ in 0..7 {
        scenario.add_basic_land(P1, engine::types::mana::ManaColor::Green);
    }

    let mut builder = scenario.add_land_to_hand(P0, "Turbulent Fen");
    builder.with_replacement_definition(turbulent_land_replacement(
        "This land enters tapped unless your opponents control eight or more lands.",
    ));
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        obj.tapped,
        "Turbulent Fen should enter tapped when opponents control only 7 lands"
    );
}

/// CR 614.1d: Turbulent Fen with opponent controlling ≥8 lands → enters untapped.
#[test]
fn turbulent_land_opponent_meets_threshold_enters_untapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Opponent controls 8 lands — threshold met → replacement suppressed, land enters untapped.
    for _ in 0..8 {
        scenario.add_basic_land(P1, engine::types::mana::ManaColor::Green);
    }

    let mut builder = scenario.add_land_to_hand(P0, "Turbulent Fen");
    builder.with_replacement_definition(turbulent_land_replacement(
        "This land enters tapped unless your opponents control eight or more lands.",
    ));
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        !obj.tapped,
        "Turbulent Fen should enter untapped when opponents control 8 lands"
    );
}

/// CR 614.1d + CR 109.5: Lands controlled by the Turbulent land's controller must NOT
/// count toward the "your opponents control" threshold.
#[test]
fn turbulent_land_own_lands_do_not_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Controller has 8 lands, opponent has 0 — threshold must NOT be met.
    for _ in 0..8 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    }

    let mut builder = scenario.add_land_to_hand(P0, "Turbulent Fen");
    builder.with_replacement_definition(turbulent_land_replacement(
        "This land enters tapped unless your opponents control eight or more lands.",
    ));
    let land_id = builder.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land_id].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(
        obj.tapped,
        "Turbulent Fen must not count controller's lands against the opponent threshold"
    );
}

// ---------------------------------------------------------------------------
// CR 614.12 + CR 712.14a: a transformed battlefield entry consults the back
// face's own replacement definitions, not the stored front face's. Effect
// route (`ChangeZone { enter_transformed: true }`) — real cards from the
// committed integration fixture.
// ---------------------------------------------------------------------------

fn add_red_mana(runner: &mut GameRunner, count: u32) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..count {
        pool.add(engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Red,
            dummy,
            false,
            vec![],
        ));
    }
}

fn resolve_transformed_entry_under(
    runner: &mut GameRunner,
    object_id: ObjectId,
    enters_under: Option<ControllerRef>,
) {
    let resolved = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Battlefield,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: true,
            enters_under,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![TargetRef::Object(object_id)],
        object_id,
        P0,
    );
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("transformed battlefield entry resolves");
}

fn resolve_transformed_entry(runner: &mut GameRunner, object_id: ObjectId) {
    resolve_transformed_entry_under(runner, object_id, None);
}

/// (CR 614.12 + CR 712.14a): a back face's own mandatory as-enters counter
/// replacement applies on the effect route — Ral, Monsoon Mage returned
/// transformed enters with its back face's loyalty bonus on top of printed
/// loyalty, not the front face's (nonexistent) replacement.
#[test]
fn ral_returned_transformed_applies_back_face_loyalty_replacement() {
    let db = shared_card_db().expect("integration fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ral = scenario.add_real_card(P0, "Ral, Monsoon Mage", Zone::Exile, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let mut runner = scenario.build();
    add_red_mana(&mut runner, 1);

    runner.cast(bolt).target_player(P1).resolve();

    // Reach guards: one instant was cast this turn, and the back face carries
    // exactly one `Moved` replacement (the loyalty bonus) while the front
    // face carries none.
    assert_eq!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P0)
            .map(|v| v.len())
            .unwrap_or(0),
        1
    );
    {
        let obj = &runner.state().objects[&ral];
        assert_eq!(obj.replacement_definitions.len(), 0);
        let back = obj.back_face.as_ref().expect("Ral must have a back face");
        assert_eq!(
            back.replacement_definitions
                .iter_unchecked()
                .filter(|def| def.event == ReplacementEvent::Moved)
                .count(),
            1
        );
    }

    resolve_transformed_entry(&mut runner, ral);

    let obj = &runner.state().objects[&ral];
    assert_eq!(obj.name, "Ral, Leyline Prodigy");
    assert!(obj.transformed);
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "CR 614.12 + CR 712.14a: the transformed entry must deliver Ral to the battlefield"
    );
    assert_eq!(
        obj.counters.get(&CounterType::Loyalty).copied(),
        Some(3),
        "CR 614.12 + CR 712.14a: printed loyalty 2 plus the back face's own \
         replacement (+1 for the instant cast this turn)"
    );
}

/// (CR 614.12 + CR 110.2a + CR 108.4a) a back-face mandatory as-enters counter
/// replacement reads "you" as the entering controller, not the owner, when an
/// effect returns another player's double-faced card to the battlefield
/// transformed under a different player's control.
#[test]
fn ral_returned_transformed_under_non_owner_counts_entering_controllers_spells() {
    let db = shared_card_db().expect("integration fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ral = scenario.add_real_card(P1, "Ral, Monsoon Mage", Zone::Exile, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let mut runner = scenario.build();
    add_red_mana(&mut runner, 1);

    runner.cast(bolt).target_player(P1).resolve();

    // Reach guards: P0 (the entering controller) cast one instant this turn,
    // P1 (the owner) cast none, Ral's owner is P1, and the back face carries
    // exactly one `Moved` replacement (the loyalty bonus).
    assert_eq!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P0)
            .map_or(0, |v| v.len()),
        1
    );
    assert_eq!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P1)
            .map_or(0, |v| v.len()),
        0
    );
    assert_eq!(runner.state().objects[&ral].owner, P1);
    assert_eq!(
        runner.state().objects[&ral].controller,
        P1,
        "reach guard: Ral's stored controller field is the owner while it sits off the \
         battlefield, distinct from the entering controller the transformed entry installs"
    );
    {
        let obj = &runner.state().objects[&ral];
        let back = obj.back_face.as_ref().expect("Ral must have a back face");
        assert_eq!(
            back.replacement_definitions
                .iter_unchecked()
                .filter(|def| def.event == ReplacementEvent::Moved)
                .count(),
            1
        );
    }

    resolve_transformed_entry_under(&mut runner, ral, Some(ControllerRef::You));

    let obj = &runner.state().objects[&ral];
    assert_eq!(obj.name, "Ral, Leyline Prodigy");
    assert!(obj.transformed);
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "CR 614.12 + CR 712.14a: the transformed entry must deliver Ral to the battlefield"
    );
    assert_eq!(obj.owner, P1);
    assert_eq!(
        obj.controller, P0,
        "reach guard: the enters_under override must deliver Ral under the entering \
         controller, not its owner"
    );
    assert_eq!(
        obj.counters.get(&CounterType::Loyalty).copied(),
        Some(3),
        "CR 614.12: printed 2 + 1 for the instant the ENTERING controller (P0) cast; \
         the owner (P1) cast none"
    );
}

/// (CR 614.12 + CR 616.1f + CR 110.2a): a back-face dynamic counter replacement
/// that reads "you" sees the controller an earlier entry-controller replacement
/// installed, not the controller the entry started with. Ral, Monsoon Mage is
/// P1's card returned transformed with no controller instruction (so the entry
/// starts under its owner P1), and its back face additionally carries the
/// "enters under the control of an opponent of your choice" replacement, which
/// hands the permanent to P0. The instant P0 cast counts; P1 cast none.
/// CR 616.1b: the controller replacement must be chosen first, so it applies
/// without a prompt.
#[test]
fn ral_returned_transformed_counts_spells_of_controller_set_by_earlier_entry_replacement() {
    let db = shared_card_db().expect("integration fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ral = scenario.add_real_card(P1, "Ral, Monsoon Mage", Zone::Exile, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let mut runner = scenario.build();
    add_red_mana(&mut runner, 1);

    runner.cast(bolt).target_player(P1).resolve();

    runner
        .state_mut()
        .objects
        .get_mut(&ral)
        .expect("Ral exists")
        .back_face
        .as_mut()
        .expect("Ral must have a back face")
        .replacement_definitions
        .push(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield)
                .enters_under(ControllerRef::Opponent)
                .description("Enters under the control of an opponent of your choice.".to_string()),
        );

    // Reach guards: P0 cast one instant this turn and P1 none, Ral is P1's
    // card with P1 as its stored controller (so the entry starts under P1), and
    // the back face carries two `Moved` replacements (the loyalty bonus and the
    // controller override).
    let spells_cast = |runner: &GameRunner, player| {
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&player)
            .map_or(0, |v| v.len())
    };
    assert_eq!(spells_cast(&runner, P0), 1);
    assert_eq!(spells_cast(&runner, P1), 0);
    assert_eq!(runner.state().objects[&ral].owner, P1);
    assert_eq!(runner.state().objects[&ral].controller, P1);
    {
        let obj = &runner.state().objects[&ral];
        let back = obj.back_face.as_ref().expect("Ral must have a back face");
        assert_eq!(
            back.replacement_definitions
                .iter_unchecked()
                .filter(|def| def.event == ReplacementEvent::Moved)
                .count(),
            2
        );
    }

    resolve_transformed_entry_under(&mut runner, ral, None);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "CR 616.1b: the counter replacement is never offered before the controller replacement, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().pending_replacement.is_none());
    for index in 0..2 {
        assert!(
            runner
                .act(GameAction::ChooseReplacement { index })
                .is_err(),
            "no CR 616.1 choice is open, so no ordering (counter-first included) is accepted for index {index}"
        );
    }

    let obj = &runner.state().objects[&ral];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.transformed);
    assert_eq!(obj.name, "Ral, Leyline Prodigy");
    assert_eq!(obj.owner, P1);
    assert_eq!(
        obj.controller, P0,
        "reach guard: the entry-controller replacement must deliver Ral under P0"
    );
    assert_eq!(
        obj.counters.get(&CounterType::Loyalty).copied(),
        Some(3),
        "CR 614.12 + CR 616.1f: printed 2 + 1 for the instant P0, the controller the \
         earlier replacement installed, cast; the entry's starting controller P1 cast none"
    );
}

/// (CR 616.1b + CR 616.1f + CR 614.12): when two applicable replacements each modify
/// under whose control Ral, Monsoon Mage enters, the affected player chooses between
/// those two; the back face's own counter replacement is not offered until both have
/// applied. A choice index naming the withheld counter replacement is rejected through
/// the production `GameAction::ChooseReplacement` path, and the controller written last
/// is the one whose cast spells the counter replacement then counts.
#[test]
fn two_entry_controller_replacements_are_ordered_before_the_counter_replacement() {
    let db = shared_card_db().expect("integration fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ral = scenario.add_real_card(P1, "Ral, Monsoon Mage", Zone::Exile, db);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let mut runner = scenario.build();
    add_red_mana(&mut runner, 1);

    runner.cast(bolt).target_player(P1).resolve();

    {
        let back_face_defs = &mut runner
            .state_mut()
            .objects
            .get_mut(&ral)
            .expect("Ral exists")
            .back_face
            .as_mut()
            .expect("Ral must have a back face")
            .replacement_definitions;
        for controller in [ControllerRef::Opponent, ControllerRef::You] {
            back_face_defs.push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .valid_card(TargetFilter::SelfRef)
                    .destination_zone(Zone::Battlefield)
                    .enters_under(controller)
                    .description("Enters under a controller.".to_string()),
            );
        }
    }

    // Reach guards: three `Moved` back-face replacements (the loyalty bonus and the
    // two controller overrides), P0 cast one instant this turn and P1 none.
    let spells_cast = |runner: &GameRunner, player| {
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&player)
            .map_or(0, |v| v.len())
    };
    assert_eq!(spells_cast(&runner, P0), 1);
    assert_eq!(spells_cast(&runner, P1), 0);
    {
        let obj = &runner.state().objects[&ral];
        let back = obj.back_face.as_ref().expect("Ral must have a back face");
        assert_eq!(
            back.replacement_definitions
                .iter_unchecked()
                .filter(|def| def.event == ReplacementEvent::Moved)
                .count(),
            3
        );
    }

    resolve_transformed_entry_under(&mut runner, ral, None);

    let WaitingFor::ReplacementChoice {
        candidate_count,
        kind,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a choice between the two controller replacements, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*candidate_count, 2);
    assert_eq!(*kind, ReplacementChoiceKind::Order);

    let offered = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the parked entry keeps its pending replacement")
        .candidates
        .clone();
    let controller_of =
        |runner: &GameRunner, rid: &engine::types::proposed_event::ReplacementId| {
            runner
                .state()
                .entering_or_live_object(ral)
                .and_then(|o| o.replacement_definitions.get(rid.index))
                .and_then(|def| def.enters_under.clone())
        };
    assert_eq!(offered.len(), 2);
    assert!(
        offered
            .iter()
            .all(|rid| rid.source == ral && controller_of(&runner, rid).is_some()),
        "CR 616.1b: only the controller replacements are offered, not the counter replacement"
    );

    // The withheld counter replacement has no acceptable index.
    assert!(
        runner
            .act(GameAction::ChooseReplacement { index: 2 })
            .is_err(),
        "an index past the two offered controller replacements is rejected"
    );
    let WaitingFor::ReplacementChoice {
        candidate_count,
        kind,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the rejected index leaves the choice open, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*candidate_count, 2);
    assert_eq!(*kind, ReplacementChoiceKind::Order);
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .map(|pending| pending.candidates.clone()),
        Some(offered.clone())
    );
    assert_eq!(runner.state().objects[&ral].zone, Zone::Exile);

    let you_index = offered
        .iter()
        .position(|rid| controller_of(&runner, rid) == Some(ControllerRef::You))
        .expect("the `You` controller replacement is offered");
    runner
        .act(GameAction::ChooseReplacement { index: you_index })
        .expect("choosing among the offered controller replacements resolves");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the remaining controller replacement and the counter replacement apply without a further choice, got {:?}",
        runner.state().waiting_for
    );
    let obj = &runner.state().objects[&ral];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.transformed);
    assert_eq!(obj.name, "Ral, Leyline Prodigy");
    let final_controller = obj.controller;
    let loyalty = obj.counters.get(&CounterType::Loyalty).copied();
    assert_eq!(
        loyalty,
        Some(2 + spells_cast(&runner, final_controller) as u32),
        "CR 614.12: the counter replacement counts the spells of the controller that the controller replacements installed"
    );
    assert_eq!(
        final_controller, P0,
        "`You` applies first, then `Opponent` last, so Ral ends under P0"
    );
    assert_eq!(loyalty, Some(3));
}

/// (CR 614.12 + CR 714.3a): a back-face Saga entering transformed gets
/// exactly one lore counter, from its own replacement applied through the
/// CR 614.12 transformed-entry projection; the transformed-entry seeding
/// (`intrinsic_face_entry_counters`) adds no lore counter of its own.
#[test]
fn sheoldred_returns_true_scriptures_with_one_lore_counter() {
    let db = shared_card_db().expect("integration fixture must be present");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sheoldred = scenario.add_real_card(P0, "Sheoldred", Zone::Exile, db);
    let mut runner = scenario.build();

    {
        let obj = &runner.state().objects[&sheoldred];
        let back = obj
            .back_face
            .as_ref()
            .expect("Sheoldred must have a back face");
        assert_eq!(back.name, "The True Scriptures");
        assert!(back.card_types.subtypes.iter().any(|s| s == "Saga"));
        assert!(back.replacement_definitions.iter_unchecked().any(|def| {
            def.event == ReplacementEvent::Moved
                && matches!(
                    def.execute.as_deref().map(|a| a.effect.as_ref()),
                    Some(Effect::PutCounter {
                        counter_type: CounterType::Lore,
                        ..
                    })
                )
        }));
    }

    resolve_transformed_entry(&mut runner, sheoldred);

    let obj = &runner.state().objects[&sheoldred];
    assert!(obj.transformed);
    assert_eq!(obj.name, "The True Scriptures");
    assert!(obj.card_types.subtypes.iter().any(|s| s == "Saga"));
    assert_eq!(
        obj.counters.get(&CounterType::Lore).copied(),
        Some(1),
        "CR 714.3a + CR 614.12: exactly one lore counter, from the back \
         face's own replacement"
    );
}
