//! Issue #9632: resolved "you/players gain hexproof/shroud" spells grant the
//! player hexproof or shroud (CR 702.11c, CR 702.18a) for the stated duration.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::static_abilities::{
    player_cannot_be_targeted_by, player_has_cant_gain_life, player_has_cant_lose_life,
    player_has_hexproof, player_has_shroud,
};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Effect, TargetFilter,
    TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const BLOSSOMING_CALM: &str = "You gain hexproof until your next turn. You gain 2 life.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
const GILDED_LIGHT: &str = "You gain shroud until end of turn. (You can't be the target of spells or abilities.)\nCycling {2} ({2}, Discard this card: Draw a card.)";
const EVERYBODY_LIVES: &str = "All creatures gain hexproof and indestructible until end of turn. Players gain hexproof until end of turn. Players can't lose life this turn and players can't lose the game or win the game this turn.";
const EARTHSHAPE: &str = "Earthbend 3. Then each creature you control with power less than or equal to that land's power gains hexproof and indestructible until end of turn. You gain hexproof until end of turn.";
const FLARE_OF_FORTITUDE: &str = "You may sacrifice a nontoken white creature rather than pay this spell's mana cost.\nUntil end of turn, your life total can't change, and permanents you control gain hexproof and indestructible.";
const PERCH_PROTECTION: &str = "Gift an extra turn (You may promise an opponent a gift as you cast this spell. If you do, they take an extra turn after this one.)\nCreate four 2/2 blue Bird creature tokens with flying. If the gift was promised, all permanents you control phase out, and until your next turn, your life total can't change and you gain protection from everything.\nExile Perch Protection.";
const SURGE_OF_SALVATION: &str = "You and permanents you control gain hexproof until end of turn. Prevent all damage that black and/or red sources would deal to creatures you control this turn.";
const LAZOTEP_PLATING: &str = "Amass Zombies 1. (Put a +1/+1 counter on an Army you control. It's also a Zombie. If you don't control an Army, create a 0/0 black Zombie Army creature token first.)\nYou and permanents you control gain hexproof until end of turn. (You and they can't be the targets of spells or abilities your opponents control.)";
const THOUGHT_SCOUR: &str = "Target player mills two cards.\nDraw a card.";

fn add_free_spell(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    instant: bool,
    oracle: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, instant, oracle)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "spell must finish resolving"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn object_has_keyword(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

fn stock_libraries(scenario: &mut GameScenario) {
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Library Card");
        scenario.add_card_to_library_top(P1, "Library Card");
    }
}

#[test]
fn blossoming_calm_grants_caster_hexproof_until_their_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    // The Oracle-text builder does not extract printed keywords.
    let calm = scenario
        .add_spell_to_hand_from_oracle(P0, "Blossoming Calm", true, BLOSSOMING_CALM)
        .with_mana_cost(ManaCost::zero())
        .with_keyword(Keyword::Rebound)
        .id();
    let p0_spell = add_free_spell(&mut scenario, P0, "Thought Scour", true, THOUGHT_SCOUR);
    let p1_spell = add_free_spell(&mut scenario, P1, "Thought Scour", true, THOUGHT_SCOUR);
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, calm);
    assert_eq!(runner.state().objects[&calm].zone, Zone::Exile);

    let state = runner.state();
    assert!(player_has_hexproof(state, P0));
    assert!(!player_has_hexproof(state, P1));
    assert!(player_cannot_be_targeted_by(state, P0, p1_spell, P1));
    assert!(!player_cannot_be_targeted_by(state, P0, p0_spell, P0));

    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    assert!(player_has_hexproof(runner.state(), P0));

    runner.advance_to_upkeep();
    assert_eq!(runner.state().active_player, P0);
    assert!(!player_has_hexproof(runner.state(), P0));
}

#[test]
fn blossoming_calm_removes_caster_from_opponent_target_player_choices() {
    const P2: PlayerId = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let calm = add_free_spell(&mut scenario, P0, "Blossoming Calm", true, BLOSSOMING_CALM);
    let scour = add_free_spell(&mut scenario, P1, "Thought Scour", true, THOUGHT_SCOUR);
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, calm);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));

    let card_id = runner.state().objects[&scour].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: scour,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Thought Scour");
    let WaitingFor::TargetSelection { target_slots, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a target-player prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    let legal = &target_slots[0].legal_targets;
    assert!(legal.contains(&TargetRef::Player(P1)));
    assert!(legal.contains(&TargetRef::Player(P2)));
    assert!(!legal.contains(&TargetRef::Player(P0)));
}

#[test]
fn gilded_light_grants_caster_shroud_until_end_of_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let light = add_free_spell(&mut scenario, P0, "Gilded Light", true, GILDED_LIGHT);
    let p0_spell = add_free_spell(&mut scenario, P0, "Thought Scour", true, THOUGHT_SCOUR);
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, light);

    let state = runner.state();
    assert!(player_has_shroud(state, P0));
    assert!(!player_has_hexproof(state, P0));
    assert!(!player_has_shroud(state, P1));
    assert!(player_cannot_be_targeted_by(state, P0, p0_spell, P0));

    runner.advance_to_upkeep();
    assert_eq!(runner.state().active_player, P1);
    assert!(!player_has_shroud(runner.state(), P0));
}

#[test]
fn everybody_lives_grants_every_player_hexproof() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_free_spell(&mut scenario, P0, "Everybody Lives!", true, EVERYBODY_LIVES);
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, spell);

    let state = runner.state();
    assert!(player_has_cant_lose_life(state, P1));
    assert!(player_has_hexproof(state, P0));
    assert!(player_has_hexproof(state, P1));
}

#[test]
fn earthshape_grants_caster_hexproof_not_the_earthbent_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let spell = add_free_spell(&mut scenario, P0, "Earthshape", false, EARTHSHAPE);
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, spell);

    assert!(object_has_keyword(&mut runner, land, &Keyword::Hexproof));
    assert!(object_has_keyword(
        &mut runner,
        land,
        &Keyword::Indestructible
    ));
    assert!(player_has_hexproof(runner.state(), P0));
    assert!(!player_has_hexproof(runner.state(), P1));
}

#[test]
fn flare_of_fortitude_locks_life_and_grants_permanents_not_player_hexproof() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_vanilla(P0, 2, 2);
    let theirs = scenario.add_vanilla(P1, 2, 2);
    let spell = add_free_spell(
        &mut scenario,
        P0,
        "Flare of Fortitude",
        true,
        FLARE_OF_FORTITUDE,
    );
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, spell);

    assert!(object_has_keyword(&mut runner, mine, &Keyword::Hexproof));
    assert!(object_has_keyword(
        &mut runner,
        mine,
        &Keyword::Indestructible
    ));
    assert!(!object_has_keyword(&mut runner, theirs, &Keyword::Hexproof));
    let state = runner.state();
    assert!(player_has_cant_gain_life(state, P0));
    assert!(player_has_cant_lose_life(state, P0));
    assert!(!player_has_hexproof(state, P0));
}

#[test]
fn surge_of_salvation_grants_caster_and_their_permanents_hexproof() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_vanilla(P0, 2, 2);
    let theirs = scenario.add_vanilla(P1, 2, 2);
    let spell = add_free_spell(
        &mut scenario,
        P0,
        "Surge of Salvation",
        true,
        SURGE_OF_SALVATION,
    );
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, spell);

    assert!(object_has_keyword(&mut runner, mine, &Keyword::Hexproof));
    assert!(!object_has_keyword(&mut runner, theirs, &Keyword::Hexproof));
    assert!(player_has_hexproof(runner.state(), P0));
    assert!(!player_has_hexproof(runner.state(), P1));
}

#[test]
fn lazotep_plating_grants_caster_and_amassed_army_hexproof() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_free_spell(&mut scenario, P0, "Lazotep Plating", true, LAZOTEP_PLATING);
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, spell);

    let army = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            let obj = &runner.state().objects[id];
            obj.is_token && obj.controller == P0
        })
        .expect("Amass created the Army token");
    assert!(object_has_keyword(&mut runner, army, &Keyword::Hexproof));
    assert!(player_has_hexproof(runner.state(), P0));
    assert!(!player_has_hexproof(runner.state(), P1));
}

#[test]
fn serial_subject_list_with_you_control_stays_one_clause() {
    let def = parse_effect_chain(
        "Auras, Equipment, and modified creatures you control gain hexproof until end of turn.",
        AbilityKind::Spell,
    );
    assert!(def.sub_ability.is_none());
    let Effect::GenericEffect {
        static_abilities, ..
    } = def.effect.as_ref()
    else {
        panic!("expected GenericEffect, got {:?}", def.effect);
    };
    assert!(matches!(
        &static_abilities[0].affected,
        Some(TargetFilter::Or { filters }) if filters.len() == 3
    ));
}

fn chain_nodes(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut nodes = vec![def];
    while let Some(next) = nodes.last().unwrap().sub_ability.as_deref() {
        nodes.push(next);
    }
    nodes
}

#[test]
fn perch_protection_keeps_controller_life_lock_and_protection_grant() {
    let parsed = parse_oracle_text(
        PERCH_PROTECTION,
        "Perch Protection",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let nodes = chain_nodes(&parsed.abilities[0]);
    let you = TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You));
    assert!(nodes.iter().any(|node| matches!(
        node.effect.as_ref(),
        Effect::GenericEffect { static_abilities, .. }
            if static_abilities.iter().map(|s| (&s.mode, s.affected.as_ref())).eq([
                (&StaticMode::CantGainLife, Some(&you)),
                (&StaticMode::CantLoseLife, Some(&you)),
            ])
    )));
    assert!(nodes.iter().any(|node| matches!(
        node.effect.as_ref(),
        Effect::GenericEffect { static_abilities, target: Some(TargetFilter::Controller), .. }
            if static_abilities.iter().any(|s| {
                s.affected == Some(TargetFilter::Controller)
                    && s.modifications.contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Protection(ProtectionTarget::Everything),
                    })
            })
    )));
}
