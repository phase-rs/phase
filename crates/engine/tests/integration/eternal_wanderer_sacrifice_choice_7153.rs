//! Implements the previously-unparsed −4 clause on The Eternal Wanderer:
//! "For each player, choose a creature that player controls. Each player
//! sacrifices all creatures they control not chosen this way."
//!
//! Oracle text verified against the live Scryfall API (2026-08-28). CR 608.2c:
//! a bare imperative "choose" with no explicit chooser still addresses the
//! ability's controller by default — confirmed by Breach the Multiverse /
//! Ghouls' Night Out ("For each player, choose a creature card in that
//! player's graveyard. Put those cards onto the battlefield under YOUR
//! control"), which end up under the caster's control despite lacking "you".
//! This card is the same idiom applied to a battlefield-control choose
//! instead of a zone choose, so it lowers to the SAME `ChooseAndSacrificeRest`
//! building block Tragic Arrogance/Winnowing use, with
//! `CategoryChooserScope::ControllerForAll` — the caster chooses each
//! player's keeper, and "not chosen this way" is `ChooseAndSacrificeRest`'s
//! own guaranteed semantics rather than a separately-parsed filter.
//!
//! Mirrors `winnowing.rs`'s driving pattern (mod `crate::support` unused here
//! — this card needs no oracle-line building blocks beyond the shared
//! `GameScenario`/`GameRunner` surface). Mutation-tested: reverting the
//! bare-"choose" dispatch arm collapses the whole ability to
//! `Effect::Unimplemented`, so `cast` below would leave the stack entry
//! resolving to nothing and `WaitingFor::CategoryChoice` would never appear —
//! the `answer_keeps` loop's `while let` would simply not execute, and every
//! creature (including the ones that should be sacrificed) would still be
//! alive, failing the negative assertions.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::collections::HashMap;

const P2: PlayerId = PlayerId(2);

const ETERNAL_WANDERER_MINUS_FOUR: &str = "For each player, choose a creature that player \
    controls. Each player sacrifices all creatures they control not chosen this way.";

fn add_spell(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Test Sweep", false, ETERNAL_WANDERER_MINUS_FOUR)
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
        .expect("casting the free test sweep spell must succeed");
    runner.resolve_top();
}

/// The caster (`ControllerForAll`) answers each per-player `CategoryChoice`,
/// keeping `keep[target_player]` when eligible. A player with a single
/// creature auto-resolves (no prompt); a player with none is skipped. Every
/// prompting (multi-creature) player MUST have an entry in `keep`, or the
/// choice is rejected — proving the prompt was actually reached.
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

/// Three players, each with multiple creatures: every player keeps EXACTLY
/// their chosen creature and loses every other creature they control. This is
/// the exact per-player "pick one to keep, sacrifice the rest" shape the
/// bare "for each player, choose" dispatch must reach.
#[test]
fn each_player_keeps_exactly_their_chosen_creature_and_loses_the_rest() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let p0_keep = scenario.add_creature(P0, "P0 Kept", 2, 2).id();
    let p0_lose_a = scenario.add_creature(P0, "P0 Lost A", 2, 2).id();
    let p0_lose_b = scenario.add_creature(P0, "P0 Lost B", 2, 2).id();

    let p1_keep = scenario.add_creature(P1, "P1 Kept", 3, 3).id();
    let p1_lose = scenario.add_creature(P1, "P1 Lost", 3, 3).id();

    // P2 controls only one creature — an auto-resolved (no-prompt) keep,
    // proving the sweep still reaches a player who never surfaces a choice.
    let p2_keep = scenario.add_creature(P2, "P2 Kept", 1, 1).id();

    let spell = add_spell(&mut scenario);
    let mut runner = scenario.build();

    cast(&mut runner, spell);
    answer_keeps(&mut runner, &HashMap::from([(P0, p0_keep), (P1, p1_keep)]));

    // P0: keeps exactly the chosen creature, loses both others.
    assert!(alive(&runner, p0_keep), "P0's chosen creature survives");
    assert!(
        !alive(&runner, p0_lose_a),
        "P0's unchosen creature A is sacrificed"
    );
    assert!(
        !alive(&runner, p0_lose_b),
        "P0's unchosen creature B is sacrificed"
    );

    // P1: keeps exactly the chosen creature, loses the other.
    assert!(alive(&runner, p1_keep), "P1's chosen creature survives");
    assert!(
        !alive(&runner, p1_lose),
        "P1's unchosen creature is sacrificed"
    );

    // P2: the lone creature is auto-kept — the sweep reaches every player,
    // not just the ones who prompted.
    assert!(
        alive(&runner, p2_keep),
        "P2's sole creature is auto-kept and survives"
    );
}

/// A player controlling no creatures is skipped cleanly — resolution
/// completes and the other players' sweeps still land. Reach-guard for the
/// negative assertion above: if the parser dropped the whole ability to
/// `Effect::Unimplemented`, this test's `while let CategoryChoice` loop would
/// never execute and BOTH creatures below would incorrectly survive.
#[test]
fn player_with_no_creatures_is_skipped() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let a = scenario.add_creature(P0, "P0 Kept", 2, 2).id();
    let b = scenario.add_creature(P0, "P0 Lost", 2, 2).id();
    // P1 controls nothing.

    let spell = add_spell(&mut scenario);
    let mut runner = scenario.build();

    cast(&mut runner, spell);
    answer_keeps(&mut runner, &HashMap::from([(P0, a)]));

    assert!(alive(&runner, a), "P0's kept creature survives");
    assert!(!alive(&runner, b), "P0's unchosen creature is sacrificed");
}
