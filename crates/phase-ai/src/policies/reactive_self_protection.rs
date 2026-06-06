//! Reactive self-protection tactical policy.
//!
//! Penalises the AI for casting OR activating "save yourself" effects when
//! there is no immediate threat to react to. Empirically observed: AI casting
//! Teferi's Protection on turn 3 against an empty board; AI repeatedly paying
//! "discard a card: ~ gains protection from everything" until its hand is
//! empty; AI activating Sylvan Safekeeper ("sacrifice a land: target creature
//! you control gains shroud") on turn 1 for no reason. All are the same class:
//! paying a cost for a defensive grant with no work to do.
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
        &[DecisionKind::CastSpell, DecisionKind::ActivateAbility]
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
        if !matches!(
            ctx.candidate.action,
            GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. }
        ) {
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

/// Returns true if any of four threat signals is present:
///   - Stack contains an opponent-controlled object whose targets include
///     the AI player or any AI-controlled permanent (CR 117.1a — instants
///     are how protection responds to spells already on the stack).
///   - Stack contains an opponent-controlled untargeted mass-removal /
///     mass-bounce / mass-exile effect (Wrath of God, Damnation, Cyclonic
///     Rift, etc.) — these have no `targets`, so the targeted-threat check
///     above never sees them, but Heroic Intervention is exactly the right
///     answer.
///   - The AI's own life total is below 40% of starting life.
///   - Some opponent's `threat_level` is at or above `THREAT_FLOOR`.
fn any_immediate_threat(state: &GameState, ai_player: PlayerId) -> bool {
    if any_stack_targets_ai_or_ai_permanent(state, ai_player) {
        return true;
    }
    if any_stack_has_untargeted_mass_threat(state, ai_player) {
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

/// Returns true if the stack contains an opponent-controlled mass-removal /
/// mass-bounce / mass-exile effect — i.e., an effect that hits the AI's
/// board without naming a specific target. Conservative: any such mass effect
/// is treated as hostile, even if its filter excludes the AI's permanents
/// (rare and not worth the analysis cost — over-permitting a defensive cast
/// is strictly better than under-permitting).
fn any_stack_has_untargeted_mass_threat(state: &GameState, ai_player: PlayerId) -> bool {
    use engine::types::zones::Zone;
    state.stack.iter().any(|entry| {
        if entry.controller == ai_player {
            return false;
        }
        let Some(ability) = entry.ability() else {
            return false;
        };
        matches!(
            &ability.effect,
            Effect::DestroyAll { .. }
                | Effect::DamageAll { .. }
                | Effect::BounceAll { .. }
                | Effect::ChangeZoneAll {
                    destination: Zone::Exile | Zone::Graveyard | Zone::Hand,
                    ..
                }
        )
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
        // modes to the caster's own permanents. The grant's scope may live on
        // the static's `affected` filter (self-referential: "~ gains shroud")
        // OR on the enclosing `GenericEffect.target` when `affected` is
        // `ParentTarget` (targeted: "target creature you control gains shroud").
        Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } => static_abilities
            .iter()
            .any(|sd| static_definition_is_self_protection(sd, target.as_ref())),
        _ => false,
    }
}

fn static_definition_is_self_protection(
    sd: &StaticDefinition,
    parent_target: Option<&TargetFilter>,
) -> bool {
    let affects_self = match sd.affected.as_ref() {
        // A static scoped to ParentTarget grants to whatever the parent ability
        // targets (e.g. Sylvan Safekeeper: "target creature you control gains
        // shroud"), so self-scoping is decided by the parent ability's target
        // filter, not by `affected`.
        Some(TargetFilter::ParentTarget) => parent_target.is_some_and(target_filter_self_scoped),
        Some(f) => target_filter_self_scoped(f),
        None => false,
    };
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
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, QuantityExpr, StaticDefinition, TypedFilter,
    };
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::zones::Zone;
    use std::sync::Arc;

    use crate::config::AiConfig;
    use crate::context::AiContext;

    const AI: PlayerId = PlayerId(0);

    /// Build a `GenericEffect` keyword grant with explicit `affected` (static
    /// scope) and `target` (parent-ability scope) filters — the two axes that
    /// decide self-scoping.
    fn grant_effect(
        affected: Option<TargetFilter>,
        target: Option<TargetFilter>,
        keyword: Keyword,
    ) -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected,
                modifications: vec![ContinuousModification::AddKeyword { keyword }],
                condition: None,
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
                attack_defended: None,
            }],
            target,
            duration: None,
        }
    }

    /// Battlefield object controlled by the AI with a single activated ability
    /// whose effect is `effect` (ability index 0).
    fn ai_object_with_activated(state: &mut GameState, effect: Effect) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Self-Protector".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities)
            .push(AbilityDefinition::new(AbilityKind::Activated, effect));
        id
    }

    /// Run `ReactiveSelfProtectionPolicy::verdict` for activating `source_id`'s
    /// ability 0 — drives the real policy path for an `ActivateAbility` candidate.
    fn activate_verdict(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        ReactiveSelfProtectionPolicy.verdict(&ctx)
    }

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
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
                attack_defended: None,
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
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
                attack_defended: None,
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
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
                attack_defended: None,
            }],
            target: None,
            duration: None,
        };
        assert!(is_self_protection_effect(&effect));
    }

    // ───────── #3: activation cost-benefit (extend to ActivateAbility) ────────

    /// Classifier: a targeted grant scoped to ParentTarget with a self-scoped
    /// parent target (Sylvan Safekeeper shape) IS self-protection.
    /// Discriminating for the part-3 classifier extension.
    #[test]
    fn classifier_recognises_parent_target_grant_to_you() {
        assert!(is_self_protection_effect(&grant_effect(
            Some(TargetFilter::ParentTarget),
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            )),
            Keyword::Shroud,
        )));
    }

    /// Classifier: the same shape but granting to an OPPONENT's creature is NOT
    /// self-protection (parent target is opponent-scoped).
    #[test]
    fn classifier_rejects_parent_target_grant_to_opponent() {
        assert!(!is_self_protection_effect(&grant_effect(
            Some(TargetFilter::ParentTarget),
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            )),
            Keyword::Shroud,
        )));
    }

    /// Runtime: activating a SelfRef defensive grant ("~ gains indestructible")
    /// with no threat present is penalized. Discriminating for the
    /// decision_kinds/guard widening (revert → `_na`).
    #[test]
    fn activation_self_ref_protection_no_threat_penalized() {
        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        match activate_verdict(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_no_threat");
                assert_eq!(delta, NO_THREAT_PENALTY);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }

    /// Runtime: activating a ParentTarget grant to a creature you control
    /// ("sac a land: target creature you control gains shroud", Sylvan
    /// Safekeeper) with no threat is penalized. Discriminating for part 3
    /// (revert the ParentTarget classifier branch → `_na`).
    #[test]
    fn activation_parent_target_protection_no_threat_penalized() {
        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(
                Some(TargetFilter::ParentTarget),
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                Keyword::Shroud,
            ),
        );
        match activate_verdict(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_no_threat");
                assert_eq!(delta, NO_THREAT_PENALTY);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }

    /// Runtime guard: a non-protection activated ability (draw) is unaffected.
    #[test]
    fn activation_non_protection_unaffected() {
        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        match activate_verdict(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }

    /// Runtime guard: with an opponent removal spell on the stack targeting the
    /// AI's permanent, the protection activation IS allowed (threat present).
    #[test]
    fn activation_self_protection_with_threat_allowed() {
        use engine::types::ability::{ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        // Opponent Destroy spell on the stack targeting the AI's permanent.
        let opp = PlayerId(1);
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
            vec![TargetRef::Object(id)],
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

        match activate_verdict(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_threat_present");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }
}
