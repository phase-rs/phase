//! Reactive self-protection tactical policy.
//!
//! Penalises the AI for casting "save yourself" spells when there is no
//! immediate threat to react to. Empirically observed: AI casting Teferi's
//! Protection on turn 3 against an empty board — wasting a 3-mana reactive
//! spell that has no work to do.
//!
//! The classifier keys on **typed effect signatures**, not card names or
//! Oracle text. A spell is treated as self-protection when it grants
//! defensive keywords (Indestructible, Hexproof, Protection) or
//! defensive static modes (CantBeTargeted, CantLoseLife) to the caster's
//! own permanents/players, *or* phases the caster's permanents out
//! (Teferi's Protection-shaped). This generalises across the class:
//! Heroic Intervention, Make a Stand, Rootborn Defenses, Boros Charm
//! mode 2, Teferi's Protection, etc.
//!
//! Threat assessment reuses `eval::threat_level` (existing building block)
//! plus a low-life sentinel — no parallel heuristic is introduced.
//!
//! CR 117.1a: instants can be cast at any time priority is held — leaving
//! protection in hand for the moment a threat arrives is strictly better
//! than burning it pre-emptively.

use engine::types::ability::{
    ContinuousModification, ControllerRef, Effect, StaticDefinition, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::eval::threat_level;
use crate::features::DeckFeatures;

/// Threat-level threshold above which protection casts are unblocked.
/// `threat_level` is normalised 0..1; 0.45 corresponds to a meaningfully
/// developed opposing board (creatures + power) or a low life total.
const THREAT_FLOOR: f64 = 0.45;

/// Penalty applied when the AI tries to cast self-protection with no threat.
const NO_THREAT_PENALTY: f64 = -8.0;

pub struct ReactiveSelfProtectionPolicy;

impl TacticalPolicy for ReactiveSelfProtectionPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ReactiveSelfProtection
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Always active — applies to every deck. The classifier itself
        // returns false for non-protection spells.
        // activation-constant: classifier-gated reactive self-protection policy.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        if !matches!(ctx.candidate.action, GameAction::CastSpell { .. }) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("reactive_self_protection_na"),
            };
        }

        let effects = ctx.effects();
        if !effects
            .iter()
            .any(|e: &&Effect| is_self_protection_effect(e))
        {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("reactive_self_protection_na"),
            };
        }

        if any_immediate_threat(ctx.state, ctx.ai_player) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("reactive_self_protection_threat_present"),
            };
        }

        PolicyVerdict::Score {
            delta: NO_THREAT_PENALTY,
            reason: PolicyReason::new("reactive_self_protection_no_threat"),
        }
    }
}

/// Returns true if any of three threat signals is present:
///   - Stack contains an opponent-controlled object whose targets include
///     the AI player or any AI-controlled permanent (CR 117.1a — instants
///     are how protection responds to spells already on the stack).
///   - The AI's own life total is below 40% of starting life.
///   - Some opponent's `threat_level` is at or above `THREAT_FLOOR`.
///
/// Stack-targeted threats are the load-bearing signal for the user-reported
/// "opponent casts Doom Blade on my commander" scenario — neither board
/// pressure nor life ratio change in that moment, but Heroic Intervention
/// is exactly the right cast.
fn any_immediate_threat(state: &GameState, ai_player: PlayerId) -> bool {
    if any_stack_targets_ai_or_ai_permanent(state, ai_player) {
        return true;
    }
    let starting_life = state.format_config.starting_life.max(1) as f64;
    let life_ratio = state.players[ai_player.0 as usize].life as f64 / starting_life;
    if life_ratio < 0.4 {
        return true;
    }
    state.players.iter().any(|p| {
        if p.id == ai_player || p.is_eliminated {
            return false;
        }
        threat_level(state, ai_player, p.id) >= THREAT_FLOOR
    })
}

/// Returns true if any opponent-controlled stack entry targets the AI or an
/// AI-controlled object. Conservative — assumes any such target is hostile
/// rather than classifying the effect's polarity. Over-permitting a defensive
/// cast (rare false positives like opponent's "untap target permanent")
/// is strictly better than under-permitting (false negative = blowout).
fn any_stack_targets_ai_or_ai_permanent(state: &GameState, ai_player: PlayerId) -> bool {
    state.stack.iter().any(|entry| {
        if entry.controller == ai_player {
            return false;
        }
        let Some(ability) = entry.ability() else {
            return false;
        };
        ability.targets.iter().any(|t| match t {
            TargetRef::Player(pid) => *pid == ai_player,
            TargetRef::Object(obj_id) => state
                .objects
                .get(obj_id)
                .is_some_and(|obj| obj.controller == ai_player),
        })
    })
}

/// Effect-signature classifier: returns true when an `Effect` represents
/// "save yourself / your permanents." Conservative — false negatives only
/// cost a turn of not casting, false positives let the AI burn a defensive
/// spell prematurely (the worse of the two).
fn is_self_protection_effect(effect: &Effect) -> bool {
    match effect {
        // CR 702.26a: Phasing your own permanents out is a save-yourself
        // pattern (Teferi's Protection sub-effect).
        Effect::PhaseOut { target } => target_filter_self_scoped(target),
        // CR 615.1: Damage prevention shielding the caster.
        Effect::PreventDamage { .. } => true,
        // CR 604.3: Continuous static abilities granting defensive keywords or
        // modes to the caster's own permanents.
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities
            .iter()
            .any(static_definition_is_self_protection),
        _ => false,
    }
}

fn static_definition_is_self_protection(sd: &StaticDefinition) -> bool {
    let affects_self = sd.affected.as_ref().is_some_and(target_filter_self_scoped);
    if !affects_self {
        return false;
    }
    if static_mode_is_defensive(&sd.mode) {
        return true;
    }
    sd.modifications.iter().any(modification_is_defensive)
}

/// Defensive static modes — restricting outside interaction.
fn static_mode_is_defensive(mode: &StaticMode) -> bool {
    matches!(
        mode,
        StaticMode::CantBeTargeted
            | StaticMode::CantBeBlocked  // not strictly defensive, but rare on protection spells
            | StaticMode::CantLoseLife
            | StaticMode::Protection
    )
}

/// Defensive continuous modifications — keyword grants that prevent harm.
fn modification_is_defensive(m: &ContinuousModification) -> bool {
    match m {
        ContinuousModification::AddKeyword { keyword } => matches!(
            keyword,
            Keyword::Indestructible
                | Keyword::Hexproof
                | Keyword::HexproofFrom(_)
                | Keyword::Shroud
                | Keyword::Protection(_)
        ),
        _ => false,
    }
}

/// Returns true if the filter scopes effects to the source's controller
/// (the caster) — i.e., affects "you", "permanents you control", or the
/// source itself. The parser emits `TargetFilter::SelfRef` for ~570 cards
/// with "this permanent" / "~ has X" patterns; without `SelfRef` the
/// classifier silently misses every such self-buff.
fn target_filter_self_scoped(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Controller | TargetFilter::SelfRef => true,
        TargetFilter::Typed(tf) => matches!(tf.controller, Some(ControllerRef::You)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{ControllerRef, StaticDefinition, TypedFilter};

    fn indestructible_grant_to_self() -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                modifications: vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Indestructible,
                }],
                condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
            }],
            target: None,
            duration: None,
        }
    }

    #[test]
    fn classifier_recognises_self_indestructible_grant() {
        assert!(is_self_protection_effect(&indestructible_grant_to_self()));
    }

    #[test]
    fn classifier_recognises_self_phaseout() {
        let effect = Effect::PhaseOut {
            target: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
        };
        assert!(is_self_protection_effect(&effect));
    }

    #[test]
    fn classifier_rejects_opponent_indestructible_grant() {
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                modifications: vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Indestructible,
                }],
                condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
            }],
            target: None,
            duration: None,
        };
        assert!(!is_self_protection_effect(&effect));
    }

    #[test]
    fn classifier_ignores_unrelated_proliferate_effect() {
        assert!(!is_self_protection_effect(&Effect::Proliferate));
    }

    /// Regression: opponent's Doom Blade on the stack targeting the AI's
    /// commander is the canonical "cast Heroic Intervention now" trigger.
    /// Prior to the fix, `any_immediate_threat` only inspected board pressure
    /// and life ratio, so the policy still blocked the protection cast at
    /// the exact moment it was needed.
    #[test]
    fn stack_targeting_ai_permanent_counts_as_threat() {
        use engine::game::zones::create_object;
        use engine::types::ability::{ResolvedAbility, TargetFilter, TargetRef};
        use engine::types::game_state::{GameState, StackEntry, StackEntryKind};
        use engine::types::identifiers::CardId;
        use engine::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let ai_player = PlayerId(1);
        let opp = PlayerId(0);

        // AI controls a creature on battlefield.
        let ai_creature = create_object(
            &mut state,
            CardId(1),
            ai_player,
            "AI Creature".to_string(),
            Zone::Battlefield,
        );
        // Opponent has a Destroy spell on the stack targeting AI's creature.
        let spell_id = create_object(
            &mut state,
            CardId(99),
            opp,
            "Doom Blade".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(ai_creature)],
            spell_id,
            opp,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: opp,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        assert!(any_immediate_threat(&state, ai_player));
    }

    /// Sanity: with no stack, no attackers, full life, board empty → no
    /// threat. Reactive protection must NOT fire.
    #[test]
    fn no_threat_on_empty_state() {
        use engine::types::game_state::GameState;

        let state = GameState::new_two_player(42);
        assert!(!any_immediate_threat(&state, PlayerId(1)));
    }

    /// Regression: 570+ cards parse "this permanent gains X" with
    /// `affected = TargetFilter::SelfRef`. Prior to the fix, the
    /// classifier's `target_filter_self_scoped` only matched `Controller`
    /// and `Typed{controller: You}`, silently missing every self-targeted
    /// keyword grant.
    #[test]
    fn classifier_recognises_self_ref_indestructible_grant() {
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(TargetFilter::SelfRef),
                modifications: vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Indestructible,
                }],
                condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
            }],
            target: None,
            duration: None,
        };
        assert!(is_self_protection_effect(&effect));
    }
}
