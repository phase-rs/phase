//! Regression for GitHub issue #8077 — Heart-Shaped Herb returned itself
//! instead of the creature sacrificed by its own optional effect.
//!
//! Oracle text (verified against the live Scryfall API):
//!   "If a source an opponent controls would deal damage to you, prevent 1 of
//!    that damage.
//!    {2}, {T}, Sacrifice this artifact: You may sacrifice a creature. If you
//!    do, return that card to the battlefield under its owner's control with
//!    three +1/+1 counters on it and you become the monarch."
//!
//! The bug: "that card" in the gated return clause is a back-reference to the
//! creature the OPTIONAL SACRIFICE EFFECT ("You may sacrifice a creature")
//! just chose — a resolution-time choice (CR 701.21a), not a target
//! selection. With no chosen target for `ParentTarget` to inherit, the parser
//! defaulted the anaphor to `ParentTarget`, which resolves at runtime to the
//! ability's own SOURCE — Heart-Shaped Herb itself, already sacrificed as
//! part of the activation cost — instead of the creature sacrificed by the
//! effect.
//!
//! This test drives the real activation pipeline end to end: activate Heart-
//! Shaped Herb (paying its own sacrifice-this-artifact cost), accept the
//! optional sacrifice, choose a DIFFERENT creature (not Heart-Shaped Herb)
//! from a two-creature pool, and assert that CHOSEN creature — never
//! Heart-Shaped Herb — returns to the battlefield with three +1/+1 counters,
//! plus that the monarch changes hands. Fails on pre-fix HEAD (Heart-Shaped
//! Herb itself would come back with the counters instead).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// CR 613: power/toughness after the layer pipeline has been applied.
fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = runner.state().objects.get(&id).expect("object present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); n]
}

/// CR 122.1: `+1/+1` counters on `obj`, `0` if the object no longer exists or
/// carries none.
fn plus1plus1_counters(state: &GameState, obj: ObjectId) -> u32 {
    state
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

/// Board: Heart-Shaped Herb plus TWO creatures (Solemn Simulacrum and a
/// P1-owned creature controlled by P0) so the optional sacrifice's "which
/// creature" choice is genuinely interactive — the returned object must be
/// the one the player actually picked, not merely "the only eligible creature".
fn board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let db = load_db().expect("integration card fixture must load");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let herb = scenario.add_real_card(P0, "Heart-Shaped Herb", Zone::Battlefield, db);
    let solemn = scenario.add_creature(P0, "Solemn Simulacrum", 2, 2).id();
    let other = scenario
        .add_creature(P1, "Bystander Bear", 2, 2)
        .controlled_by(P0)
        .id();

    // {2} generic for the activation cost — pre-funded so mana payment never
    // surfaces a prompt and the test stays focused on the sacrifice/return
    // anaphor binding.
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 2));

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    (runner, herb, solemn, other)
}

/// Drive Heart-Shaped Herb's activated ability to completion, accepting the
/// optional sacrifice and choosing `chosen` as the creature to sacrifice.
fn activate_and_choose(runner: &mut GameRunner, herb: ObjectId, chosen: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: herb,
            ability_index: 0,
        })
        .expect("activating Heart-Shaped Herb must succeed with cost paid in full");

    let mut accepted = false;
    let mut chose = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional 'you may sacrifice a creature'");
                accepted = true;
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert!(
                    cards.contains(&chosen),
                    "the chosen creature must be a legal sacrifice option, got {cards:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![chosen],
                    })
                    .expect("sacrificing the chosen creature must succeed");
                chose = true;
            }
            _ => {
                runner.advance_until_stack_empty();
                if !runner.state().stack.is_empty() {
                    continue;
                }
                if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    break;
                }
            }
        }
    }

    assert!(accepted, "must have accepted the optional sacrifice");
    assert!(chose, "must have chosen a creature to sacrifice");
}

/// Positive control: accepting the optional sacrifice and choosing Solemn
/// Simulacrum must return SOLEMN SIMULACRUM (with three +1/+1 counters) —
/// never Heart-Shaped Herb, which is already in the graveyard as the
/// activation's own cost payment. Also asserts the monarch changes hands.
#[test]
fn heart_shaped_herb_returns_the_sacrificed_creature_not_itself() {
    let (mut runner, herb, solemn, other) = board();

    assert_eq!(runner.state().monarch, None, "precondition: no monarch yet");

    activate_and_choose(&mut runner, herb, solemn);

    // Heart-Shaped Herb itself is gone (sacrificed to pay its own cost) and
    // must NOT be the object that came back.
    assert!(
        runner
            .state()
            .objects
            .get(&herb)
            .is_none_or(|o| o.zone != Zone::Battlefield),
        "Heart-Shaped Herb must remain off the battlefield — it was sacrificed as a cost"
    );

    // The chosen creature (Solemn Simulacrum) must be the one that returned.
    let solemn_obj = runner
        .state()
        .objects
        .get(&solemn)
        .expect("Solemn Simulacrum object must still exist");
    assert_eq!(
        solemn_obj.zone,
        Zone::Battlefield,
        "the sacrificed creature (Solemn Simulacrum) must return to the battlefield, not \
         Heart-Shaped Herb — got zone {:?}",
        solemn_obj.zone
    );
    assert_eq!(
        plus1plus1_counters(runner.state(), solemn),
        3,
        "the returned creature must carry three +1/+1 counters"
    );

    // The other creature was never touched by this activation.
    assert!(
        runner.state().battlefield.contains(&other),
        "the non-chosen creature must remain untouched on the battlefield"
    );
    assert_eq!(
        plus1plus1_counters(runner.state(), other),
        0,
        "the non-chosen creature must not receive counters meant for the sacrificed one"
    );

    assert_eq!(
        runner.state().monarch,
        Some(P0),
        "the activating player must become the monarch (CR 725.1)"
    );
}

/// Positive control on the SECOND creature: choosing the OTHER creature
/// (not Solemn Simulacrum) must return THAT creature instead — proving the
/// binding tracks the player's actual choice rather than defaulting to a
/// single hardcoded object.
#[test]
fn heart_shaped_herb_returns_whichever_creature_was_chosen() {
    let (mut runner, herb, solemn, other) = board();

    let other_before = runner
        .state()
        .objects
        .get(&other)
        .expect("the P1-owned creature must exist before activation");
    assert_eq!(other_before.owner, P1, "fixture must preserve P1 ownership");
    assert_eq!(
        other_before.controller, P0,
        "fixture must make P0 the creature's controller and therefore able to sacrifice it"
    );

    activate_and_choose(&mut runner, herb, other);

    assert!(
        runner
            .state()
            .objects
            .get(&herb)
            .is_none_or(|o| o.zone != Zone::Battlefield),
        "Heart-Shaped Herb must remain off the battlefield — it was sacrificed as a cost"
    );

    let other_obj = runner
        .state()
        .objects
        .get(&other)
        .expect("the other creature object must still exist");
    assert_eq!(
        other_obj.zone,
        Zone::Battlefield,
        "the sacrificed creature must return, got zone {:?}",
        other_obj.zone
    );
    assert_eq!(plus1plus1_counters(runner.state(), other), 3);
    assert_eq!(
        other_obj.owner, P1,
        "the returned card must retain its original owner"
    );
    assert_eq!(
        other_obj.controller, P1,
        "the return effect must place the sacrificed creature under its owner's control"
    );

    // Solemn Simulacrum, not chosen this time, must be untouched.
    assert!(runner.state().battlefield.contains(&solemn));
    assert_eq!(plus1plus1_counters(runner.state(), solemn), 0);

    assert_eq!(runner.state().monarch, Some(P0));
}

/// The Equipment case from the Discord report: a creature wearing an Equipment
/// is sacrificed and returned by Heart-Shaped Herb, both inside ONE resolution.
///
/// CR 704.3 + CR 704.4: state-based actions are checked only when a player
/// would receive priority and "pay no attention to what happens during the
/// resolution of a spell or ability", so the CR 704.5n unattach SBA never
/// observes the host as absent. `ObjectId` is storage identity in this engine,
/// so a back-pointer left dangling by the host's exit re-validates against the
/// RETURNED permanent — which CR 400.7 makes a new object with no relation to
/// the one that left. Observed symptoms, both asserted below: the Equipment
/// rendered nowhere (its `attached_to` was set but no host listed it), and the
/// returned creature kept the Equipment's continuous bonus.
///
/// CR 301.5c: the Equipment itself remains on the battlefield, unattached.
#[test]
fn equipment_unattaches_when_heart_shaped_herb_returns_its_host() {
    let db = load_db().expect("integration card fixture must load");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let herb = scenario.add_real_card(P0, "Heart-Shaped Herb", Zone::Battlefield, db);
    let creature = scenario.add_creature(P0, "Solemn Simulacrum", 2, 2).id();
    // A second creature keeps the sacrifice genuinely interactive — with a sole
    // eligible creature the engine auto-selects and never surfaces
    // `EffectZoneChoice`, so the equipped creature would be sacrificed by
    // default rather than by the player's choice.
    let bystander = scenario
        .add_creature(P1, "Bystander Bear", 2, 2)
        .controlled_by(P0)
        .id();
    // Bonesplitter: "Equipped creature gets +2/+0." A real card, so the bonus
    // travels through the production parser and layer pipeline rather than a
    // hand-built modification.
    let bonesplitter = scenario.add_real_card(P0, "Bonesplitter", Zone::Battlefield, db);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 2));

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    engine::game::effects::attach::attach_to(runner.state_mut(), bonesplitter, creature);
    engine::game::layers::evaluate_layers(runner.state_mut());

    // REACH GUARD: prove the fixture actually built the equipped state and that
    // the bonus is live. Without this, every post-resolution assertion below
    // could pass for the trivial reason that the Equipment never applied.
    assert_eq!(
        runner.state().objects[&bonesplitter].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(creature)),
        "precondition: Bonesplitter must be attached to the creature"
    );
    assert!(
        runner.state().objects[&creature]
            .attachments
            .contains(&bonesplitter),
        "precondition: the host must list the Equipment (symmetric edge)"
    );
    assert_eq!(
        power_toughness(&runner, creature),
        (4, 2),
        "precondition: Bonesplitter's +2/+0 must be live on the 2/2 host"
    );

    activate_and_choose(&mut runner, herb, creature);

    // The unchosen creature is untouched — it keeps no Equipment it never had.
    assert!(
        runner.state().battlefield.contains(&bystander),
        "the non-chosen creature must remain on the battlefield"
    );
    assert!(
        runner.state().objects[&bystander].attachments.is_empty(),
        "the Equipment must not migrate to the untouched creature"
    );

    // The creature came back (that is issue #8077's behavior, re-asserted here
    // as a reach guard so the assertions below are about the RETURNED object).
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "the sacrificed creature must return to the battlefield"
    );
    assert_eq!(
        plus1plus1_counters(runner.state(), creature),
        3,
        "the returned creature must carry three +1/+1 counters"
    );

    // CR 704.5n / CR 301.5c: the Equipment stays on the battlefield...
    assert_eq!(
        runner.state().objects[&bonesplitter].zone,
        Zone::Battlefield,
        "the Equipment must remain on the battlefield, not vanish"
    );
    // ...unattached, in BOTH directions. A one-sided sever is exactly what made
    // it invisible: the client drops an attachment whose `attached_to` is set
    // from the battlefield rows, expecting the host surface to render it.
    assert_eq!(
        runner.state().objects[&bonesplitter].attached_to,
        None,
        "CR 704.5n: the Equipment must be unattached once its host left the battlefield"
    );
    assert!(
        !runner.state().objects[&creature]
            .attachments
            .contains(&bonesplitter),
        "CR 400.7: the returned permanent is a new object and must not inherit the attachment"
    );

    // CR 400.7: no memory of, or relation to, its previous existence — so the
    // Equipment's +2/+0 must be gone. 2/2 base + three +1/+1 counters = 5/5;
    // with the stale edge re-validating it was 7/5.
    assert_eq!(
        power_toughness(&runner, creature),
        (5, 5),
        "the returned creature must be base 2/2 plus three +1/+1 counters, with NO \
         Bonesplitter bonus — a 7/5 here means the stale attachment re-applied"
    );
}
