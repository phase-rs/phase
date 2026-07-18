//! dq-f: "becomes a copy of target creature card in your graveyard with mana
//! value X" (Lazav, the Multifarious; Likeness Looter). The copy target's
//! mana-value clause TRAILS its zone clause ("… in your graveyard with mana value
//! X"). Before the zone-then-mana-value second pass in `parse_type_phrase_with_ctx`,
//! the trailing "with mana value X" clause was dropped, so (a) the target filter
//! carried no `FilterProp::Cmc` bound to the announced X, and (b) the leftover
//! "with mana value X" fragment sat in front of the ", except …" body and blocked
//! `parse_except_clause`, dropping the copy's `additional_modifications` entirely.
//!
//! `lazav_multifarious_copies_by_mana_value_x` drives the real activation
//! pipeline: with X = 2, only mana-value-2 graveyard creatures may be targeted.
//! `likeness_looter_parses_copy_mana_value_x_shape` pins the parsed AST shape
//! (Cmc{EQ, Variable("X")} in the target filter, plus non-empty
//! additional_modifications) that the runtime test relies on.

use engine::game::scenario::{GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    Comparator, ContinuousModification, Effect, FilterProp, QuantityExpr, QuantityRef,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

// Verbatim Scryfall {X} ability line (Lazav, the Multifarious, GRN).
const LAZAV_ORACLE: &str = "{X}: Lazav becomes a copy of target creature card in your graveyard with mana value X, except its name is Lazav, the Multifarious, it's legendary in addition to its other types, and it has this ability.";

// Verbatim Scryfall full Oracle text (Likeness Looter, MH3): the static Flying,
// the {T} loot ability (parsed.abilities[0]), and the {X} copy ability.
const LIKENESS_ORACLE: &str = "Flying\n{T}: Draw a card, then discard a card.\n{X}: This creature becomes a copy of target creature card in your graveyard with mana value X, except it has flying and this ability. Activate only as a sorcery.";

#[test]
fn lazav_multifarious_copies_by_mana_value_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Two colorless mana funds the {X} = {2} activation cost from the pool.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let lazav = scenario
        .add_creature(P0, "Lazav, the Multifarious", 1, 3)
        .from_oracle_text(LAZAV_ORACLE)
        .id();

    // Two mana-value-2 creatures keep target selection genuinely interactive
    // (no sole-legal-target auto-pick) and prove X is bound to 2.
    let mut mv2a = scenario.add_creature_to_graveyard(P0, "MV Two A", 2, 2);
    mv2a.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 2,
    });
    let mv2a = mv2a.id();
    let mut mv2b = scenario.add_creature_to_graveyard(P0, "MV Two B", 2, 2);
    mv2b.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 2,
    });
    let mv2b = mv2b.id();
    // Discriminator: mana-value-3 creature is illegal when X = 2.
    let mut mv3 = scenario.add_creature_to_graveyard(P0, "MV Three", 3, 3);
    mv3.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 3,
    });
    let mv3 = mv3.id();

    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&lazav]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::BecomeCopy { .. }))
        .expect("Lazav must have a BecomeCopy activated ability");

    runner
        .act(GameAction::ActivateAbility {
            source_id: lazav,
            ability_index,
        })
        .expect("activating Lazav's {X} ability must be accepted");

    let mut saw_choose_x = false;
    let mut saw_target = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: 2 })
                    .expect("announce X = 2");
                saw_choose_x = true;
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay {2} activation cost from the pool");
            }
            WaitingFor::TargetSelection { ref selection, .. } => {
                // X was bound to 2 before target legality was computed: the two
                // mana-value-2 creatures are legal, the mana-value-3 is not. If X
                // had failed closed to 0 (the pre-fix pin), the EQ-0 set would be
                // empty and no MV2 card would appear here.
                assert!(
                    selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(mv2a)),
                    "MV2 creature A must be legal when X = 2, got {:?}",
                    selection.current_legal_targets
                );
                assert!(
                    selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(mv2b)),
                    "MV2 creature B must be legal when X = 2, got {:?}",
                    selection.current_legal_targets
                );
                assert!(
                    !selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(mv3)),
                    "MV3 creature must NOT be legal when X = 2, got {:?}",
                    selection.current_legal_targets
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(mv2a)],
                    })
                    .expect("select an MV2 graveyard creature");
                saw_target = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve the copy ability");
            }
            other => panic!("unexpected waiting state during activation: {other:?}"),
        }
    }

    assert!(saw_choose_x, "activation must announce X");
    assert!(
        saw_target,
        "activation must choose a graveyard-creature target"
    );

    // Belt-and-suspenders: Lazav copies the MV2 creature's copiable P/T (2/2),
    // overriding its printed 1/3.
    assert_eq!(
        runner.state().objects[&lazav].power,
        Some(2),
        "Lazav must adopt the copied MV2 creature's power (2), not its printed 1"
    );
    assert_eq!(
        runner.state().objects[&lazav].toughness,
        Some(2),
        "Lazav must adopt the copied MV2 creature's toughness (2), not its printed 3"
    );
}

#[test]
fn likeness_looter_parses_copy_mana_value_x_shape() {
    // Mirror the production loader: MTGJSON supplies "Flying" as a keyword name, so
    // the bare "Flying" line is lifted into `extracted_keywords` (not an
    // Unimplemented ability). abilities then = [{T} loot (0), {X} copy (1)].
    let parsed = parse_oracle_text(
        LIKENESS_ORACLE,
        "Likeness Looter",
        &["Flying".to_string()],
        &["Creature".to_string()],
        &[],
    );

    // Reach-guard: nothing degraded to Unimplemented (the {T} loot ability and the
    // {X} copy ability both parse). A vacuous negative would otherwise pass.
    assert!(
        !parsed
            .abilities
            .iter()
            .any(|a| matches!(*a.effect, Effect::Unimplemented { .. })),
        "no ability may degrade to Unimplemented: {:?}",
        parsed.abilities
    );

    let copy = parsed
        .abilities
        .iter()
        .find(|a| matches!(*a.effect, Effect::BecomeCopy { .. }))
        .expect("Likeness Looter must have a BecomeCopy activated ability");

    let Effect::BecomeCopy {
        target,
        additional_modifications,
        ..
    } = &*copy.effect
    else {
        unreachable!("matched BecomeCopy above");
    };

    let TargetFilter::Typed(tf) = target else {
        panic!("copy target must be a typed filter, got {target:?}");
    };
    assert!(
        tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::Cmc {
                comparator: Comparator::EQ,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable { name },
                },
            } if name == "X"
        )),
        "copy target filter must carry Cmc{{EQ, Variable(\"X\")}}, got {:?}",
        tf.properties
    );

    // The ", except it has flying and this ability" body only reaches
    // parse_except_clause once the trailing "with mana value X" clause is consumed,
    // so a non-empty additional_modifications with Flying is a joint witness of the
    // fix.
    assert!(
        additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Flying
            }
        )),
        "copy must grant Flying via additional_modifications, got {additional_modifications:?}"
    );
}
