//! Fetch-land patience tactical policy.
//!
//! Report (Discord #ai-suggestions): the AI cracks Evolving Wilds the instant
//! it resolves, with no patience. Evolving Wilds ("{T}, Sacrifice this land:
//! Search your library for a basic land card, put it onto the battlefield
//! tapped, then shuffle.") and its class (Terramorphic Expanse, Terminal
//! Moraine, …) sacrifice themselves to fetch a land that enters **tapped**.
//!
//! Because the fetched land enters tapped (CR 305.4 — it is *put* onto the
//! battlefield, not played), cracking the fetch yields **zero mana this turn**.
//! The only payoff is fixing/thinning for a *later* turn. So there is never a
//! same-turn reason to crack early: the patient line is to hold the fetch until
//! the AI's own end step, by which point it has seen the whole turn and knows
//! which basic it actually wants. Cracking earlier gives up that information for
//! no tempo gain.
//!
//! This policy rejects cracking a tapped self-sacrifice land-fetch outside the
//! AI's own end step, and gives a small nudge to crack it *at* the end step (the
//! source produces no mana on its own, so leaving it uncracked strands a dead
//! land).
//!
//! The untapped class (Misty Rainforest, Marsh Flats, Flooded Strand) is the
//! mirror image and is scored, not gated. Its replacement enters able to tap for
//! mana the same turn, while the fetch land itself has no mana ability at all —
//! so every turn it sits uncracked is a turn the AI plays a mana source short,
//! for no compensating information. Left unscored it merely ties with
//! `PassPriority`, and the softmax selector then cracks it at an arbitrary
//! moment turns later; a preference-band score on the AI's own turn is what
//! makes it act promptly instead.

use engine::game::filter::{matches_target_filter, FilterContext};
use engine::types::ability::{AbilityCost, AbilityDefinition, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::{EtbTapState, Zone};

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::mana_ramp::target_filter_references_land;
use crate::features::DeckFeatures;

/// Small positive nudge to crack the fetch at the AI's own end step. The source
/// produces no mana itself, so converting it to a (tapped) basic is strictly
/// card-neutral and readies a real mana source for next turn — leaving it
/// uncracked strands a dead land. Nudge-band: enough to beat `PassPriority`,
/// never enough to override a genuinely better line.
const END_STEP_CRACK_NUDGE: f64 = 0.3;

/// Preference-band score for cracking an *untapped* fetch on the AI's own turn.
/// The fetch land produces no mana itself and its replacement enters untapped,
/// so cracking converts a dead permanent into a live mana source for this turn —
/// worth about a land drop's tempo, and deliberately above `NUDGE_MAX` so it
/// decides the softmax rather than merely tilting it.
const OWN_TURN_CRACK_PREFERENCE: f64 = 1.0;

/// Which class of self-sacrifice land fetch an ability belongs to.
///
/// Both classes pay the source land itself to search out a replacement; they
/// differ only in whether that replacement can produce mana the same turn, and
/// that single difference inverts the correct timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LandFetchKind {
    /// The replacement enters tapped (Evolving Wilds, Terramorphic Expanse).
    /// Cracking yields no mana this turn whenever it happens, so the only thing
    /// early cracking spends is information — hold until the own end step.
    EntersTapped,
    /// The replacement enters untapped (Misty Rainforest, Marsh Flats). Cracking
    /// yields a usable mana source this turn, so holding is a pure loss.
    EntersUntapped,
}

pub struct FetchLandPatiencePolicy;

impl TacticalPolicy for FetchLandPatiencePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::FetchLandPatience
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Applies to every deck — Evolving Wilds shows up anywhere. The verdict's
        // classifier self-gates to the tapped fetch-sacrifice land class.
        // activation-constant: classifier-gated fetch-land patience policy.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || PolicyVerdict::neutral(PolicyReason::new("fetch_patience_na"));

        let GameAction::ActivateAbility {
            source_id,
            ability_index,
        } = &ctx.candidate.action
        else {
            return na();
        };

        let Some(source) = ctx.state.objects.get(source_id) else {
            return na();
        };
        // A fetchland is a land replacing itself. Sakura-Tribe Elder and
        // Wayfarer's Bauble carry the identical sacrifice-search-put chain but
        // are a creature and an artifact: sacrificing them spends a body or a
        // rock, not a land drop already made, so this policy's timing argument
        // does not apply and its end-step `Reject` would veto their real lines
        // (the Elder's block-then-sacrifice among them).
        if !source.card_types.core_types.contains(&CoreType::Land) {
            return na();
        }
        let Some(def) = source.abilities.get(*ability_index) else {
            return na();
        };

        // Both classes share the self-sacrifice cost; the tap state the
        // replacement arrives in is what separates them.
        if !cost_sacrifices_self(def.cost.as_ref()) {
            return na();
        }
        let Some(fetch) = classify_land_fetch(def) else {
            return na();
        };

        let own_turn = ctx.state.active_player == ctx.ai_player;

        match fetch.kind {
            LandFetchKind::EntersTapped => {
                // CR 513 / CR 514: the AI's own end step is the patient window.
                // The land enters tapped, so cracking now vs. at end step is
                // identical for this turn's mana — but end step preserves
                // information until the last moment.
                if own_turn && ctx.state.phase == Phase::End {
                    return PolicyVerdict::nudge(
                        END_STEP_CRACK_NUDGE,
                        PolicyReason::new("fetch_patience_end_step"),
                    );
                }
                // Any earlier window: hold the fetch. Hard-`Reject` rather than
                // penalise — the end-step nudge above guarantees it still cracks
                // eventually, so the land is never stranded.
                PolicyVerdict::reject(PolicyReason::new("fetch_patience_hold"))
            }
            LandFetchKind::EntersUntapped => {
                // On an opponent's turn holding is defensible (the fetch can still
                // be cracked later at no cost), so stay neutral rather than
                // pushing either way. The own-turn score below means a fetch
                // rarely survives to see that window at all.
                //
                // The empty stack is part of the same restraint. This policy's
                // whole argument is about the turn's mana, which it can make
                // only in a clean window; with something on the stack the
                // question is whether to respond to *that*, which this policy
                // has no information about. Cracking as a response stays legal
                // and reachable, it just is not scored up from here.
                if !own_turn || !ctx.state.stack.is_empty() {
                    return na();
                }
                // Paying the life to search a library that holds no match would
                // sacrifice the land for nothing.
                if !library_holds_search_match(ctx, *source_id, fetch.search_filter) {
                    return PolicyVerdict::neutral(PolicyReason::new("fetch_patience_no_match"));
                }
                PolicyVerdict::preference(
                    OWN_TURN_CRACK_PREFERENCE,
                    PolicyReason::new("fetch_untapped_own_turn"),
                )
            }
        }
    }
}

/// True if `cost` sacrifices the source permanent itself (CR 701.21) — the
/// signature of a one-shot fetch land. Recurses into composite costs so it
/// matches "{T}, Sacrifice ~" and "{1}, {T}, Sacrifice ~" alike.
fn cost_sacrifices_self(cost: Option<&AbilityCost>) -> bool {
    fn check(c: &AbilityCost) -> bool {
        match c {
            AbilityCost::Sacrifice(sac) => matches!(sac.target, TargetFilter::SelfRef),
            AbilityCost::Composite { costs } => costs.iter().any(check),
            _ => false,
        }
    }
    cost.is_some_and(check)
}

/// A self-replacement land fetch: the ability searches its own controller's
/// library for a land and puts what it found straight onto the battlefield.
struct LandFetch<'a> {
    kind: LandFetchKind,
    /// What the search accepts, so the caller can check the library still
    /// holds something worth paying for.
    search_filter: &'a TargetFilter,
}

/// Classify `ability` as a self-replacement land fetch, or `None` if it is not
/// one.
///
/// The shape is read from the ability's own chain rather than from a flattened
/// effect list, because land *destruction* carries the same effects in a
/// different arrangement. Ghost Quarter ("Destroy target land. Its controller
/// may search their library for a basic land card, put it onto the
/// battlefield…") and Field of Ruin ("…Each player searches their library…")
/// both contain a land search and a put onto the battlefield, but the search
/// is a continuation of the destruction and, for Ghost Quarter, runs on the
/// destroyed land's controller's library. Three structural facts separate a
/// real fetch from them:
///
/// - the search is the ability's root effect, not a rider on something else;
/// - it searches the activator's own library (`target_player` is
///   unset or the controller), so the replacement is the activator's land;
/// - its immediate continuation moves the found card from the library onto the
///   battlefield — the put that makes it a *replacement* rather than a tutor.
///
/// Only `EtbTapState::Tapped` means tapped: a printed fetchland parses to
/// `EtbTapState::Unspecified` (its Oracle text says nothing about tapping),
/// which is the same "enters untapped" outcome as an explicit `Untapped`.
fn classify_land_fetch(ability: &AbilityDefinition) -> Option<LandFetch<'_>> {
    let Effect::SearchLibrary {
        filter,
        target_player: None | Some(TargetFilter::Controller),
        ..
    } = &*ability.effect
    else {
        return None;
    };
    if !target_filter_references_land(filter) {
        return None;
    }
    let delivery = ability.sub_ability.as_deref()?;
    let Effect::ChangeZone {
        origin: Some(Zone::Library),
        destination: Zone::Battlefield,
        enter_tapped,
        ..
    } = &*delivery.effect
    else {
        return None;
    };
    let kind = match enter_tapped {
        EtbTapState::Tapped => LandFetchKind::EntersTapped,
        EtbTapState::Unspecified | EtbTapState::Untapped => LandFetchKind::EntersUntapped,
    };
    Some(LandFetch {
        kind,
        search_filter: filter,
    })
}

/// True if the AI's library still holds a card `search_filter` accepts. A fetch
/// whose colours have run dry pays its life and sacrifices its land for
/// nothing, so it must not be scored up. The AI's library is the right one to
/// scan because `classify_land_fetch` only admits searches of the activator's
/// own library, and this policy only scores the AI's own activations.
fn library_holds_search_match(
    ctx: &PolicyContext<'_>,
    source_id: ObjectId,
    search_filter: &TargetFilter,
) -> bool {
    let filter_ctx = FilterContext::from_source(ctx.state, source_id);
    ctx.state.players[ctx.ai_player.0 as usize]
        .library
        .iter()
        .any(|&card_id| matches_target_filter(ctx.state, card_id, search_filter, &filter_ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, QuantityExpr, SacrificeCost, TypedFilter,
    };
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::CardId;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

    /// Build an Evolving Wilds-shaped activated ability: `{T}, Sacrifice ~`
    /// searching for a land that enters the battlefield with the given tap state.
    fn fetch_land_ability(enter_tapped: EtbTapState) -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
        );
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::land()),
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        )));
        ability
    }

    fn ai_land_with_ability(state: &mut GameState, ability: AbilityDefinition) -> ObjectId {
        ai_permanent_with_ability(state, CoreType::Land, ability)
    }

    fn ai_permanent_with_ability(
        state: &mut GameState,
        core_type: CoreType,
        ability: AbilityDefinition,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Fetch Source".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(core_type);
        Arc::make_mut(&mut obj.abilities).push(ability);
        id
    }

    fn verdict_for(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        FetchLandPatiencePolicy.verdict(&ctx)
    }

    /// Cracking Evolving Wilds during the AI's own precombat main (the reported
    /// "instant pop") is rejected — the fetched land enters tapped, so there is
    /// no same-turn payoff to justify giving up information now.
    #[test]
    fn evolving_wilds_main_phase_rejected() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        let id = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Tapped));
        match verdict_for(&state, id) {
            PolicyVerdict::Reject { reason } => assert_eq!(reason.kind, "fetch_patience_hold"),
            PolicyVerdict::Score { .. } => panic!("expected reject during main phase"),
        }
    }

    /// At the AI's own end step the fetch is nudged to crack — leaving the
    /// no-mana source uncracked strands a dead land.
    #[test]
    fn evolving_wilds_own_end_step_nudged() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::End;
        let id = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Tapped));
        match verdict_for(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "fetch_patience_end_step");
                assert!(delta > 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("expected nudge at end step"),
        }
    }

    /// Put a land into the AI's library so an untapped fetch has something to
    /// find. Without a match the policy deliberately declines to score.
    fn ai_library_land(state: &mut GameState) -> ObjectId {
        let id = create_object(state, CardId(2), AI, "Forest".to_string(), Zone::Library);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        id
    }

    /// An untapped true fetchland on the AI's own turn is scored up, not merely
    /// left alone: the fetch land taps for nothing itself, so holding it plays
    /// the AI a mana source short. The score must clear the nudge band, since a
    /// nudge only tilts the softmax instead of deciding it.
    #[test]
    fn untapped_fetchland_own_turn_preferred() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        ai_library_land(&mut state);
        let id = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Untapped));
        match verdict_for(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "fetch_untapped_own_turn");
                assert!(
                    delta > super::super::registry::NUDGE_MAX,
                    "a nudge-band delta ({delta}) would leave the crack up to softmax chance"
                );
            }
            PolicyVerdict::Reject { .. } => panic!("untapped fetch must not be gated"),
        }
    }

    /// A printed fetchland's Oracle text says nothing about tapping, so the
    /// parser emits `EtbTapState::Unspecified` — the same enters-untapped
    /// outcome as an explicit `Untapped`, and it must classify identically.
    /// This is the shape the real card database actually produces.
    #[test]
    fn unspecified_tap_state_is_the_untapped_class() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        ai_library_land(&mut state);
        let id = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Unspecified));
        match verdict_for(&state, id) {
            PolicyVerdict::Score { reason, .. } => {
                assert_eq!(reason.kind, "fetch_untapped_own_turn");
            }
            PolicyVerdict::Reject { .. } => panic!("an unspecified tap state enters untapped"),
        }
    }

    /// With no matching card left in the library, cracking pays the cost and
    /// sacrifices the land for nothing — so it is not scored up.
    #[test]
    fn untapped_fetchland_without_library_match_is_not_preferred() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        let id = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Untapped));
        match verdict_for(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "fetch_patience_no_match");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("an empty library must not be gated"),
        }
    }

    /// Build a Ghost Quarter-shaped ability: `{T}, Sacrifice ~: Destroy target
    /// land. Its controller may search their library for a basic land card, put
    /// it onto the battlefield, then shuffle.` The search is a continuation of
    /// the destruction and runs on the destroyed land's controller's library.
    fn ghost_quarter_ability() -> AbilityDefinition {
        let mut search = fetch_land_ability(EtbTapState::Unspecified);
        search.kind = AbilityKind::Spell;
        search.cost = None;
        let Effect::SearchLibrary { target_player, .. } = &mut *search.effect else {
            unreachable!("fetch_land_ability roots on SearchLibrary");
        };
        *target_player = Some(TargetFilter::ParentTargetController);

        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::land()),
                cant_regenerate: false,
            },
        );
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });
        ability.sub_ability = Some(Box::new(search));
        ability
    }

    fn assert_not_a_fetch(state: &GameState, id: ObjectId) {
        match verdict_for(state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "fetch_patience_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("a non-fetch must not be gated"),
        }
    }

    /// Ghost Quarter sacrifices itself and its chain holds both a land search
    /// and a put onto the battlefield, but it is land destruction: the search
    /// rides on the `Destroy` and belongs to the destroyed land's controller.
    /// With a matching land in the AI's own library it must still not be
    /// scored as a fetch — that would push the AI to fire it at nothing.
    #[test]
    fn ghost_quarter_is_not_a_fetch() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        ai_library_land(&mut state);
        let id = ai_land_with_ability(&mut state, ghost_quarter_ability());
        assert_not_a_fetch(&state, id);
    }

    /// A self-sacrificing search of another player's library is not a
    /// replacement for the sacrificed land, even with the search at the root:
    /// the card found is that player's, not the activator's.
    #[test]
    fn search_of_another_players_library_is_not_a_fetch() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        ai_library_land(&mut state);
        let mut ability = fetch_land_ability(EtbTapState::Unspecified);
        let Effect::SearchLibrary { target_player, .. } = &mut *ability.effect else {
            unreachable!("fetch_land_ability roots on SearchLibrary");
        };
        *target_player = Some(TargetFilter::ParentTargetController);
        let id = ai_land_with_ability(&mut state, ability);
        assert_not_a_fetch(&state, id);
    }

    /// A search whose continuation is not the library-to-battlefield put (a
    /// tutor to hand) is not a land replacement either.
    #[test]
    fn search_without_a_put_onto_the_battlefield_is_not_a_fetch() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        ai_library_land(&mut state);
        let mut ability = fetch_land_ability(EtbTapState::Unspecified);
        let delivery = ability.sub_ability.as_deref_mut().unwrap();
        let Effect::ChangeZone { destination, .. } = &mut *delivery.effect else {
            unreachable!("fetch_land_ability continues with ChangeZone");
        };
        *destination = Zone::Hand;
        let id = ai_land_with_ability(&mut state, ability);
        assert_not_a_fetch(&state, id);
    }

    /// Sakura-Tribe Elder ("Sacrifice this creature: Search your library for a
    /// basic land card, put that card onto the battlefield tapped") and
    /// Wayfarer's Bauble carry the Evolving Wilds chain on a nonland source.
    /// Without the land gate the tapped arm would `Reject` them in main phase
    /// — the same fixture on a land source is rejected, so only the source
    /// type separates the two outcomes.
    #[test]
    fn nonland_self_sacrifice_tutors_are_not_fetchlands() {
        for core_type in [CoreType::Creature, CoreType::Artifact] {
            let mut state = GameState::new_two_player(42);
            state.active_player = AI;
            state.phase = Phase::PreCombatMain;
            ai_library_land(&mut state);
            let id = ai_permanent_with_ability(
                &mut state,
                core_type,
                fetch_land_ability(EtbTapState::Tapped),
            );
            assert_not_a_fetch(&state, id);
        }
    }

    /// With something on the stack the live question is whether to respond to
    /// it, which this policy cannot judge — so it declines to score rather than
    /// pushing a second fetch onto the stack ahead of the pending one.
    #[test]
    fn untapped_fetchland_with_a_non_empty_stack_is_neutral() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        ai_library_land(&mut state);
        let id = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Untapped));
        let pending = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Untapped));
        state
            .stack
            .push_back(engine::types::game_state::StackEntry {
                id: pending,
                source_id: pending,
                controller: AI,
                kind: engine::types::game_state::StackEntryKind::Spell {
                    card_id: CardId(1),
                    ability: None,
                    casting_variant: Default::default(),
                    actual_mana_spent: 0,
                },
            });
        match verdict_for(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "fetch_patience_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("a stack response must not be gated"),
        }
    }

    /// On an opponent's turn the untapped class is left neutral: holding costs
    /// nothing there, and the own-turn score means a fetch rarely survives to
    /// see that window at all.
    #[test]
    fn untapped_fetchland_on_opponent_turn_is_neutral() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1);
        state.phase = Phase::Upkeep;
        ai_library_land(&mut state);
        let id = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Untapped));
        match verdict_for(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "fetch_patience_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => {
                panic!("opponent-turn untapped fetch must not be gated")
            }
        }
    }
}
