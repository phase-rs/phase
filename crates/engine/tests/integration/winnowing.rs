//! Winnowing (Lorwyn Eclipsed) — "For each player, you choose a creature that
//! player controls. Then each player sacrifices all other creatures they control
//! that don't share a creature type with the chosen creature they control."
//!
//! CR 608.2d (the spell's controller makes all choices) + CR 701.21a
//! (sacrifice) + CR 101.4 (the unkept creatures are sacrificed simultaneously).
//! Drives the real cast/resolution pipeline:
//!   1. mixed tribes — one creature kept per player; non-type-sharers sacrificed.
//!   2. the shared-type reference is scoped PER PLAYER, not to a single global
//!      keeper (the cross-player bug the recipient binding fixes).
//!   3. a player with no creatures is skipped cleanly.
//!   4. a kept Changeling shares every type — that player sacrifices nothing.
//!   5. a Changeling that is NOT kept still shares with the kept creature and
//!      survives.
//!   6. non-creature permanents are untouched; the sweep is one batch.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::collections::HashMap;

const P2: PlayerId = PlayerId(2);

const WINNOWING: &str = "For each player, you choose a creature that player controls. \
    Then each player sacrifices all other creatures they control that don't share a \
    creature type with the chosen creature they control.";

fn add_winnowing(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Winnowing", false, WINNOWING)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let spell_card = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free Winnowing must succeed");
    runner.resolve_top();
}

/// The caster (ControllerForAll) answers each per-player `CategoryChoice`,
/// keeping `keep[target_player]` when eligible. Players with a single creature
/// auto-resolve (no prompt); players with none are skipped. Every prompting
/// (multi-creature) player MUST have an entry in `keep`, or the choice is
/// rejected — which itself proves the prompt was reached (non-vacuous).
fn answer_keeps(runner: &mut GameRunner, keep: &HashMap<PlayerId, ObjectId>) {
    while let WaitingFor::CategoryChoice {
        target_player,
        eligible_per_category,
        ..
    } = runner.state().waiting_for.clone()
    {
        let chosen = keep
            .get(&target_player)
            .copied()
            .filter(|id| eligible_per_category[0].contains(id));
        assert!(
            chosen.is_some(),
            "no eligible keep supplied for prompting player {target_player:?}"
        );
        runner
            .act(GameAction::SelectCategoryPermanents {
                choices: vec![chosen],
            })
            .expect("the caster's per-player keep choice must be legal");
    }
    runner.advance_until_stack_empty();
}

fn alive(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .is_some_and(|o| o.zone == Zone::Battlefield)
}

fn creature(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    subtypes: &[&str],
) -> ObjectId {
    scenario
        .add_creature(player, name, 2, 2)
        .with_subtypes(subtypes.to_vec())
        .id()
}

/// (1) Mixed tribes across three players: the caster keeps one creature each
/// player controls; every non-type-sharing creature is sacrificed while
/// same-type creatures survive. Reaches the real `CategoryChoice` prompt (the
/// `answer_keeps` assertion) and the per-player sweep.
#[test]
fn winnowing_keeps_one_per_player_and_sacrifices_non_type_sharers() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // P0: keep the Goblin; the Zombie shares no type and is sacrificed.
    let g0 = creature(&mut scenario, P0, "P0 Goblin", &["Goblin"]);
    let z0 = creature(&mut scenario, P0, "P0 Zombie", &["Zombie"]);
    // P1: keep an Elf; a second Elf shares and survives, a Goblin is sacrificed.
    let e1 = creature(&mut scenario, P1, "P1 Elf A", &["Elf"]);
    let e1b = creature(&mut scenario, P1, "P1 Elf B", &["Elf"]);
    let g1 = creature(&mut scenario, P1, "P1 Goblin", &["Goblin"]);
    // P2: a single creature auto-keeps and sacrifices nothing.
    let m2 = creature(&mut scenario, P2, "P2 Merfolk", &["Merfolk"]);

    let spell = add_winnowing(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec![
        "Goblin".into(),
        "Elf".into(),
        "Zombie".into(),
        "Merfolk".into(),
    ];

    cast(&mut runner, spell);
    answer_keeps(&mut runner, &HashMap::from([(P0, g0), (P1, e1)]));

    assert!(alive(&runner, g0), "kept Goblin survives");
    assert!(!alive(&runner, z0), "non-sharing Zombie is sacrificed");
    assert!(alive(&runner, e1), "kept Elf survives");
    assert!(alive(&runner, e1b), "same-type Elf survives");
    assert!(!alive(&runner, g1), "non-sharing Goblin is sacrificed");
    assert!(
        alive(&runner, m2),
        "lone creature is auto-kept and survives"
    );
}

/// (2) Cross-player guard: a creature that shares a type with the CASTER's kept
/// creature but NOT with its own controller's kept creature is still sacrificed.
/// This flips iff the shared-type reference is scoped to each player's kept
/// creature (the recipient binding) rather than a single global keeper.
#[test]
fn winnowing_scopes_shared_type_reference_per_player() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // P0 (caster) keeps a Goblin (needs a second creature to force a prompt).
    let kg = creature(&mut scenario, P0, "Caster Goblin", &["Goblin"]);
    let _extra = creature(&mut scenario, P0, "Caster Elf", &["Elf"]);
    // P1 keeps an Elf; the Goblin shares with the caster's kept Goblin but not
    // with P1's own kept Elf — the buggy global keeper would spare it.
    let ke = creature(&mut scenario, P1, "P1 Elf", &["Elf"]);
    let xg = creature(&mut scenario, P1, "P1 Goblin", &["Goblin"]);

    let spell = add_winnowing(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Elf".into()];

    cast(&mut runner, spell);
    answer_keeps(&mut runner, &HashMap::from([(P0, kg), (P1, ke)]));

    assert!(alive(&runner, kg), "caster's kept Goblin survives");
    assert!(alive(&runner, ke), "P1's kept Elf survives");
    assert!(
        !alive(&runner, xg),
        "P1's Goblin shares only with the CASTER's keeper, so it must still be sacrificed"
    );
}

/// (3) A player controlling no creatures is skipped cleanly — resolution
/// completes and the other players' sweeps still land.
#[test]
fn winnowing_skips_player_with_no_creatures() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let a = creature(&mut scenario, P0, "P0 Goblin", &["Goblin"]);
    let b = creature(&mut scenario, P0, "P0 Elf", &["Elf"]);
    // P1 controls nothing.

    let spell = add_winnowing(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Elf".into()];

    cast(&mut runner, spell);
    answer_keeps(&mut runner, &HashMap::from([(P0, a)]));

    assert!(alive(&runner, a), "kept creature survives");
    assert!(!alive(&runner, b), "non-sharing creature is sacrificed");
}

/// (4) A kept Changeling shares every creature type, so its controller
/// sacrifices nothing.
#[test]
fn winnowing_kept_changeling_sacrifices_nothing() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let ch = scenario
        .add_creature(P0, "Shapeshifter", 2, 2)
        .with_keyword(Keyword::Changeling)
        .id();
    let g = creature(&mut scenario, P0, "P0 Goblin", &["Goblin"]);
    let e = creature(&mut scenario, P0, "P0 Elf", &["Elf"]);

    let spell = add_winnowing(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Elf".into(), "Merfolk".into()];

    cast(&mut runner, spell);
    answer_keeps(&mut runner, &HashMap::from([(P0, ch)]));

    assert!(alive(&runner, ch), "kept Changeling survives");
    assert!(
        alive(&runner, g),
        "Goblin shares a type with the Changeling and survives"
    );
    assert!(
        alive(&runner, e),
        "Elf shares a type with the Changeling and survives"
    );
}

/// (5) A Changeling that is NOT kept still shares a type with the kept creature
/// (it shares every type) and survives, while a genuinely non-sharing creature
/// is sacrificed.
#[test]
fn winnowing_unkept_changeling_survives() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let g = creature(&mut scenario, P0, "P0 Goblin", &["Goblin"]);
    let ch = scenario
        .add_creature(P0, "Shapeshifter", 2, 2)
        .with_keyword(Keyword::Changeling)
        .id();
    let e = creature(&mut scenario, P0, "P0 Elf", &["Elf"]);

    let spell = add_winnowing(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Elf".into()];

    cast(&mut runner, spell);
    answer_keeps(&mut runner, &HashMap::from([(P0, g)]));

    assert!(alive(&runner, g), "kept Goblin survives");
    assert!(
        alive(&runner, ch),
        "unkept Changeling shares a type with the kept Goblin and survives"
    );
    assert!(!alive(&runner, e), "non-sharing Elf is sacrificed");
}

/// (6) Non-creature permanents are never swept (the sacrifice domain is
/// creatures only), and every unkept non-sharer across all players is
/// sacrificed in the single resolution step.
#[test]
fn winnowing_leaves_noncreatures_and_sweeps_in_one_step() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let g0 = creature(&mut scenario, P0, "P0 Goblin", &["Goblin"]);
    let e0 = creature(&mut scenario, P0, "P0 Elf", &["Elf"]);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let g1 = creature(&mut scenario, P1, "P1 Goblin", &["Goblin"]);
    let e1 = creature(&mut scenario, P1, "P1 Elf", &["Elf"]);

    let spell = add_winnowing(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".into(), "Elf".into()];

    cast(&mut runner, spell);
    answer_keeps(&mut runner, &HashMap::from([(P0, g0), (P1, g1)]));

    // The unkept non-sharers across BOTH players are removed in the single
    // post-choice sweep (`sacrifice_unchosen_from_handler` runs once, after all
    // category choices), so both Elves are gone in the same resolution step.
    assert!(alive(&runner, land), "non-creature land is never swept");
    assert!(alive(&runner, g0), "P0's kept Goblin survives");
    assert!(alive(&runner, g1), "P1's kept Goblin survives");
    assert!(!alive(&runner, e0), "P0's non-sharing Elf is sacrificed");
    assert!(!alive(&runner, e1), "P1's non-sharing Elf is sacrificed");
}
