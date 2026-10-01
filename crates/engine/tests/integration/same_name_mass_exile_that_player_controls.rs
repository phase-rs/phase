//! "<verb> target <object> an opponent controls and all <X> that player
//! controls with the same name as that <object>" — the mass conjunct's "that
//! player" is the targeted object's controller (CR 608.2c), read through
//! last-known information once the target has already been exiled (CR 608.2h).
//!
//! Before the fix the conjunct bound `ControllerRef::You`, so Legion's End and
//! Deputy of Detention exiled the CASTER's same-named permanents, and Legions
//! to Ashes ("all tokens …") did not parse at all. Deputy's "until this
//! creature leaves the battlefield" must also cover the mass conjunct (CR 610.3).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const LEGIONS_TO_ASHES: &str = "Exile target nonland permanent an opponent controls and all \
tokens that player controls with the same name as that permanent.";

const LEGIONS_END: &str = "Exile target creature an opponent controls with mana value 2 or less \
and all other creatures that player controls with the same name as that creature. Then that \
player reveals their hand and exiles all cards with that name from their hand and graveyard.";

const DEPUTY_OF_DETENTION: &str = "When this creature enters, exile target nonland permanent an \
opponent controls and all other nonland permanents that player controls with the same name as \
that permanent until this creature leaves the battlefield.";

fn free() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![],
    }
}

fn make_token(runner: &mut GameRunner, id: ObjectId) {
    runner.state_mut().objects.get_mut(&id).unwrap().is_token = true;
}

fn on_battlefield(state: &GameState, id: ObjectId) -> bool {
    state.battlefield.contains(&id)
}

fn named_on_battlefield(state: &GameState, player: PlayerId, name: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.controller == player && o.name == name)
        .count()
}

/// CR 111.1 + CR 201.2a + CR 608.2c: Legions to Ashes exiles the target and
/// every TOKEN its controller controls with that name — not that player's
/// same-named nontoken permanents, not other-named tokens, and never the
/// caster's tokens.
#[test]
fn legions_to_ashes_exiles_only_that_players_same_name_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let twin_token = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let other_name_token = scenario.add_creature(P1, "Spirit", 1, 1).id();
    let nontoken_twin = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let my_token = scenario.add_creature(P0, "Soldier", 1, 1).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Legions to Ashes", false, LEGIONS_TO_ASHES)
        .with_mana_cost(free())
        .id();

    let mut runner = scenario.build();
    for token in [victim, twin_token, other_name_token, my_token] {
        make_token(&mut runner, token);
    }

    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Legions to Ashes must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );

    let state = runner.state();
    assert!(!on_battlefield(state, victim), "the target must be exiled");
    // Reach guard: the mass conjunct ran and exiled the opponent's same-name token.
    assert!(
        !on_battlefield(state, twin_token),
        "that player's same-name token must be exiled"
    );
    assert!(
        on_battlefield(state, my_token),
        "the caster's same-name token must stay: \"that player\" is the target's controller"
    );
    assert!(
        on_battlefield(state, nontoken_twin),
        "a same-name NONTOKEN permanent must stay (\"all tokens\")"
    );
    assert!(
        on_battlefield(state, other_name_token),
        "a token with a different name must stay"
    );
}

/// CR 608.2c: Legion's End exiles the target and that player's same-name
/// creatures; the caster's same-name creature is untouched.
#[test]
fn legions_end_spares_the_casters_same_name_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let twin = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mine = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Legion's End", false, LEGIONS_END)
        .with_mana_cost(free())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Legion's End must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );

    outcome.assert_zone(&[victim], Zone::Exile);
    // Reach guard: the mass conjunct exiled the opponent's other copy.
    outcome.assert_zone(&[twin], Zone::Exile);
    assert!(
        on_battlefield(runner.state(), mine),
        "the caster's same-name creature must stay on the battlefield"
    );
}

/// CR 610.3: Deputy of Detention's "until this creature leaves the
/// battlefield" covers the whole exile instruction, so the same-name mass
/// exile returns with the target when Deputy leaves.
#[test]
fn deputy_of_detention_returns_mass_exiled_permanents_when_it_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let twin = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let mine = scenario.add_creature(P0, "Soldier", 1, 1).id();

    let deputy = scenario
        .add_creature_to_hand_from_oracle(P0, "Deputy of Detention", 1, 3, DEPUTY_OF_DETENTION)
        .with_mana_cost(free())
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, "Destroy target creature.")
        .with_mana_cost(free())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(deputy).target_objects(&[victim]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Deputy's ETB must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[victim, twin], Zone::Exile);
    assert!(
        on_battlefield(runner.state(), mine),
        "the caster's same-name permanent must stay on the battlefield"
    );

    runner.cast(removal).target_objects(&[deputy]).resolve();
    assert!(
        !on_battlefield(runner.state(), deputy),
        "Deputy must have left the battlefield"
    );
    // CR 610.3: both exiled permanents return (as new objects, CR 400.7).
    assert_eq!(
        named_on_battlefield(runner.state(), P1, "Soldier"),
        2,
        "the target AND the mass-exiled same-name permanent must return"
    );
}
