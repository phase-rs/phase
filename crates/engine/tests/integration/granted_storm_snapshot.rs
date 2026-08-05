//! Regression for a cast-time Storm grant whose condition becomes false when
//! the spell is recorded. The trigger must therefore use the cast-finalization
//! snapshot rather than re-evaluating the live static after `SpellCast`.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    Comparator, ControllerRef, CountScope, QuantityExpr, QuantityRef, StaticCondition,
    StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::game_state::{StackEntryKind, SyntheticTriggerProvenance};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

const OPT_ORACLE: &str = "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.";

/// CR 601.2a + CR 611.2f + CR 702.40a: a Storm grant that only applies before
/// the caster has cast a spell this turn is latched before the spell enters the
/// cast ledger, then produces its trigger from that snapshot.
#[test]
fn first_spell_storm_grant_is_snapshotted_before_cast_recording() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Storm Grantor", 1, 1)
        .with_static_definition(
            StaticDefinition::new(StaticMode::CastWithKeyword {
                keyword: Keyword::Storm,
            })
            .affected(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Instant).controller(ControllerRef::You),
            ))
            .condition(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::SpellsCastThisTurn {
                        scope: CountScope::Controller,
                        filter: None,
                    },
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            }),
        );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, OPT_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let commit = runner.cast(spell).commit();
    let state = commit.state();

    assert_eq!(
        state
            .spells_cast_this_turn_by_player
            .get(&P0)
            .map_or(0, |spells| spells.len()),
        1,
        "the spell is recorded before its cast trigger is collected"
    );
    assert_eq!(
        state.objects[&spell].cast_spell_keywords,
        [Keyword::Storm],
        "cast finalization must preserve the pre-record Storm grant on the spell"
    );
    let storm_copy_counts: Vec<_> = state
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                provenance: Some(SyntheticTriggerProvenance::Storm { copy_count }),
                ..
            } => Some(*copy_count),
            StackEntryKind::TriggeredAbility {
                provenance: None, ..
            } => None,
            StackEntryKind::Spell { .. }
            | StackEntryKind::ActivatedAbility { .. }
            | StackEntryKind::KeywordAction { .. } => None,
        })
        .collect();
    assert_eq!(
        storm_copy_counts,
        [0],
        "the finalized snapshot must create exactly one zero-copy Storm trigger for the first spell"
    );
}
