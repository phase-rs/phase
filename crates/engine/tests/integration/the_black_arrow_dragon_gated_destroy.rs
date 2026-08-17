//! The Black Arrow's ETB rider destroys the damaged permanent only when it is a
//! Dragon.
//!
//! CR 120.3 + CR 608.2c: "If a Dragon is dealt damage this way, destroy it" is a
//! back-reference gated on the *recipient* of the preceding damage instruction.
//! Before the "dealt damage this way" damage channel existed in the condition
//! grammar, the clause head was dropped and the rider lowered to an
//! unconditional `Effect::Destroy { target: ParentTarget }` — killing every
//! damaged creature.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::{
    AbilityCondition, Comparator, DamageChannel, Effect, QuantityExpr, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const THE_BLACK_ARROW: &str = "Flash\n\
When The Black Arrow enters, it deals 1 damage to any target. \
If a Dragon is dealt damage this way, destroy it.\n\
Equipped creature gets +1/+1 and has reach.\n\
Equip {1}";

/// The damage recipient the ETB trigger is pointed at.
enum ArrowTarget {
    Dragon,
    NonDragon,
    Player,
}

#[test]
fn the_black_arrow_parses_dragon_gated_destroy_rider() {
    let parsed = parse_oracle_text(
        THE_BLACK_ARROW,
        "The Black Arrow",
        &["Flash".into()],
        &["Artifact".into()],
        &["Equipment".into()],
    );

    assert!(
        !parsed
            .abilities
            .iter()
            .chain(
                parsed
                    .triggers
                    .iter()
                    .filter_map(|trigger| trigger.execute.as_deref()),
            )
            .any(|ability| matches!(*ability.effect, Effect::Unimplemented { .. })),
        "no clause of The Black Arrow may fall back to Unimplemented: {parsed:?}"
    );

    let etb = parsed
        .triggers
        .first()
        .expect("The Black Arrow must have an enters-the-battlefield trigger")
        .execute
        .as_deref()
        .expect("the ETB trigger must have an effect");
    let rider = etb
        .sub_ability
        .as_ref()
        .expect("the damage clause must carry a conditional destroy rider");

    assert!(
        matches!(
            *rider.effect,
            Effect::Destroy {
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "the rider must destroy the damaged permanent: {:?}",
        rider.effect
    );

    let Some(AbilityCondition::And { conditions }) = &rider.condition else {
        panic!(
            "the destroy rider must retain its Dragon gate: {:?}",
            rider.condition
        );
    };

    // CR 615.1: prevented damage means nothing was "dealt damage this way".
    assert!(
        conditions.iter().any(|condition| matches!(
            condition,
            AbilityCondition::PreviousEffectAmount {
                comparator: Comparator::GT,
                rhs: QuantityExpr::Fixed { value: 0 },
                channel: DamageChannel::Total,
            }
        )),
        "the rider must require damage to actually be dealt: {conditions:?}"
    );
    // Without this guard the filter match falls back to the trigger source, which
    // in an ETB trigger is The Black Arrow itself.
    assert!(
        conditions
            .iter()
            .any(|condition| matches!(condition, AbilityCondition::HasObjectTarget)),
        "the rider must require an object recipient: {conditions:?}"
    );
    // CR 704.3: SBAs never run mid-resolution, so the damaged Dragon is still the
    // same battlefield object — present tense, not LKI.
    assert!(
        conditions.iter().any(|condition| matches!(
            condition,
            AbilityCondition::TargetMatchesFilter {
                use_lki: false,
                subject_slot: None,
                ..
            }
        )),
        "the rider must gate on the recipient's current characteristics: {conditions:?}"
    );

    assert!(
        !parsed.parse_warnings.iter().any(|warning| matches!(
            warning,
            OracleDiagnostic::SwallowedClause { detector, .. } if detector == "Condition_If"
        )),
        "the represented Dragon gate must not be reported as swallowed: {:?}",
        parsed.parse_warnings
    );
}

/// Casts The Black Arrow, points its ETB damage at `target`, and returns
/// `(victim_zone, victim_damage_marked, defending_player_life)`.
fn cast_the_black_arrow(target: ArrowTarget) -> (Zone, u32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let subtypes = match target {
        ArrowTarget::Dragon => vec!["Dragon"],
        // A Bear is the control case: same toughness, no Dragon subtype.
        ArrowTarget::NonDragon | ArrowTarget::Player => vec!["Bear"],
    };
    // Toughness 2 so 1 damage is never lethal on its own — any death must come
    // from the rider, not from CR 704.5g.
    let victim = scenario
        .add_creature(P1, "Damage Recipient", 2, 2)
        .with_subtypes(subtypes)
        .id();

    let arrow = scenario
        .add_artifact_to_hand_from_oracle(P0, "The Black Arrow", THE_BLACK_ARROW)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 3,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_901), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_902), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_903), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&arrow].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: arrow,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting The Black Arrow must succeed");

    let chosen = match target {
        ArrowTarget::Player => TargetRef::Player(P1),
        ArrowTarget::Dragon | ArrowTarget::NonDragon => TargetRef::Object(victim),
    };

    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![chosen.clone()],
                    })
                    .expect("selecting The Black Arrow's ETB target must succeed");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
            }
            other => panic!("unexpected The Black Arrow prompt: {other:?}"),
        }
    }

    let state = runner.state();
    let (zone, damage) = state
        .objects
        .get(&victim)
        .map_or((Zone::Graveyard, 0), |object| {
            (object.zone, object.damage_marked)
        });
    (zone, damage, state.players[P1.0 as usize].life)
}

#[test]
fn the_black_arrow_destroys_a_damaged_dragon() {
    let (zone, _, life) = cast_the_black_arrow(ArrowTarget::Dragon);
    assert_eq!(
        zone,
        Zone::Graveyard,
        "a Dragon dealt damage this way must be destroyed"
    );
    assert_eq!(life, 20, "damaging a creature must not change player life");
}

#[test]
fn the_black_arrow_spares_a_damaged_non_dragon() {
    let (zone, damage, life) = cast_the_black_arrow(ArrowTarget::NonDragon);
    assert_eq!(
        zone,
        Zone::Battlefield,
        "a non-Dragon dealt damage this way must survive — the destroy rider is Dragon-gated"
    );
    assert_eq!(damage, 1, "the non-Dragon must still take 1 damage");
    assert_eq!(life, 20, "damaging a creature must not change player life");
}

#[test]
fn the_black_arrow_damages_a_player_without_destroying_anything() {
    let (zone, damage, life) = cast_the_black_arrow(ArrowTarget::Player);
    assert_eq!(
        zone,
        Zone::Battlefield,
        "damaging a player must not reach an untargeted creature"
    );
    assert_eq!(damage, 0, "the untargeted creature must take no damage");
    assert_eq!(life, 19, "the targeted player must take 1 damage");
}
