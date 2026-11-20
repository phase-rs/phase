//! CR 111.4 + CR 105.2 + CR 602.2b + CR 603.3d: Dragonbroods' Relic.
//!
//! Oracle: "{T}, Tap an untapped creature you control: Add one mana of any color.
//! {3}{W}{U}{B}{R}{G}, Sacrifice this artifact: Create a 4/4 Dragon creature token named
//! Reliquary Dragon that's all colors. It has flying, lifelink, and "When this token enters, it
//! deals 3 damage to any target." Activate only as a sorcery."
//!
//! The creating ability names the token "Reliquary Dragon" and makes it all five colors. The
//! quoted "When this token enters, ..." is a triggered ability the token has; its target is
//! chosen when that ability is put on the stack (CR 603.3d), so the activation itself chooses
//! no target (CR 602.2b).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DRAGONBROODS_RELIC: &str = "{T}, Tap an untapped creature you control: Add one mana of \
     any color.\n{3}{W}{U}{B}{R}{G}, Sacrifice this artifact: Create a 4/4 Dragon creature token \
     named Reliquary Dragon that's all colors. It has flying, lifelink, and \"When this token \
     enters, it deals 3 damage to any target.\" Activate only as a sorcery.";

fn relic_board() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let relic = scenario
        .add_artifact_from_oracle(P0, "Dragonbroods' Relic", DRAGONBROODS_RELIC)
        .id();
    let mut pool: Vec<ManaUnit> = [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ]
    .into_iter()
    .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
    .collect();
    pool.extend(vec![
        ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![]
        );
        3
    ]);
    scenario.with_mana_pool(P0, pool);
    (scenario.build(), relic)
}

fn life(runner: &GameRunner, player: engine::types::player::PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .life
}

/// Activate the Relic's token ability through the real action. Returns the prompt the
/// activation stopped at (if it asked for a target) and drives the token's enter trigger,
/// targeting P1 when the trigger is put on the stack.
fn activate_and_resolve(runner: &mut GameRunner, relic: ObjectId) -> Vec<String> {
    let mut prompts = Vec::new();
    runner
        .act(GameAction::ActivateAbility {
            source_id: relic,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation accepted");
    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return prompts,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::TargetSelection { .. } => {
                prompts.push("activation target".to_string());
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                }
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                prompts.push("trigger target".to_string());
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("legal answer");
    }
    panic!("did not resolve: {:?}", runner.state().waiting_for);
}

fn reliquary_dragon(runner: &GameRunner) -> ObjectId {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].is_token)
        .expect("the token")
}

/// CR 602.2b + CR 603.3d: the activation chooses no target; the token's enter trigger asks for
/// its target when it is put on the stack, and deals 3 damage to it. A guard (it passes at the base): the
/// "activation target" the task reported was the scenario harness's `.resolve()` asking for the
/// token trigger's target, not a slot of the activation itself.
#[test]
fn relic_activation_chooses_no_target_and_the_token_trigger_does() {
    let (mut runner, relic) = relic_board();
    let p1_before = life(&runner, P1);

    let prompts = activate_and_resolve(&mut runner, relic);

    assert_eq!(
        runner.state().objects[&relic].zone,
        Zone::Graveyard,
        "reach: activated"
    );
    assert_eq!(prompts, vec!["trigger target".to_string()]);
    assert_eq!(
        life(&runner, P1),
        p1_before - 3,
        "the enter trigger dealt 3"
    );
}

/// CR 111.4 + CR 105.2: the token is named Reliquary Dragon, is all five colors, and has
/// flying and lifelink. Revert-to-red: the name swallows "that's all colors" and the token is
/// colorless.
#[test]
fn relic_token_is_named_reliquary_dragon_and_all_colors() {
    let (mut runner, relic) = relic_board();

    activate_and_resolve(&mut runner, relic);

    let dragon = &runner.state().objects[&reliquary_dragon(&runner)];
    assert_eq!(dragon.name, "Reliquary Dragon");
    let mut colors = dragon.color.clone();
    colors.sort();
    let mut all = ManaColor::ALL.to_vec();
    all.sort();
    assert_eq!(colors, all);
    assert_eq!((dragon.power, dragon.toughness), (Some(4), Some(4)));
    assert!(dragon.has_keyword(&Keyword::Flying));
    assert!(dragon.has_keyword(&Keyword::Lifelink));
}
