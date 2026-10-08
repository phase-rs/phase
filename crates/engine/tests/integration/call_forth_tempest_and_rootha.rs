//! Issue #6865 — runtime coverage for spell-history mana-value aggregates.

use engine::game::restrictions::record_spell_cast_from_zone;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::Effect;
use engine::types::actions::{CastChoice, GameAction};
use engine::types::game_state::{CastingVariant, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{ResolvedRulesCommand, ResolvedStackPushOrigin};
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";

fn colorless_pool(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn call_forth_pool(generic: usize) -> Vec<ManaUnit> {
    let mut pool = colorless_pool(generic);
    pool.extend((0..3).map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])));
    pool
}

fn add_filler_spell(
    scenario: &mut GameScenario,
    name: &str,
    is_instant: bool,
    mana_value: u32,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, is_instant, "Target player gains 1 life.")
        .with_mana_cost(ManaCost::generic(mana_value))
        .id()
}

fn add_call_forth(
    scenario: &mut GameScenario,
    db: &engine::database::card_db::CardDatabase,
) -> ObjectId {
    scenario.add_real_card(P0, "Call Forth the Tempest", Zone::Hand, db)
}

fn build_rehydrated(
    scenario: GameScenario,
    db: &engine::database::card_db::CardDatabase,
) -> GameRunner {
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
}

fn cast_filler(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).target_player(P0).resolve();
}

#[test]
fn call_forth_damages_each_opponent_creature_for_prior_cast_mana_value_sum() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let three = add_filler_spell(&mut scenario, "Three-Mana Instant", true, 3);
    let five = add_filler_spell(&mut scenario, "Five-Mana Sorcery", false, 5);
    let call_forth = add_call_forth(&mut scenario, db);
    let own = scenario.add_creature(P0, "Own Survivor", 2, 20).id();
    let opponent_one = scenario
        .add_creature(P1, "Opponent One Survivor", 2, 20)
        .id();
    let opponent_two = scenario
        .add_creature(P2, "Opponent Two Survivor", 2, 20)
        .id();
    scenario.with_mana_pool(P0, call_forth_pool(13));

    let mut runner = build_rehydrated(scenario, db);
    cast_filler(&mut runner, three);
    cast_filler(&mut runner, five);
    let outcome = runner.cast(call_forth).resolve();
    let state = outcome.state();

    assert_eq!(
        state.spells_cast_this_turn_by_player[&P0].len(),
        3,
        "reach guard: both prior spells and Call Forth must be journaled"
    );
    assert_eq!(state.objects[&own].damage_marked, 0);
    assert_eq!(state.objects[&opponent_one].damage_marked, 8);
    assert_eq!(state.objects[&opponent_two].damage_marked, 8);
}

#[test]
fn call_forth_zero_prior_spells_resolves_with_zero_damage() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let call_forth = add_call_forth(&mut scenario, db);
    let opponent = scenario.add_creature(P1, "Untouched Survivor", 2, 20).id();
    scenario.with_mana_pool(P0, call_forth_pool(5));

    let mut runner = build_rehydrated(scenario, db);
    let outcome = runner.cast(call_forth).resolve();
    let state = outcome.state();

    assert_eq!(
        state.spells_cast_this_turn_by_player[&P0].len(),
        1,
        "Call Forth itself must be present so zero proves current-cast exclusion"
    );
    assert_eq!(state.objects[&opponent].damage_marked, 0);
    assert_eq!(state.objects[&call_forth].zone, Zone::Graveyard);
}

#[test]
fn call_forth_real_card_cast_puts_two_cascade_triggers_on_the_stack() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let call_forth = add_call_forth(&mut scenario, db);
    scenario.with_mana_pool(P0, call_forth_pool(5));

    let mut runner = build_rehydrated(scenario, db);
    {
        let _committed = runner.cast(call_forth).commit();
    }
    let state = runner.state();
    assert_eq!(
        state.spells_cast_this_turn_by_player[&P0].len(),
        1,
        "the real Call Forth card must complete the production cast ledger"
    );
    assert_eq!(
        state
            .stack
            .iter()
            .filter(|entry| matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { ability, .. }
                    if matches!(ability.effect, Effect::Cascade)
            ))
            .count(),
        2,
        "CR 702.85c: the real card's two Cascade instances must trigger separately"
    );
}

#[test]
fn call_forth_same_object_id_recast_counts_prior_occurrence_only() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let recast = scenario
        .add_creature_to_hand(P0, "Recast Creature", 2, 20)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Production Bounce",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let call_forth = add_call_forth(&mut scenario, db);
    let opponent = scenario
        .add_creature(P1, "Eight-Damage Survivor", 2, 20)
        .id();
    scenario.with_mana_pool(P0, call_forth_pool(13));

    let mut runner = build_rehydrated(scenario, db);
    runner.cast(recast).resolve();
    runner.cast(bounce).target_object(recast).resolve();
    assert_eq!(runner.state().objects[&recast].zone, Zone::Hand);
    runner.cast(recast).resolve();

    let records = &runner.state().spells_cast_this_turn_by_player[&P0];
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].spell_object_id, Some(recast));
    assert_eq!(records[2].spell_object_id, Some(recast));

    let outcome = runner.cast(call_forth).resolve();
    assert_eq!(
        outcome.state().objects[&opponent].damage_marked,
        8,
        "3 + 2 + 3 proves distinct same-ObjectId cast occurrences all contribute"
    );
}

#[test]
fn call_forth_counts_cascade_casts_but_not_uncast_spell_copies() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let copyable = scenario
        .add_spell_to_hand_from_oracle(P0, "Copyable Three", true, "You gain 1 life.")
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let cascade = scenario
        .add_spell_to_hand_from_oracle(P0, "Cascade Four", false, "Cascade")
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let cascade_hit = scenario
        .add_spell_to_library_top(P0, "Cascade Hit", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let call_forth = add_call_forth(&mut scenario, db);
    let opponent = scenario.add_creature(P1, "Ten-Damage Survivor", 2, 20).id();
    scenario.with_mana_pool(P0, call_forth_pool(14));

    let mut runner = build_rehydrated(scenario, db);
    {
        let mut committed = runner.cast(copyable).commit();
        committed.cast(twincast).target_object(copyable).resolve();
    }
    assert!(
        runner
            .state()
            .resolved_rules_journal
            .entries()
            .iter()
            .any(|entry| matches!(
                entry.command.as_ref(),
                Some(ResolvedRulesCommand::StackPush(command))
                    if command.origin == ResolvedStackPushOrigin::Copy
            )),
        "Twincast must reach the production CR 707.10 stack-copy path"
    );
    if matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
        runner
            .act(GameAction::KeepAllCopyTargets)
            .expect("keep the targetless copy's targets");
    }
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().spells_cast_this_turn_by_player[&P0].len(),
        2,
        "the copied spell is not cast and must not create a third journal record"
    );

    let cascade_outcome = runner.cast(cascade).resolve();
    assert!(
        matches!(
            cascade_outcome.final_waiting_for(),
            WaitingFor::CastOffer { .. }
        ),
        "Cascade must offer the exiled hit"
    );
    runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        })
        .expect("cast the cascade hit without paying its mana cost");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&cascade_hit].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().spells_cast_this_turn_by_player[&P0].len(),
        4,
        "copyable, Twincast, cascade source, and cascade hit are casts; the copy is not"
    );

    let outcome = runner.cast(call_forth).resolve();
    assert_eq!(
        outcome.state().objects[&opponent].damage_marked,
        10,
        "3 + 2 + 4 + 1 counts the cascade hit but not the uncast MV3 stack copy"
    );
}

#[test]
fn call_forth_each_opponent_fanout_uses_original_controller_journal() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let own_three = add_filler_spell(&mut scenario, "Caster's Three", true, 3);
    let poison_one = scenario
        .add_spell_to_hand_from_oracle(P1, "Opponent Eleven", true, "You gain 1 life.")
        .with_mana_cost(ManaCost::generic(11))
        .id();
    let poison_two = scenario
        .add_spell_to_hand_from_oracle(P2, "Opponent Seventeen", true, "You gain 1 life.")
        .with_mana_cost(ManaCost::generic(17))
        .id();
    let call_forth = add_call_forth(&mut scenario, db);
    let opponent_one = scenario.add_creature(P1, "Opponent One Fanout", 2, 20).id();
    let opponent_two = scenario.add_creature(P2, "Opponent Two Fanout", 2, 20).id();
    scenario.with_mana_pool(P0, call_forth_pool(8));

    let mut runner = build_rehydrated(scenario, db);
    for (player, object_id) in [(P1, poison_one), (P2, poison_two)] {
        let object = runner.state().objects[&object_id].clone();
        record_spell_cast_from_zone(
            runner.state_mut(),
            player,
            &object,
            Zone::Hand,
            CastingVariant::Normal,
        )
        .expect("seed a hostile opponent journal through the ledger authority");
    }
    cast_filler(&mut runner, own_three);
    let outcome = runner.cast(call_forth).resolve();
    let state = outcome.state();

    assert_eq!(state.spells_cast_this_turn_by_player[&P1].len(), 1);
    assert_eq!(state.spells_cast_this_turn_by_player[&P2].len(), 1);
    assert_eq!(state.objects[&opponent_one].damage_marked, 3);
    assert_eq!(state.objects[&opponent_two].damage_marked, 3);
}

#[test]
fn rootha_token_uses_greatest_instant_or_sorcery_cast_mana_value() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Rootha, Mastering the Moment", Zone::Battlefield, db);
    let hostile_creature = scenario
        .add_creature_to_hand(P0, "Seven-Mana Creature", 7, 7)
        .with_mana_cost(ManaCost::generic(7))
        .id();
    let instant = add_filler_spell(&mut scenario, "Two-Mana Instant", true, 2);
    let sorcery = add_filler_spell(&mut scenario, "Four-Mana Sorcery", false, 4);
    scenario.with_mana_pool(P0, colorless_pool(13));

    let mut runner = build_rehydrated(scenario, db);
    runner.cast(hostile_creature).resolve();
    cast_filler(&mut runner, instant);
    cast_filler(&mut runner, sorcery);
    runner.pass_both_players();
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    runner.advance_until_stack_empty();

    let token = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .find(|object| object.is_token && object.name == "Elemental")
        .expect("Rootha's beginning-of-combat trigger must create an Elemental");
    assert_eq!((token.power, token.toughness), (Some(4), Some(4)));
    assert!(token.keywords.contains(&Keyword::Flying));
    assert!(token.keywords.contains(&Keyword::Haste));
}
