//! Digital-only Alchemy (no CR entry for intensity itself): a triggered or
//! activated ability reads its source's intensity for the source incarnation
//! it belongs to (CR 608.2h) — live while that incarnation is in its public
//! zone, its last known information once it has left. Before, a triggered read
//! went through last known information that carried no intensity, so "damage
//! equal to its intensity" dealt 0.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// Verbatim Oracle text (MTGJSON `text`).
const BELLOWSBREATH_OGRE: &str = "Starting intensity 1\nWhenever this creature attacks, it \
deals damage equal to its intensity to any target. Then this creature intensifies by 1.";

/// The attack trigger deals the intensity it reads (1), then intensifies.
#[test]
fn bellowsbreath_ogre_deals_its_intensity_then_intensifies() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let ogre = scenario
        .add_creature_from_oracle(P0, "Bellowsbreath Ogre", 3, 3, BELLOWSBREATH_OGRE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&ogre).unwrap().intensity = 1;
    let life_before = runner.state().players[1].life;

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(ogre, AttackTarget::Player(P1))])
        .expect("DeclareAttackers accepted");
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("ChooseTarget accepted");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            _ => break,
        }
    }

    assert!(runner.state().stack.is_empty(), "the trigger resolved");
    assert_eq!(
        life_before - runner.state().players[1].life,
        1,
        "damage equal to its intensity"
    );
    assert_eq!(runner.state().objects[&ogre].intensity, 2);
}

/// Verbatim Oracle text (MTGJSON `text`).
const GREAT_DESERT_HELLION: &str = "Starting intensity 1\nMenace\nAt the beginning of your upkeep, \
sacrifice Great Desert Hellion unless you discard a card. If you discard a card this way, Great \
Desert Hellion intensifies by 1.\nWhen Great Desert Hellion leaves the battlefield, you may discard \
your hand. If you do, draw cards equal to Great Desert Hellion's intensity.";

/// CR 608.2h: the leaves-the-battlefield trigger reads the departed source's
/// last known intensity (2): discard the hand, draw two.
#[test]
fn great_desert_hellion_leaving_draws_its_last_known_intensity() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }
    scenario.add_card_to_hand(P0, "Spare Card");
    let hellion = scenario
        .add_creature_from_oracle(P0, "Great Desert Hellion", 5, 5, GREAT_DESERT_HELLION)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .with_mana_cost(engine::types::mana::ManaCost::default())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&hellion)
        .unwrap()
        .intensity = 2;
    let card_id = runner.state().objects[&murder].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: murder,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("CastSpell accepted");
    let mut asked = false;
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(hellion)),
                    })
                    .expect("ChooseTarget accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                asked = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("discard the hand");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(
        asked,
        "reach guard: the leaves-the-battlefield trigger asked"
    );
    assert_eq!(
        runner.state().objects[&hellion].zone,
        engine::types::zones::Zone::Graveyard
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        2,
        "the hand was discarded, then two cards drawn"
    );
}

/// Verbatim Oracle text (MTGJSON `text`).
const DRIX_INTERLACER: &str = "Starting intensity 0\nWhenever another artifact you control \
enters, this artifact intensifies by 1.\n{T}, Sacrifice this artifact: Draw X cards, where X is \
half this artifact's intensity, rounded down. Activate only as a sorcery.";

/// CR 608.2h + CR 704.5d: a token Drix Interlacer sacrificed as the cost
/// ceases to exist before its ability resolves; the ability reads the
/// intensity the token left with (4 → draw two).
#[test]
fn drix_interlacer_token_sacrificed_draws_half_its_last_known_intensity() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }
    let drix = scenario
        .add_artifact_from_oracle(P0, "Drix Interlacer", DRIX_INTERLACER)
        .id();
    let mut runner = scenario.build();
    {
        let object = runner.state_mut().objects.get_mut(&drix).unwrap();
        object.intensity = 4;
        object.is_token = true;
    }
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: drix,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("ActivateAbility accepted");
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(
        !runner.state().objects.contains_key(&drix),
        "reach guard: the token ceased to exist"
    );
    assert_eq!(runner.state().players[0].hand.len() - hand_before, 2);
}

/// CR 608.2h + CR 400.7: a card Drix Interlacer sacrificed for its ability is a
/// new object in the graveyard; intensifying that card in response does not
/// change the intensity the ability reads (4 → draw two, not five).
#[test]
fn drix_interlacer_reads_the_intensity_it_was_sacrificed_with() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..8 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }
    let drix = scenario
        .add_artifact_from_oracle(P0, "Drix Interlacer", DRIX_INTERLACER)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&drix).unwrap().intensity = 4;
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: drix,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("ActivateAbility accepted");
    assert_eq!(
        runner.state().objects[&drix].zone,
        engine::types::zones::Zone::Graveyard,
        "reach guard: sacrificed as the cost"
    );
    runner.state_mut().objects.get_mut(&drix).unwrap().intensity = 10;
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_eq!(runner.state().players[0].hand.len() - hand_before, 2);
}
