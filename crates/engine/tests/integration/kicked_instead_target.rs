//! "If [the additional cost was paid], instead … target" spells declare their
//! replacement target once.
//!
//! CR 601.2c: a spell may require alternative targets only if an additional
//! cost was chosen, and the chosen objects each become a target of that spell
//! (abilities that trigger when they become a target trigger at this point).
//! When the cost is paid the replacement clause's target is the spell's only
//! target, so it becomes a target once and emits one `BecomesTarget` event.
//! CR 702.33d (kicked), CR 702.174m (gift promised) and CR 702.194c (cast using
//! teamwork) are the printed gates.

use engine::game::derived_views::derive_views;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BLOODCHIEFS_THIRST: &str =
    "Kicker {2}{B} (You may pay an additional {2}{B} as you cast this spell.)\n\
Destroy target creature or planeswalker with mana value 2 or less. If this spell was kicked, \
instead destroy target creature or planeswalker.";

const INTO_THE_FLOOD_MAW: &str =
    "Gift a tapped Fish (You may promise an opponent a gift as you cast this spell. \
If you do, they create a tapped 1/1 blue Fish creature token before its other effects.)\n\
Return target creature an opponent controls to its owner's hand. If the gift was promised, \
instead return target nonland permanent an opponent controls to its owner's hand.";

const CRUEL_ALLIANCE: &str =
    "Teamwork 2 (As an additional cost to cast this spell, you may tap any number \
of creatures you control with total power 2 or more.)\n\
Exile target creature with mana value 3 or less. If this spell was cast using teamwork, instead \
exile target creature and you gain 3 life.";

const GIGGLING_SKITTERSPIKE: &str = "Indestructible\n\
Whenever this creature attacks, blocks, or becomes the target of a spell, it deals damage \
equal to its power to each opponent.\n\
{5}: Monstrosity 5. (If this creature isn't monstrous, put five +1/+1 counters on it and it \
becomes monstrous.)";

/// What a cast declared: the legal targets of each target prompt, and every
/// `BecomesTarget` event the spell emitted while being cast.
struct Declaration {
    prompts: Vec<Vec<TargetRef>>,
    becomes_target: Vec<TargetRef>,
}

fn mana_pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|&mana_type| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect()
}

/// Casts `spell`, answers its optional additional cost with `pay` (tapping `tap`
/// when that cost taps creatures), and selects `chosen` at the target prompt;
/// stops once the spell is on the stack.
fn declare(
    runner: &mut GameRunner,
    spell: ObjectId,
    pay: bool,
    tap: &[ObjectId],
    chosen: ObjectId,
) -> Declaration {
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the spell is castable")
        .events;
    let mut prompts = Vec::new();
    loop {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalCostChoice { .. } => GameAction::DecideOptionalCost { pay },
            WaitingFor::ChooseGiftRecipient { .. } => {
                GameAction::ChooseGiftRecipient { opponent: P1 }
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                prompts.extend(target_slots.iter().map(|slot| slot.legal_targets.clone()));
                GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(chosen)],
                }
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            } => GameAction::SelectCards {
                cards: tap.to_vec(),
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while casting: {other:?}"),
        };
        events.extend(runner.act(action).expect("the cast advances").events);
    }
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == spell),
        "the spell is on the stack"
    );
    let becomes_target = events
        .into_iter()
        .filter_map(|event| match event {
            GameEvent::BecomesTarget {
                target, source_id, ..
            } if source_id == spell => Some(target),
            _ => None,
        })
        .collect();
    Declaration {
        prompts,
        becomes_target,
    }
}

struct Thirst {
    runner: GameRunner,
    spell: ObjectId,
    small: ObjectId,
    big: ObjectId,
}

/// Which mana value 4 creature P1 controls beside its two small ones.
enum Big {
    HillGiant,
    GigglingSkitterspike,
}

/// P1 controls two creatures of mana value 2 or less (so each declaration is a
/// real prompt) and `big`, a mana value 4 creature only a paid "instead"
/// target allows. Returns the mana value 1 creature and `big`.
fn add_opponent_creatures(scenario: &mut GameScenario, big: Big) -> (ObjectId, ObjectId) {
    let small = scenario
        .add_creature(P1, "Elvish Mystic", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
    let big = match big {
        Big::HillGiant => scenario
            .add_creature(P1, "Hill Giant", 3, 3)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 3,
            })
            .id(),
        Big::GigglingSkitterspike => scenario
            .add_creature(P1, "Giggling Skitterspike", 1, 1)
            .from_oracle_text_with_keywords(
                &["Indestructible", "Monstrosity"],
                GIGGLING_SKITTERSPIKE,
            )
            .with_mana_cost(ManaCost::generic(4))
            .id(),
    };
    (small, big)
}

/// P0 holds Bloodchief's Thirst with mana for the kicker, against
/// [`add_opponent_creatures`].
fn thirst_against(big: Big) -> Thirst {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Bloodchief's Thirst", false)
        .from_oracle_text_with_keywords(&["Kicker"], BLOODCHIEFS_THIRST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let (small, big) = add_opponent_creatures(&mut scenario, big);
    scenario.with_mana_pool(
        P0,
        mana_pool(&[
            ManaType::Black,
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    Thirst {
        runner: scenario.build(),
        spell,
        small,
        big,
    }
}

/// CR 601.2c + CR 702.33d: kicked, Bloodchief's Thirst prompts once for its
/// replacement target, which admits the mana value 4 Hill Giant, and that
/// creature becomes a target once.
#[test]
fn kicked_bloodchiefs_thirst_declares_its_replacement_target_once() {
    let Thirst {
        mut runner,
        spell,
        small,
        big,
    } = thirst_against(Big::HillGiant);

    let declared = declare(&mut runner, spell, true, &[], big);

    assert_eq!(declared.prompts.len(), 1, "one target slot when kicked");
    assert!(declared.prompts[0].contains(&TargetRef::Object(big)));
    assert!(declared.prompts[0].contains(&TargetRef::Object(small)));
    assert_eq!(
        declared.becomes_target,
        vec![TargetRef::Object(big)],
        "the kicked target becomes a target of the spell exactly once"
    );

    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&big].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&small].zone, Zone::Battlefield);
}

/// CR 601.2c: the kicked Thirst's stack entry shows its one target once.
#[test]
fn kicked_bloodchiefs_thirst_displays_its_target_once() {
    let Thirst {
        mut runner,
        spell,
        big,
        ..
    } = thirst_against(Big::HillGiant);

    declare(&mut runner, spell, true, &[], big);

    let targets: Vec<TargetRef> = derive_views(runner.state(), Some(P0)).stack_entry_details
        [&spell]
        .targets
        .iter()
        .map(|display| display.target.clone())
        .collect();
    assert_eq!(targets, vec![TargetRef::Object(big)]);
}

/// CR 601.2c: an ability that triggers when its source becomes the target of
/// a spell triggers once for the kicked Thirst's single target, so Giggling
/// Skitterspike deals its 1 damage to P0 once.
#[test]
fn kicked_bloodchiefs_thirst_triggers_a_becomes_target_ability_once() {
    let Thirst {
        mut runner,
        spell,
        big,
        ..
    } = thirst_against(Big::GigglingSkitterspike);

    let declared = declare(&mut runner, spell, true, &[], big);
    assert_eq!(declared.becomes_target, vec![TargetRef::Object(big)]);

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 19, "Skitterspike triggers once");
    assert_eq!(
        runner.state().objects[&big].zone,
        Zone::Battlefield,
        "Skitterspike is indestructible"
    );
}

/// CR 601.2c: unkicked, the only target is the mana value 2 or less slot;
/// the mana value 4 creature is not a legal target.
#[test]
fn unkicked_bloodchiefs_thirst_declares_only_the_small_target() {
    let Thirst {
        mut runner,
        spell,
        small,
        big,
    } = thirst_against(Big::GigglingSkitterspike);

    let declared = declare(&mut runner, spell, false, &[], small);

    assert_eq!(declared.prompts.len(), 1, "one target slot when unkicked");
    assert!(declared.prompts[0].contains(&TargetRef::Object(small)));
    assert!(
        !declared.prompts[0].contains(&TargetRef::Object(big)),
        "only the kicked target admits a mana value 4 creature"
    );
    assert_eq!(declared.becomes_target, vec![TargetRef::Object(small)]);

    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&small].zone, Zone::Graveyard);
    assert_eq!(runner.life(P0), 20, "Skitterspike never became a target");
}

/// CR 601.2c + CR 702.174m: with the gift promised, Into the Flood Maw's
/// replacement target admits a noncreature permanent, which becomes a target
/// once.
#[test]
fn promised_into_the_flood_maw_declares_its_replacement_target_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Into the Flood Maw", true)
        .from_oracle_text_with_keywords(&["Gift"], INTO_THE_FLOOD_MAW)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let creature = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let artifact = scenario.add_artifact_from_oracle(P1, "Mind Stone", "").id();
    scenario.with_mana_pool(P0, mana_pool(&[ManaType::Blue]));
    let mut runner = scenario.build();

    let declared = declare(&mut runner, spell, true, &[], artifact);

    assert_eq!(declared.prompts.len(), 1, "one target slot when promised");
    assert!(declared.prompts[0].contains(&TargetRef::Object(artifact)));
    assert!(declared.prompts[0].contains(&TargetRef::Object(creature)));
    assert_eq!(
        declared.becomes_target,
        vec![TargetRef::Object(artifact)],
        "the promised target becomes a target of the spell exactly once"
    );

    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Hand);
}

/// CR 601.2c + CR 702.194c: cast using teamwork, Cruel Alliance prompts once for
/// its replacement target, which admits the mana value 4 Hill Giant, and that
/// creature becomes a target once.
#[test]
fn cruel_alliance_cast_using_teamwork_declares_its_replacement_target_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Cruel Alliance", false)
        .from_oracle_text_with_keywords(&["Teamwork"], CRUEL_ALLIANCE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 2,
        })
        .id();
    let tapper = scenario.add_creature(P0, "Teammate", 2, 2).id();
    let (small, big) = add_opponent_creatures(&mut scenario, Big::HillGiant);
    scenario.with_mana_pool(
        P0,
        mana_pool(&[ManaType::Black, ManaType::Colorless, ManaType::Colorless]),
    );
    let mut runner = scenario.build();

    let declared = declare(&mut runner, spell, true, &[tapper], big);

    assert!(runner.state().objects[&tapper].tapped, "teamwork was paid");
    assert_eq!(declared.prompts.len(), 1, "one target slot with teamwork");
    assert!(declared.prompts[0].contains(&TargetRef::Object(big)));
    assert!(declared.prompts[0].contains(&TargetRef::Object(small)));
    assert_eq!(
        declared.becomes_target,
        vec![TargetRef::Object(big)],
        "the teamwork target becomes a target of the spell exactly once"
    );

    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&big].zone, Zone::Exile);
    assert_eq!(runner.life(P0), 23);
}
