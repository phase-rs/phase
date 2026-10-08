//! Land-play sequencing tactical policy.
//!
//! CR 305.1: every `PlayLand` candidate is otherwise unscored — `PlayLand` is
//! declared in `BoardDevelopmentPolicy.decision_kinds()` but its `score()` only
//! handles `CastSpell`/`PassPriority`, so equal priors made land choice a
//! uniform sample. This policy scores the choice on three additive, card-local
//! terms, every one of them read from the land face actually being played (the
//! front face, or an MDFC's land back face — CR 712.12):
//!
//! 1. **Bounce-land deprioritization** (#ai-suggestions "AI Cast & Bounce
//!    lands"): a self-bouncing Ravnica/MOM bounce-land played first loses a land
//!    drop, so it is deferred while another non-bouncing land is playable.
//!    Deferred (#4b, a separate `SelectTarget` decision): choosing WHICH land
//!    the ETB returns needs its own target-selection policy.
//! 2. **Color demand** ("Land Selection": a third Plains over the Forest the
//!    only castable 3-drop needs): credit a land for each unmet color it covers
//!    — a color some near-term hand card demands (CR 107.4b) that the AI's own
//!    battlefield lands cannot yet produce.
//! 3. **Tempo riders** ("Gateway Plaza as the first land"): charge a land for an
//!    unconditional "enters tapped" replacement (CR 614.12) and again for an ETB
//!    "sacrifice it unless you pay" trigger (CR 118.12) — but only while a land
//!    carrying neither rider is also playable, since a tapland that is the only
//!    land is still the right play.
//!
//! Every detector is structural, never name-matched, and reads only the
//! candidate's own definitions plus one battlefield/hand scan, so the whole
//! verdict stays inside the per-candidate search budget. The bounce detector
//! deliberately does NOT match the Mercadian "Karoo"/Coral Atoll cycle, which
//! sacrifices itself rather than bouncing a land — that cycle is priced by the
//! unless-pay rider term instead.

use engine::ai_support::CandidateAction;
use engine::game::game_object::GameObject;
use engine::game::mana_payment::{mana_type_to_demand_index, outer_cost_color_demand, ColorDemand};
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, ReplacementDefinition, TapStateChange, TargetFilter,
    TriggerDefinition, TriggerEntry, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::mulligan::modal_back_face;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::features::DeckFeatures;
use crate::mana_colors::land_produced_color_types;

/// Penalty for playing a self-bouncing land while a non-bouncing land is also
/// currently playable. The non-bounce land's `PlayLand` scores `0.0` here, so
/// it wins the argmax and is played first; the bounce-land is only deferred
/// within the turn.
///
/// Deliberately a module constant rather than a `PolicyPenalties` knob: it is
/// the shipped, reviewed magnitude of the original bounce-land fix, and the two
/// new terms are seeded relative to it.
const BOUNCE_DEPRIORITIZE: f64 = 1.5;

/// Most unmet colors one land is credited with fixing. A dual covering two open
/// colors is a real gain over a basic; a five-color land is not five cards
/// better, and the uncapped sum would outrank every tempo rider.
const COLOR_FIX_CAP: usize = 2;

const REASON_NA: &str = "land_sequencing_na";
const REASON_PLAY_OTHER_FIRST: &str = "land_sequencing_play_other_first";
const REASON_NO_ALTERNATIVE: &str = "land_sequencing_no_alternative";
const REASON_TEMPO_RIDER: &str = "land_sequencing_tempo_rider";
const REASON_COLOR_DEMAND: &str = "land_sequencing_color_demand";

pub struct LandSequencingPolicy;

impl TacticalPolicy for LandSequencingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::LandSequencing
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::PlayLand]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Applies to every deck; the verdict's bounce-land guard self-gates.
        // activation-constant: land-play sequencing, universal.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let neutral = || PolicyVerdict::neutral(PolicyReason::new(REASON_NA));

        let GameAction::PlayLand { object_id, .. } = &ctx.candidate.action else {
            return neutral();
        };
        let object_id = *object_id;

        let Some(played) = ctx.state.objects.get(&object_id) else {
            return neutral();
        };
        // CR 712.12: an MDFC in hand is played as its land back face, and every
        // term below must read that face rather than the spell front.
        let Some(face) = land_face(played) else {
            return neutral();
        };

        // The decision candidates are the engine's current legal actions. A
        // land merely present in hand can be unavailable due to a play
        // restriction, so it must not cause the only legal bounce-land to be
        // deferred.
        let bounce = is_self_bounce_land(&face);
        let has_non_bounce_alternative = bounce
            && ctx.decision.candidates.iter().any(|candidate| {
                other_playable_land(ctx, object_id, candidate)
                    .is_some_and(|alternative| !is_self_bounce_land(&alternative))
            });

        let enters_tapped = enters_tapped_unconditionally(&face);
        let unless_pay = has_unless_pay_rider(&face);
        let riders = usize::from(enters_tapped) + usize::from(unless_pay);
        let has_untapped_alternative = riders > 0
            && ctx.decision.candidates.iter().any(|candidate| {
                other_playable_land(ctx, object_id, candidate)
                    .is_some_and(|alternative| !carries_tempo_rider(&alternative))
            });
        let charged_riders = if has_untapped_alternative { riders } else { 0 };

        let color_fixes = unmet_colors_covered(ctx, &face);

        let penalties = &ctx.config.policy_penalties;
        let delta = color_fixes as f64 * penalties.land_color_demand_unit
            - charged_riders as f64 * penalties.land_tempo_rider_penalty
            - if has_non_bounce_alternative {
                BOUNCE_DEPRIORITIZE
            } else {
                0.0
            };

        // One reason kind per verdict, by descending magnitude of the term that
        // dominates it: the bounce deprioritization outweighs a rider, which
        // outweighs the capped color bonus. Every term's size is a fact.
        let kind = if has_non_bounce_alternative {
            REASON_PLAY_OTHER_FIRST
        } else if charged_riders > 0 {
            REASON_TEMPO_RIDER
        } else if color_fixes > 0 {
            REASON_COLOR_DEMAND
        } else if bounce {
            // Bounce-land is the only land to play — let it through.
            REASON_NO_ALTERNATIVE
        } else {
            REASON_NA
        };

        PolicyVerdict::score(
            delta,
            PolicyReason::new(kind)
                .with_fact("color_fixes", color_fixes as i64)
                .with_fact("tempo_riders", charged_riders as i64),
        )
    }
}

/// The land face of another currently-legal `PlayLand` candidate, or `None`
/// when the candidate is this same land, is not a `PlayLand`, or has no land
/// face. Shared by both alternative scans so they agree on what "another land
/// is playable this turn" means.
fn other_playable_land<'a>(
    ctx: &PolicyContext<'a>,
    object_id: ObjectId,
    candidate: &CandidateAction,
) -> Option<LandFace<'a>> {
    let GameAction::PlayLand {
        object_id: alternative_id,
        ..
    } = &candidate.action
    else {
        return None;
    };
    if *alternative_id == object_id {
        return None;
    }
    land_face(ctx.state.objects.get(alternative_id)?)
}

/// The face a `PlayLand` actually puts onto the battlefield: the card's own
/// front face when it is a land, else its modal back face when THAT is a land
/// (CR 712.12). Mirrors the engine's own `PlayLand` candidate emission, so this
/// policy never scores a face the engine would not play.
struct LandFace<'a> {
    subtypes: &'a [String],
    abilities: &'a [AbilityDefinition],
    replacements: &'a [ReplacementDefinition],
    triggers: TriggerDefs<'a>,
}

/// Trigger definitions of a face. A live object carries identity-bearing
/// `TriggerEntry` values while a stored back face carries payload-only
/// `TriggerDefinition`s, so the two are read through one allocation-free
/// predicate rather than collected into a common shape.
enum TriggerDefs<'a> {
    Live(&'a [TriggerEntry]),
    Face(&'a [TriggerDefinition]),
}

impl TriggerDefs<'_> {
    fn any(&self, predicate: impl Fn(&TriggerDefinition) -> bool) -> bool {
        match self {
            Self::Live(entries) => entries.iter().any(|entry| predicate(&entry.definition)),
            Self::Face(definitions) => definitions.iter().any(predicate),
        }
    }
}

fn land_face(obj: &GameObject) -> Option<LandFace<'_>> {
    if obj.card_types.core_types.contains(&CoreType::Land) {
        return Some(LandFace {
            subtypes: &obj.card_types.subtypes,
            abilities: &obj.abilities,
            replacements: obj.replacement_definitions.as_slice(),
            triggers: TriggerDefs::Live(obj.trigger_definitions.as_slice()),
        });
    }
    let back = modal_back_face(obj)?;
    back.card_types
        .core_types
        .contains(&CoreType::Land)
        .then(|| LandFace {
            subtypes: &back.card_types.subtypes,
            abilities: &back.abilities,
            replacements: back.replacement_definitions.as_slice(),
            triggers: TriggerDefs::Face(back.trigger_definitions.as_slice()),
        })
}

/// True when the face has an ETB trigger that returns a land YOU control to hand
/// (the Ravnica/MOM bounce-land / "Karoo" cycle). Structural, not name-matched.
fn is_self_bounce_land(face: &LandFace<'_>) -> bool {
    face.triggers.any(|t| {
        is_etb_self_trigger(t)
            && t.execute
                .as_deref()
                .is_some_and(|exec| collect_chain_effects(exec).iter().any(bounces_own_land))
    })
}

/// CR 614.12: the face has an "enters tapped" self-replacement with no
/// condition attached. A CONDITIONAL one (check/fast/slow lands) is card-locally
/// unknowable — evaluating it needs the battlefield state the condition names —
/// so it is neither charged here nor counted as an untapped alternative below;
/// the land is simply neutral on this term.
fn enters_tapped_unconditionally(face: &LandFace<'_>) -> bool {
    face.replacements
        .iter()
        .any(|r| r.condition.is_none() && is_etb_self_tap_replacement(r))
}

/// CR 118.12: the face has an ETB "sacrifice it unless you [pay a cost]" trigger
/// — Gateway Plaza's `{1}`, Coral Atoll's "return an untapped Island". Any
/// `unless_pay` cost counts: the land is only kept by paying a real cost, which
/// is the tempo hit being priced, regardless of the currency.
fn has_unless_pay_rider(face: &LandFace<'_>) -> bool {
    face.triggers.any(|t| {
        is_etb_self_trigger(t)
            && t.unless_pay.is_some()
            && t.execute.as_deref().is_some_and(|exec| {
                collect_chain_effects(exec).into_iter().any(|effect| {
                    matches!(
                        effect,
                        Effect::Sacrifice {
                            target: TargetFilter::SelfRef,
                            ..
                        }
                    )
                })
            })
    })
}

/// Whether a land carries either tempo rider — the predicate that disqualifies
/// it from being the "untapped alternative" that makes a rider chargeable.
fn carries_tempo_rider(face: &LandFace<'_>) -> bool {
    enters_tapped_unconditionally(face)
        || conditionally_enters_tapped(face)
        || has_unless_pay_rider(face)
}

fn conditionally_enters_tapped(face: &LandFace<'_>) -> bool {
    face.replacements
        .iter()
        .any(|r| r.condition.is_some() && is_etb_self_tap_replacement(r))
}

/// CR 614.12: a self-replacement of this permanent's own battlefield entry that
/// taps it. Mirrors `cast_facts::qualifies_immediate_replacement`'s ETB shape.
fn is_etb_self_tap_replacement(replacement: &ReplacementDefinition) -> bool {
    matches!(
        replacement.event,
        ReplacementEvent::ChangeZone | ReplacementEvent::Moved
    ) && replacement.valid_card == Some(TargetFilter::SelfRef)
        && replacement.destination_zone == Some(Zone::Battlefield)
        && replacement.execute.as_deref().is_some_and(|exec| {
            collect_chain_effects(exec).into_iter().any(|effect| {
                matches!(
                    effect,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        state: TapStateChange::Tap,
                        ..
                    }
                )
            })
        })
}

fn is_etb_self_trigger(trigger: &TriggerDefinition) -> bool {
    trigger.mode == TriggerMode::ChangesZone
        && trigger.destination == Some(Zone::Battlefield)
        && matches!(trigger.valid_card, Some(TargetFilter::SelfRef))
}

/// How many colors the played land covers that the AI's near-term hand demands
/// and its battlefield cannot yet produce, capped at [`COLOR_FIX_CAP`].
///
/// Ordered so a colorless land pays for nothing: the candidate's own produced
/// colors are read first (card-local), and only a land that produces at least
/// one color triggers the battlefield and hand scans.
fn unmet_colors_covered(ctx: &PolicyContext<'_>, face: &LandFace<'_>) -> usize {
    let produced = land_produced_color_types(face.subtypes, face.abilities);
    if produced.is_empty() {
        return 0;
    }
    let (supply, land_count) = ai_land_supply(ctx.state, ctx.ai_player);
    let demand = hand_color_demand(ctx.state, ctx.ai_player, land_count);
    produced
        .into_iter()
        .filter_map(mana_type_to_demand_index)
        .filter(|&index| demand[index] > supply[index])
        .count()
        .min(COLOR_FIX_CAP)
}

/// One battlefield walk yielding both halves of the color-demand comparison:
/// how many of the AI's lands produce each color, and how many lands it controls
/// (the castability threshold the hand scan uses).
fn ai_land_supply(state: &GameState, player: PlayerId) -> (ColorDemand, u32) {
    let mut supply = [0u32; 5];
    let mut lands = 0;
    for obj in state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
    {
        if obj.controller != player || !obj.card_types.core_types.contains(&CoreType::Land) {
            continue;
        }
        lands += 1;
        for color in land_produced_color_types(&obj.card_types.subtypes, &obj.abilities) {
            if let Some(index) = mana_type_to_demand_index(color) {
                supply[index] += 1;
            }
        }
    }
    (supply, lands)
}

/// CR 107.4b: element-wise MAX colored-pip demand over the nonland cards in the
/// AI's hand that are within one land drop of being castable.
///
/// MAX, not sum: demand asks "how many sources of this color does one castable
/// card need," which is what a land drop can answer this turn — summing would
/// let a hand of three one-pip green cards claim three unmet green sources.
/// Castability is the same count-based check `ramp_timing` uses (CR 202.3 mana
/// value against lands controlled plus this turn's drop) — deliberately never an
/// affordability sweep, which is far too expensive per candidate.
fn hand_color_demand(state: &GameState, player: PlayerId, land_count: u32) -> ColorDemand {
    let mut demand = [0u32; 5];
    let Some(hand) = state.players.get(player.0 as usize).map(|p| &p.hand) else {
        return demand;
    };
    for obj in hand.iter().filter_map(|id| state.objects.get(id)) {
        if obj.card_types.core_types.contains(&CoreType::Land)
            || obj.mana_cost.mana_value() > land_count + 1
        {
            continue;
        }
        for (slot, needed) in demand
            .iter_mut()
            .zip(outer_cost_color_demand(&obj.mana_cost))
        {
            *slot = (*slot).max(needed);
        }
    }
    demand
}

fn bounces_own_land(effect: &&Effect) -> bool {
    matches!(effect, Effect::Bounce { target, .. } if target_is_own_land(target))
}

fn target_is_own_land(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(t)
            if t.controller == Some(ControllerRef::You)
                && t.type_filters.iter().any(type_filter_is_land)
    )
}

fn type_filter_is_land(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Land => true,
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_land),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{
        build_decision_context, ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass,
    };
    use engine::game::game_object::BackFaceData;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, BounceSelection, ChosenAttribute, EffectScope,
        GameRestriction, ProhibitedActivity, QuantityExpr, ReplacementCondition, RestrictionExpiry,
        RestrictionPlayerScope, TargetFilter, TriggerDefinition, TypedFilter, UnlessPayModifier,
    };
    use engine::types::card::LayoutKind;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaCost, ManaCostShard};
    use engine::types::zones::Zone;

    const AI: PlayerId = PlayerId(0);

    /// The three land-play magnitudes keep the ordering their docs promise: the
    /// capped color-fixing bonus never out-scores one charged tempo rider (at
    /// the two-color cap they cancel and the other terms decide), and a rider
    /// never out-scores the bounce-land deprioritization.
    #[test]
    fn land_play_magnitudes_keep_their_documented_ordering() {
        let penalties = AiConfig::default().policy_penalties;
        let max_fixing_bonus = penalties.land_color_demand_unit * COLOR_FIX_CAP as f64;
        assert!(
            max_fixing_bonus <= penalties.land_tempo_rider_penalty,
            "a tapped dual must not out-score an untapped basic on fixing alone: \
             capped bonus {max_fixing_bonus} vs rider {}",
            penalties.land_tempo_rider_penalty
        );
        assert!(
            penalties.land_tempo_rider_penalty < BOUNCE_DEPRIORITIZE,
            "a tempo rider must stay below the bounce deprioritization: rider {} vs {}",
            penalties.land_tempo_rider_penalty,
            BOUNCE_DEPRIORITIZE
        );
    }

    fn own_land_bounce_effect() -> Effect {
        Effect::Bounce {
            target: TargetFilter::Typed(
                TypedFilter::default()
                    .with_type(TypeFilter::Land)
                    .controller(ControllerRef::You),
            ),
            destination: None,
            selection: BounceSelection::default(),
        }
    }

    /// A bounce-land in hand with the SGC-shape ETB self-bounce trigger.
    fn bounce_land(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Growth Chamber".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    own_land_bounce_effect(),
                )),
        );
        id
    }

    fn plain_land(state: &mut GameState, name: &str) -> ObjectId {
        let id = create_object(state, CardId(2), AI, name.to_string(), Zone::Hand);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        id
    }

    /// A land in `zone` with one basic land subtype, so
    /// `mana_colors::land_produced_color_types` reads a color off it.
    fn typed_land(state: &mut GameState, name: &str, subtype: &str, zone: Zone) -> ObjectId {
        let id = create_object(state, CardId(4), AI, name.to_string(), zone);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push(subtype.to_string());
        id
    }

    fn hand_spell(
        state: &mut GameState,
        name: &str,
        shards: Vec<ManaCostShard>,
        generic: u32,
    ) -> ObjectId {
        let id = create_object(state, CardId(5), AI, name.to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = ManaCost::Cost { shards, generic };
        id
    }

    /// The "~ enters tapped" self-replacement the card data emits, optionally
    /// carrying the check/fast/slow-land condition.
    fn etb_tapped(condition: Option<ReplacementCondition>) -> ReplacementDefinition {
        let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ));
        match condition {
            Some(condition) => replacement.condition(condition),
            None => replacement,
        }
    }

    /// Gateway Plaza's "When ~ enters, sacrifice it unless you pay {1}."
    fn unless_pay_sacrifice() -> TriggerDefinition {
        TriggerDefinition {
            unless_pay: Some(UnlessPayModifier {
                cost: AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                payer: TargetFilter::Controller,
            }),
            ..TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Sacrifice {
                        target: TargetFilter::SelfRef,
                        count: QuantityExpr::Fixed { value: 1 },
                        min_count: 0,
                    },
                ))
        }
    }

    /// A hand land carrying the Gateway Plaza pair of riders.
    fn gateway_plaza(state: &mut GameState) -> ObjectId {
        let id = plain_land(state, "Gateway Plaza");
        let obj = state.objects.get_mut(&id).unwrap();
        obj.replacement_definitions.push(etb_tapped(None));
        obj.trigger_definitions.push(unless_pay_sacrifice());
        id
    }

    fn play_candidate(object_id: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::PlayLand {
                object_id,
                card_id: CardId(0),
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Land),
        }
    }

    fn play_verdict(
        state: &GameState,
        object_id: ObjectId,
        candidates: Vec<CandidateAction>,
    ) -> PolicyVerdict {
        let candidate = play_candidate(object_id);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates,
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        LandSequencingPolicy.verdict(&ctx)
    }

    fn assert_score(verdict: PolicyVerdict, kind: &str, delta: f64) {
        match verdict {
            PolicyVerdict::Score { delta: d, reason } => {
                assert_eq!(reason.kind, kind, "reason kind");
                assert_eq!(d, delta, "delta");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }

    #[test]
    fn bounce_land_deprioritized_when_alternative() {
        let mut state = GameState::new_two_player(42);
        let karoo = bounce_land(&mut state);
        let basic = plain_land(&mut state, "Forest");
        state.players[0].hand = [karoo, basic].into_iter().collect();
        assert_score(
            play_verdict(
                &state,
                karoo,
                vec![play_candidate(karoo), play_candidate(basic)],
            ),
            "land_sequencing_play_other_first",
            -BOUNCE_DEPRIORITIZE,
        );
    }

    #[test]
    fn bounce_land_alone_not_penalized() {
        let mut state = GameState::new_two_player(42);
        let karoo = bounce_land(&mut state);
        state.players[0].hand = [karoo].into_iter().collect();
        assert_score(
            play_verdict(&state, karoo, vec![play_candidate(karoo)]),
            "land_sequencing_no_alternative",
            0.0,
        );
    }

    #[test]
    fn non_bounce_land_na() {
        let mut state = GameState::new_two_player(42);
        let karoo = bounce_land(&mut state);
        let basic = plain_land(&mut state, "Forest");
        state.players[0].hand = [karoo, basic].into_iter().collect();
        assert_score(
            play_verdict(
                &state,
                basic,
                vec![play_candidate(karoo), play_candidate(basic)],
            ),
            "land_sequencing_na",
            0.0,
        );
    }

    /// A land with a non-bounce ETB (e.g. a scry/tap land) must NOT be detected
    /// as a bounce-land — guards the structural matcher against false positives.
    #[test]
    fn non_karoo_etb_land_na() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(&mut state, CardId(3), AI, "Temple".to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Proliferate,
                )),
        );
        let basic = plain_land(&mut state, "Forest");
        state.players[0].hand = [id, basic].into_iter().collect();
        assert_score(
            play_verdict(&state, id, vec![play_candidate(id), play_candidate(basic)]),
            "land_sequencing_na",
            0.0,
        );
    }

    #[test]
    fn unavailable_non_bounce_land_in_hand_is_not_an_alternative() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = engine::types::phase::Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player: AI };
        let bounce = bounce_land(&mut state);
        let restricted = plain_land(&mut state, "Restricted Forest");
        state.players[0].hand = [bounce, restricted].into_iter().collect();

        let restriction_source = create_object(
            &mut state,
            CardId(3),
            AI,
            "Conjurer's Ban".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&restriction_source)
            .expect("restriction source")
            .chosen_attributes
            .push(ChosenAttribute::CardName("Restricted Forest".to_string()));
        state.restrictions.push(GameRestriction::ProhibitActivity {
            source: restriction_source,
            affected_players: RestrictionPlayerScope::AllPlayers,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::PlayLands {
                land_filter: Some(TargetFilter::HasChosenName),
            },
        });

        let decision = build_decision_context(&state);
        assert!(decision.candidates.iter().any(|candidate| {
            matches!(candidate.action, GameAction::PlayLand { object_id, .. } if object_id == bounce)
        }));
        assert!(!decision.candidates.iter().any(|candidate| {
            matches!(candidate.action, GameAction::PlayLand { object_id, .. } if object_id == restricted)
        }));

        // The production candidate generator excludes the restricted hand land.
        assert_score(
            play_verdict(&state, bounce, decision.candidates),
            "land_sequencing_no_alternative",
            0.0,
        );
    }

    #[test]
    fn preferred_basic_land_is_engine_legal_before_self_bounce_land() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = engine::types::phase::Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player: AI };
        let bounce = bounce_land(&mut state);
        let basic = plain_land(&mut state, "Forest");
        state.players[0].hand = [bounce, basic].into_iter().collect();

        assert_score(
            play_verdict(
                &state,
                bounce,
                vec![play_candidate(bounce), play_candidate(basic)],
            ),
            "land_sequencing_play_other_first",
            -BOUNCE_DEPRIORITIZE,
        );
        engine::game::engine::apply(
            &mut state,
            AI,
            GameAction::PlayLand {
                object_id: basic,
                card_id: CardId(2),
            },
        )
        .expect("the policy-selected basic land must be engine-legal");
        assert!(state.battlefield.contains(&basic));
    }

    /// The reported "Land Selection" line: two Plains already down, the only
    /// castable spell needs green, so the Forest — not a third Plains — is the
    /// land to play.
    #[test]
    fn forest_outranks_third_plains_when_hand_needs_green() {
        let mut state = GameState::new_two_player(42);
        typed_land(&mut state, "Plains", "Plains", Zone::Battlefield);
        typed_land(&mut state, "Plains", "Plains", Zone::Battlefield);
        hand_spell(&mut state, "Rhox", vec![ManaCostShard::Green], 2);
        let plains = typed_land(&mut state, "Plains", "Plains", Zone::Hand);
        let forest = typed_land(&mut state, "Forest", "Forest", Zone::Hand);
        let candidates = vec![play_candidate(plains), play_candidate(forest)];

        assert_score(
            play_verdict(&state, forest, candidates.clone()),
            "land_sequencing_color_demand",
            AiConfig::default().policy_penalties.land_color_demand_unit,
        );
        assert_score(
            play_verdict(&state, plains, candidates),
            "land_sequencing_na",
            0.0,
        );
    }

    /// A color the battlefield already produces is not unmet, and a hand with no
    /// castable spell demands nothing — both leave the land unscored.
    #[test]
    fn no_bonus_without_unmet_demand() {
        let mut state = GameState::new_two_player(42);
        typed_land(&mut state, "Forest", "Forest", Zone::Battlefield);
        hand_spell(&mut state, "Rhox", vec![ManaCostShard::Green], 0);
        let forest = typed_land(&mut state, "Forest", "Forest", Zone::Hand);

        assert_score(
            play_verdict(&state, forest, vec![play_candidate(forest)]),
            "land_sequencing_na",
            0.0,
        );
    }

    /// CR 202.3: only hand cards within one land drop of castable define demand —
    /// a seven-drop is not a reason to fix its color now.
    #[test]
    fn only_the_castable_range_defines_demand() {
        let mut state = GameState::new_two_player(42);
        typed_land(&mut state, "Plains", "Plains", Zone::Battlefield);
        typed_land(&mut state, "Plains", "Plains", Zone::Battlefield);
        hand_spell(
            &mut state,
            "Craterhoof",
            vec![ManaCostShard::Green, ManaCostShard::Green],
            5,
        );
        let forest = typed_land(&mut state, "Forest", "Forest", Zone::Hand);

        assert_score(
            play_verdict(&state, forest, vec![play_candidate(forest)]),
            "land_sequencing_na",
            0.0,
        );
    }

    /// The reported "Gateway Plaza as the first land" line: it enters tapped AND
    /// costs {1} to keep, so beside an untapped basic it is charged for both.
    #[test]
    fn gateway_plaza_pays_for_both_riders_when_a_basic_is_playable() {
        let mut state = GameState::new_two_player(42);
        let plaza = gateway_plaza(&mut state);
        let forest = typed_land(&mut state, "Forest", "Forest", Zone::Hand);
        let candidates = vec![play_candidate(plaza), play_candidate(forest)];
        let rider = AiConfig::default()
            .policy_penalties
            .land_tempo_rider_penalty;

        assert_score(
            play_verdict(&state, plaza, candidates.clone()),
            "land_sequencing_tempo_rider",
            -2.0 * rider,
        );
        assert_score(
            play_verdict(&state, forest, candidates),
            "land_sequencing_na",
            0.0,
        );
    }

    /// A tapland that is the only land in hand is still the right play.
    #[test]
    fn tapland_alone_is_not_penalized() {
        let mut state = GameState::new_two_player(42);
        let plaza = gateway_plaza(&mut state);

        assert_score(
            play_verdict(&state, plaza, vec![play_candidate(plaza)]),
            "land_sequencing_na",
            0.0,
        );
    }

    /// A CONDITIONAL "enters tapped" (check/fast/slow lands) is card-locally
    /// unknowable, so it is charged nothing — and it cannot serve as the
    /// untapped alternative that makes another land's rider chargeable either.
    #[test]
    fn conditional_tapland_is_neither_penalized_nor_an_untapped_alternative() {
        let mut state = GameState::new_two_player(42);
        let check_land = typed_land(&mut state, "Sunpetal Grove", "Forest", Zone::Hand);
        state
            .objects
            .get_mut(&check_land)
            .unwrap()
            .replacement_definitions
            .push(etb_tapped(Some(
                ReplacementCondition::UnlessControlsSubtype {
                    subtypes: vec!["Forest".to_string(), "Plains".to_string()],
                },
            )));
        let plaza = gateway_plaza(&mut state);
        let candidates = vec![play_candidate(check_land), play_candidate(plaza)];

        assert_score(
            play_verdict(&state, check_land, candidates.clone()),
            "land_sequencing_na",
            0.0,
        );
        assert_score(
            play_verdict(&state, plaza, candidates),
            "land_sequencing_na",
            0.0,
        );
    }

    /// CR 712.12: an MDFC is played as its land back face, so the back face's
    /// riders — not the spell front's absence of them — decide the score.
    #[test]
    fn mdfc_land_back_face_riders_are_read() {
        let mut state = GameState::new_two_player(42);
        let mdfc = create_object(
            &mut state,
            CardId(6),
            AI,
            "Sea Gate".to_string(),
            Zone::Hand,
        );
        let mut back = BackFaceData {
            name: "Sea Gate, Reborn".to_string(),
            layout_kind: Some(LayoutKind::Modal),
            ..BackFaceData::default()
        };
        back.card_types.core_types.push(CoreType::Land);
        back.replacement_definitions.push(etb_tapped(None));
        let obj = state.objects.get_mut(&mdfc).unwrap();
        obj.card_types.core_types.push(CoreType::Sorcery);
        obj.back_face = Some(back);
        let forest = typed_land(&mut state, "Forest", "Forest", Zone::Hand);
        let candidates = vec![play_candidate(mdfc), play_candidate(forest)];

        assert_score(
            play_verdict(&state, mdfc, candidates.clone()),
            "land_sequencing_tempo_rider",
            -AiConfig::default()
                .policy_penalties
                .land_tempo_rider_penalty,
        );
        assert_score(
            play_verdict(&state, forest, candidates),
            "land_sequencing_na",
            0.0,
        );
    }

    /// The real Simic Growth Chamber shape: a bounce-land that also enters
    /// tapped pays both terms beside an untapped basic.
    #[test]
    fn simic_growth_chamber_shape_pays_bounce_and_tapped() {
        let mut state = GameState::new_two_player(42);
        let karoo = bounce_land(&mut state);
        state
            .objects
            .get_mut(&karoo)
            .unwrap()
            .replacement_definitions
            .push(etb_tapped(None));
        let basic = plain_land(&mut state, "Forest");

        assert_score(
            play_verdict(
                &state,
                karoo,
                vec![play_candidate(karoo), play_candidate(basic)],
            ),
            "land_sequencing_play_other_first",
            -BOUNCE_DEPRIORITIZE
                - AiConfig::default()
                    .policy_penalties
                    .land_tempo_rider_penalty,
        );
    }
}
