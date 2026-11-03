//! Shared man-land (creature-land) reasoning.
//!
//! One structural detector for the "activated ability that turns this permanent
//! into a creature" shape, the animated body it produces, and whether activating
//! it must leave the source tapped. Detection is structural — an
//! [`Effect::Animate`], or a [`Effect::GenericEffect`] whose continuous
//! modifications add the Creature type — never a card-name list, so it covers the
//! whole class (Mutavault, Treetop Village, Celestial Colonnade, Raging Ravine,
//! Lumbering Falls, Shambling Vent, ...).
//!
//! Two consumers:
//! - [`crate::policies::land_animation`] — own-side timing ("should I animate
//!   this land now?").
//! - [`crate::combat_ai`] — opponent-side foresight ("an un-animated man-land the
//!   defender can still turn on is a latent blocker", CR 509.1a).

use std::collections::HashSet;

use engine::database::synthesis::KeywordTriggerInstaller;
use engine::game::ability_utils::build_resolved_from_def;
use engine::game::casting::can_pay_ability_mana_cost_after_auto_tap_excluding;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply_as_current_for_simulation;
use engine::game::layers::flush_layers;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, CostCategory, Effect, PtValue,
    StaticDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, StackEntryKind, TransientContinuousEffect, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

/// True when activating `ability` turns **its own source** into a creature.
/// Structural: walks the ability's full effect chain (including sub-abilities and
/// modal branches, via [`crate::cast_facts::collect_definition_effects`]).
///
/// Attachment-targeted animation is explicitly unsupported: a Genju-style Aura
/// whose ability makes its *enchanted* land a creature (`affected: AttachedTo`,
/// or an `Effect::Animate` targeting anything but `SelfRef`) is NOT reported —
/// its source (the Aura) does not become a blocker (CR 201.5b).
pub(crate) fn animates_source(ability: &AbilityDefinition) -> bool {
    crate::cast_facts::collect_definition_effects(ability)
        .into_iter()
        .any(effect_animates_self)
}

/// CR 201.5b: `SelfRef` (or unset) refers to the ability's own source. Anything
/// else names a different permanent — a Genju-style Aura's `AttachedTo`, a
/// targeted animate.
fn filter_is_self(target: &Option<TargetFilter>) -> bool {
    matches!(target, None | Some(TargetFilter::SelfRef))
}

fn static_targets_self(static_ability: &StaticDefinition) -> bool {
    filter_is_self(&static_ability.affected)
}

fn effect_animates_self(effect: &Effect) -> bool {
    match effect {
        Effect::Animate {
            target,
            types,
            remove_types,
            ..
        } => {
            matches!(target, TargetFilter::SelfRef)
                && types.iter().any(|t| t.eq_ignore_ascii_case("creature"))
                && !remove_types
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case("creature"))
        }
        // A `GenericEffect` whose own target — or any of its statics' `affected`
        // scope — names a non-self permanent applies elsewhere, not to the
        // source.
        Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } => {
            filter_is_self(target)
                && static_abilities.iter().any(|static_ability| {
                    static_targets_self(static_ability)
                        && static_ability.modifications.iter().any(adds_creature_type)
                })
        }
        _ => false,
    }
}

fn adds_creature_type(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddType {
            core_type: CoreType::Creature
        }
    )
}

/// Mana value of `ability`'s activation cost, or `None` when the cost is absent,
/// non-mana, or dynamically priced (no current man-land has such a cost — those
/// fall through to the caller's conservative handling).
pub(crate) fn animation_mana_value(ability: &AbilityDefinition) -> Option<u32> {
    match ability.cost.as_ref()? {
        AbilityCost::Mana { cost } => Some(cost.mana_value()),
        _ => None,
    }
}

/// The base power / toughness / keywords the animation grants. Reads
/// `SetPower` / `SetToughness` / `AddKeyword` continuous modifications from a
/// `GenericEffect`, and `PtValue::Fixed` power/toughness plus `keywords` from an
/// `Effect::Animate`. A dimension the animation does not set stays `0` — matching
/// the engine's `SetPower`/`SetToughness` semantics for a land with no printed
/// P/T.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AnimatedBody {
    pub power: i32,
    pub toughness: i32,
    pub keywords: Vec<Keyword>,
}

impl AnimatedBody {
    pub(crate) fn has_keyword(&self, keyword: &Keyword) -> bool {
        self.keywords.contains(keyword)
    }
}

fn merge_keyword(list: &mut Vec<Keyword>, keyword: &Keyword) {
    if !list.contains(keyword) {
        list.push(keyword.clone());
    }
}

/// The self-animated body, or `None` when the ability offers more than one
/// mutually exclusive body (a modal "becomes X, or becomes Y" — no single legal
/// activation produces a merged body, so treating it as one blocker would be
/// wrong). Only self-scoped animation contributes (see [`effect_animates_self`]).
pub(crate) fn extract_body(ability: &AbilityDefinition) -> Option<AnimatedBody> {
    let mut body = AnimatedBody::default();
    let mut set_power: Option<i32> = None;
    let mut set_toughness: Option<i32> = None;
    // Reject a modal ability whose branches disagree on the body's stats.
    let mut conflict = false;
    let assign = |slot: &mut Option<i32>, value: i32, conflict: &mut bool| {
        if slot.is_some_and(|prev| prev != value) {
            *conflict = true;
        }
        *slot = Some(value);
    };

    for effect in crate::cast_facts::collect_definition_effects(ability) {
        match effect {
            Effect::GenericEffect {
                static_abilities,
                target,
                ..
            } if filter_is_self(target) => {
                for modification in static_abilities
                    .iter()
                    .filter(|sa| static_targets_self(sa))
                    .flat_map(|sa| sa.modifications.iter())
                {
                    match modification {
                        ContinuousModification::SetPower { value } => {
                            assign(&mut set_power, *value, &mut conflict)
                        }
                        ContinuousModification::SetToughness { value } => {
                            assign(&mut set_toughness, *value, &mut conflict)
                        }
                        ContinuousModification::AddKeyword { keyword } => {
                            merge_keyword(&mut body.keywords, keyword)
                        }
                        _ => {}
                    }
                }
            }
            Effect::Animate {
                power,
                toughness,
                keywords,
                target: TargetFilter::SelfRef,
                ..
            } => {
                if let Some(PtValue::Fixed(value)) = power {
                    assign(&mut set_power, *value, &mut conflict);
                }
                if let Some(PtValue::Fixed(value)) = toughness {
                    assign(&mut set_toughness, *value, &mut conflict);
                }
                for keyword in keywords {
                    merge_keyword(&mut body.keywords, keyword);
                }
            }
            _ => {}
        }
    }

    if conflict {
        return None;
    }
    body.power = set_power.unwrap_or(0);
    body.toughness = set_toughness.unwrap_or(0);
    Some(body)
}

fn cost_taps_source(ability: &AbilityDefinition) -> bool {
    ability.cost.as_ref().is_some_and(|cost| {
        cost.categories()
            .into_iter()
            .any(|category| category == CostCategory::TapsSelf)
    })
}

/// CR 508.1a / CR 509.1a: returns true when activating `ability` must leave the
/// source `source_id` tapped — so the animated creature could neither attack nor
/// block this turn. Three ways this happens:
///   1. the source is already tapped,
///   2. the activation cost itself taps the source,
///   3. the mana cost cannot be paid from any source other than this one.
///
/// `player` is the source's controller. `ability_index` is the source's ability
/// index in the engine's enumerated space.
pub(crate) fn activation_leaves_source_tapped(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    ability: &AbilityDefinition,
) -> bool {
    let already_tapped = state
        .objects
        .get(&source_id)
        .map(|object| object.tapped)
        .unwrap_or(true);
    if already_tapped || cost_taps_source(ability) {
        return true;
    }

    // Only plain mana costs are assessable here; a non-mana / dynamically-priced
    // animation cost falls through as "does not force a tap".
    let Some(AbilityCost::Mana { cost }) = ability.cost.as_ref() else {
        return false;
    };
    if cost.mana_value() == 0 {
        return false;
    }

    let excluded = HashSet::from([source_id]);
    !can_pay_ability_mana_cost_after_auto_tap_excluding(
        state,
        player,
        source_id,
        Some(ability_index),
        cost,
        &excluded,
    )
}

/// True when every effect in `ability`'s chain is a self-scoped continuous
/// effect whose modifications are only fixed type, color or P/T values, or a
/// keyword grant that [`keyword_grant_collapses`]. A counter, a granted trigger, or any other
/// modification makes it false.
pub(crate) fn repeat_only_resets_characteristics(ability: &AbilityDefinition) -> bool {
    crate::cast_facts::collect_definition_effects(ability)
        .into_iter()
        .all(effect_only_resets_characteristics)
}

fn effect_only_resets_characteristics(effect: &Effect) -> bool {
    match effect {
        // Every field named, no `..`: a field added to either variant must be
        // classified here.
        Effect::GenericEffect {
            static_abilities,
            duration: _,
            target,
            end_cost: None,
        } => {
            filter_is_self(target)
                && static_abilities.iter().all(|static_ability| {
                    static_targets_self(static_ability)
                        && static_ability
                            .modifications
                            .iter()
                            .all(modification_only_resets_characteristics)
                })
        }
        Effect::Animate {
            power,
            toughness,
            types: _,
            remove_types: _,
            target: TargetFilter::SelfRef,
            keywords,
        } => {
            [power, toughness]
                .into_iter()
                .all(|value| matches!(value, None | Some(PtValue::Fixed(_))))
                && keywords.iter().all(keyword_grant_collapses)
        }
        _ => false,
    }
}

fn modification_only_resets_characteristics(modification: &ContinuousModification) -> bool {
    match modification {
        ContinuousModification::AddKeyword { keyword } => keyword_grant_collapses(keyword),
        // CR 613.1d / CR 613.1e / CR 613.4b: fixed type, color and P/T values.
        ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveAllSubtypes { .. } => true,
        _ => false,
    }
}

/// CR 113.2c: a second instance of an ability may or may not produce more
/// effects than one. The engine keeps a second instance of a granted keyword
/// only when [`Keyword::instances_must_coexist`], and installs a trigger per
/// granting effect when [`KeywordTriggerInstaller::triggers_for`] is non-empty;
/// this is true when neither applies.
fn keyword_grant_collapses(keyword: &Keyword) -> bool {
    !keyword.instances_must_coexist() && KeywordTriggerInstaller::triggers_for(keyword).is_empty()
}

/// True when `source_id` is a creature or an activated ability of it is on the
/// stack, `ability`'s chain only resets characteristics
/// ([`repeat_only_resets_characteristics`]), and [`settles_alike`] holds for
/// the board [`settle`]d without the repeat and the board settled with it
/// resolved first.
pub(crate) fn repeat_gains_nothing(
    state: &GameState,
    controller: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
) -> bool {
    let pending: Vec<ObjectId> = state
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::ActivatedAbility { source_id: pending_source, .. }
                    if *pending_source == source_id
            )
        })
        .map(|entry| entry.id)
        .collect();
    // Returns before the board is copied.
    if (pending.is_empty()
        && !state
            .objects
            .get(&source_id)
            .is_some_and(|object| object.card_types.core_types.contains(&CoreType::Creature)))
        || !repeat_only_resets_characteristics(ability)
    {
        return false;
    }
    let mut before = state.clone();
    flush_layers(&mut before);
    let mut after = before.clone();
    let resolved = build_resolved_from_def(ability, source_id, controller);
    if resolve_ability_chain(&mut after, &resolved, &mut Vec::new(), 0).is_err() {
        return false;
    }
    flush_layers(&mut after);
    match (settle(before, &pending), settle(after, &pending)) {
        (Some(without), Some(with)) => settles_alike(&without, &with, source_id),
        _ => false,
    }
}

/// How many passes [`settle`] makes before it gives up.
const SETTLE_PASS_LIMIT: usize = 100;

/// CR 117.4 + CR 405.5: `board` at the first priority, reached by every player
/// passing in succession, at which none of `pending` is on the stack. `None`
/// when reaching it takes any decision other than a pass, or once it has made
/// [`SETTLE_PASS_LIMIT`] passes.
fn settle(mut board: GameState, pending: &[ObjectId]) -> Option<GameState> {
    for _ in 0..SETTLE_PASS_LIMIT {
        if !matches!(board.waiting_for, WaitingFor::Priority { .. }) {
            return None;
        }
        if !board.stack.iter().any(|entry| pending.contains(&entry.id)) {
            return Some(board);
        }
        apply_as_current_for_simulation(&mut board, GameAction::PassPriority).ok()?;
    }
    None
}

/// CR 613.7 + CR 613.7b + CR 611.2a: `source_id` has the same public
/// characteristics on both boards, or is on neither, and every continuous
/// effect on `with` is also on `without` with every field but its id and
/// timestamp equal, its duration among them.
fn settles_alike(without: &GameState, with: &GameState, source_id: ObjectId) -> bool {
    let characteristics = |board: &GameState| {
        board
            .objects
            .get(&source_id)
            .map(|object| object.snapshot_public_characteristics())
    };
    characteristics(without) == characteristics(with)
        && with.transient_continuous_effects.iter().all(|effect| {
            without
                .transient_continuous_effects
                .iter()
                .any(|old| identity_free(old) == identity_free(effect))
        })
}

/// `effect` with the id and timestamp each installation mints cleared, so two
/// installations of the same effect compare equal.
fn identity_free(effect: &TransientContinuousEffect) -> TransientContinuousEffect {
    TransientContinuousEffect {
        id: 0,
        timestamp: 0,
        ..effect.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Duration, Effect, StaticDefinition, TargetFilter,
    };
    use engine::types::mana::ManaCost;
    use engine::types::statics::StaticMode;

    fn animate_ability(generic: u32, mods: Vec<ContinuousModification>) -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GenericEffect {
                static_abilities: vec![
                    StaticDefinition::new(StaticMode::Continuous).modifications(mods)
                ],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
                end_cost: None,
            },
        );
        ability.cost = Some(AbilityCost::Mana {
            cost: ManaCost::generic(generic),
        });
        ability
    }

    #[test]
    fn detects_generic_effect_animation() {
        let ability = animate_ability(
            1,
            vec![
                ContinuousModification::SetPower { value: 2 },
                ContinuousModification::SetToughness { value: 2 },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
            ],
        );
        assert!(animates_source(&ability));
    }

    #[test]
    fn non_animation_ability_is_not_detected() {
        let ability = animate_ability(1, vec![ContinuousModification::SetPower { value: 5 }]);
        assert!(!animates_source(&ability));
    }

    #[test]
    fn extract_body_reads_pt_and_keywords() {
        let ability = animate_ability(
            2,
            vec![
                ContinuousModification::SetPower { value: 3 },
                ContinuousModification::SetToughness { value: 3 },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Trample,
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
            ],
        );
        assert_eq!(
            extract_body(&ability),
            Some(AnimatedBody {
                power: 3,
                toughness: 3,
                keywords: vec![Keyword::Trample],
            })
        );
    }

    #[test]
    fn animation_mana_value_reads_generic_cost() {
        let ability = animate_ability(3, vec![]);
        assert_eq!(animation_mana_value(&ability), Some(3));
    }

    /// B9: an ability that animates its *attached* permanent (a Genju-style
    /// Aura) does not turn its own source into a blocker.
    #[test]
    fn attachment_targeted_animation_does_not_animate_the_source() {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::Continuous)
                    .affected(TargetFilter::AttachedTo)
                    .modifications(vec![
                        ContinuousModification::SetPower { value: 8 },
                        ContinuousModification::SetToughness { value: 12 },
                        ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        },
                    ])],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
                end_cost: None,
            },
        );
        ability.cost = Some(AbilityCost::Mana {
            cost: ManaCost::generic(2),
        });

        assert!(!animates_source(&ability));
        assert_eq!(extract_body(&ability), Some(AnimatedBody::default()));
    }

    /// Non-blocking review note: a modal "becomes X or becomes Y" ability yields
    /// no single body, so `extract_body` returns `None`.
    #[test]
    fn modal_animation_with_conflicting_bodies_yields_no_body() {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::Continuous)
                    .modifications(vec![
                        ContinuousModification::SetPower { value: 3 },
                        ContinuousModification::SetToughness { value: 3 },
                        ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        },
                    ])],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
                end_cost: None,
            },
        );
        ability.else_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::Continuous)
                    .modifications(vec![
                        ContinuousModification::SetPower { value: 5 },
                        ContinuousModification::SetToughness { value: 1 },
                        ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        },
                    ])],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
                end_cost: None,
            },
        )));
        ability.cost = Some(AbilityCost::Mana {
            cost: ManaCost::generic(2),
        });

        assert!(animates_source(&ability));
        assert_eq!(extract_body(&ability), None);
    }

    #[test]
    fn keyword_grant_repeat_classification() {
        assert!(
            keyword_grant_collapses(&Keyword::Trample),
            "control: a deduped keyword collapses"
        );
        assert!(
            !keyword_grant_collapses(&Keyword::Toxic(1)),
            "coexisting instances accumulate"
        );
        assert!(
            !keyword_grant_collapses(&Keyword::Flanking),
            "companion triggers accumulate"
        );
    }
}
