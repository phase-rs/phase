//! A gift token is a token (#10900).
//!
//! CR 702.174f: "Gift a tapped Fish" means "The chosen player creates a tapped
//! 1/1 blue Fish creature token." CR 111.2: the player who creates a token owns
//! and controls it. So the gift Fish is the recipient's token for every token
//! rule: their "creature tokens you control" statics pump it, their token
//! doublers apply to its creation (CR 614.1a), and once it leaves the
//! battlefield it ceases to exist (CR 704.5d).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const INTO_THE_FLOOD_MAW: &str =
    "Gift a tapped Fish (You may promise an opponent a gift as you cast this spell. \
If you do, they create a tapped 1/1 blue Fish creature token before its other effects.)\n\
Return target creature an opponent controls to its owner's hand. If the gift was promised, \
instead return target nonland permanent an opponent controls to its owner's hand.";

const POLLEN_SHIELD_HARE: &str = "Creature tokens you control get +1/+1.";

const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, \
it creates twice that many of those tokens instead.";

const DESTROY_CREATURE: &str = "Destroy target creature.";

fn mana_pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|&mana_type| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect()
}

/// Casts `spell` with any optional cost paid and the gift promised to P1,
/// targets `target`, and resolves the stack.
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the spell is castable");
    loop {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalCostChoice { .. } => GameAction::DecideOptionalCost { pay: true },
            WaitingFor::ChooseGiftRecipient { .. } => {
                GameAction::ChooseGiftRecipient { opponent: P1 }
            }
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while casting: {other:?}"),
        };
        runner.act(action).expect("the cast advances");
    }
    runner.advance_until_stack_empty();
}

/// P0 holds Into the Flood Maw and a "Destroy target creature." spell; P1
/// controls Pollen-Shield Hare and a Grizzly Bears (the bounce target).
/// `doubler` puts Parallel Lives under that player's control.
fn setup(doubler: Option<PlayerId>) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flood_maw = scenario
        .add_spell_to_hand(P0, "Into the Flood Maw", true)
        .from_oracle_text_with_keywords(&["Gift"], INTO_THE_FLOOD_MAW)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Removal", true, DESTROY_CREATURE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.add_creature_from_oracle(P1, "Pollen-Shield Hare", 2, 2, POLLEN_SHIELD_HARE);
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    if let Some(player) = doubler {
        scenario.add_enchantment_from_oracle(player, "Parallel Lives", PARALLEL_LIVES);
    }
    scenario.with_mana_pool(P0, mana_pool(&[ManaType::Blue, ManaType::Black]));
    (scenario.build(), flood_maw, destroy, bears)
}

/// The Fish objects P1 controls on the battlefield.
fn p1_fish(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = &runner.state().objects[id];
            obj.controller == P1 && obj.card_types.subtypes.iter().any(|s| s == "Fish")
        })
        .collect()
}

#[test]
fn gift_fish_is_the_recipients_token_and_ceases_to_exist_when_it_dies() {
    let (mut runner, flood_maw, destroy, bears) = setup(None);

    cast_and_resolve(&mut runner, flood_maw, bears);

    let fish = p1_fish(&runner);
    let [fish] = fish.as_slice() else {
        panic!("one gift Fish for P1, got {fish:?}");
    };
    let fish = *fish;
    let obj = &runner.state().objects[&fish];
    assert!(obj.is_token, "CR 111.1: the gift Fish is a token");
    assert_eq!(obj.owner, P1, "CR 111.2: the recipient owns it");
    assert!(obj.tapped, "CR 702.174f: it enters tapped");
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(2), Some(2)),
        "P1's Pollen-Shield Hare pumps P1's creature token"
    );

    runner.cast(destroy).target_object(fish).commit();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&destroy].zone,
        Zone::Graveyard,
        "the destroy spell resolved"
    );
    assert!(
        !runner.state().battlefield.contains(&fish),
        "the gift Fish was destroyed"
    );
    assert_eq!(
        runner.state().objects.get(&fish).map(|obj| obj.zone),
        None,
        "CR 704.5d: the dead gift Fish ceases to exist instead of staying in a zone"
    );
    assert!(!runner.state().players[1].graveyard.contains(&fish));
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Hand,
        "the spell's own effect still resolved after the gift"
    );
}

/// CR 111.2 + CR 614.1a: the recipient creates the Fish, so the recipient's
/// Parallel Lives doubles it and the gift giver's does not.
#[test]
fn gift_fish_creation_meets_the_recipients_parallel_lives_only() {
    let (mut runner, flood_maw, _, bears) = setup(Some(P1));
    cast_and_resolve(&mut runner, flood_maw, bears);
    assert_eq!(
        p1_fish(&runner).len(),
        2,
        "P1's Parallel Lives doubles P1's gift Fish"
    );

    let (mut runner, flood_maw, _, bears) = setup(Some(P0));
    cast_and_resolve(&mut runner, flood_maw, bears);
    assert_eq!(
        p1_fish(&runner).len(),
        1,
        "P0's Parallel Lives does not apply"
    );
}
