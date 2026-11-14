//! Pile-partition balancing (Fact or Fiction / Make an Example class).
//!
//! Report (Discord thread 1541557487948402754, "Fact or Fiction"; generalised
//! in-thread to "any card that requires AI to split into piles"): the AI splits
//! five revealed cards 5-0. Nothing was wrong in the engine — the partition
//! candidates were legal, no policy scored `GameAction::SubmitPilePartition`,
//! so every candidate carried the flat prior and `softmax_select_index` sampled
//! among them uniformly. With three candidates and two of them lopsided, a
//! lopsided split came up two times in three.
//!
//! CR 700.3 + CR 700.3a: the SUBJECT partitions its eligible objects into two
//! piles and the CHOOSER then picks one pile per subject. The two piles have a
//! fixed total, so the chooser's best pick is the heavier pile and the
//! subject's worst case is the lighter one — minimising the value gap between
//! the piles maximises the subject's worst case. That is why one scalar,
//! `-|value(A) - value(B)|`, is the whole policy.
//!
//! **The adversarial premise, stated because it is load-bearing.** The
//! minimise-the-gap rule is correct only because the subject and the chooser
//! are always adversaries here, and in this engine they are so by construction:
//! `separate_piles.rs::resolve_chooser` resolves the chooser only to
//! `Controller`, and `oracle_separate_piles.rs` only ever parses the
//! partitioner as an opponent of that controller. Under that constraint the
//! rule holds for both shapes of the class — chooser-keeps (Fact or Fiction,
//! Sphinx of Uthuun, Unesh) and chooser-punishes (Make an Example, Boneyard
//! Parley), since a fixed total makes "the chooser takes the best pile" and
//! "the chooser leaves the worst pile" the same optimisation. If a
//! "separate them into two piles, then choose one" card ever lands — the
//! subject choosing from its own piles — that card wants the gap MAXIMISED, and
//! this policy must gain a chooser-vs-subject test before it is let near it.
//!
//! Pricing follows the pile's origin, because "what is this object worth to
//! me?" is a different question on the battlefield than in the library.
//! `PileSource::Battlefield` (Make an Example: the chosen pile is sacrificed)
//! is a give-up decision, so it uses the codebase's single give-up authority,
//! `strategy_helpers::permanent_board_value`. Library-top and exiled piles are
//! cards to be drawn or returned, priced by `card_value::intrinsic_value` — the
//! same authority `search.rs` already uses for library-top cards in scry and
//! dig choices.
//!
//! Range note: `permanent_board_value` is unbounded on creature branches, so a
//! wide battlefield pool can produce a gap past `CRITICAL_MAX` and saturate.
//! Saturation only flattens the *top* of the distribution — the balanced
//! candidate is at the bottom of the gap range, where the scale is exact — so
//! the ordering this policy exists to fix is unaffected.
//!
//! Perf: `O(|eligible|)` value lookups per candidate over a pool that is a
//! handful of objects, at most four candidates, once per prompt. Every other
//! `ActivateAbility` candidate exits on two `matches!` before any lookup. No
//! board scan, no `find_legal_targets`, no state clone.

use engine::types::ability::PileSource;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::permanent_board_value;
use crate::card_value::intrinsic_value;
use crate::features::DeckFeatures;

pub struct PilePartitionPolicy;

impl TacticalPolicy for PilePartitionPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::PilePartition
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        // `decision_kind::classify` routes every `SeparatePilesPartition`
        // prompt into the `ActivateAbility` catch-all bucket.
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Any deck can be made to separate piles — by its own spell or by an
        // opponent's — and nothing about the deck profile changes what a
        // balanced split is worth. `verdict` exits on the prompt shape and then
        // the action shape before reading a single object.
        // activation-constant: pile-partition prompt, deck-independent.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || PolicyVerdict::neutral(PolicyReason::new("pile_partition_na"));

        let WaitingFor::SeparatePilesPartition {
            player,
            eligible,
            pile_source,
            ..
        } = &ctx.decision.waiting_for
        else {
            return na();
        };
        let GameAction::SubmitPilePartition { pile_a } = &ctx.candidate.action else {
            return na();
        };
        // Only the subject's own partition is ours to score. Another subject's
        // prompt is answered by that seat.
        if *player != ctx.ai_player {
            return na();
        }

        let value = |id: ObjectId| match pile_source {
            PileSource::Battlefield => {
                permanent_board_value(ctx.state, id, &ctx.config.policy_penalties)
            }
            PileSource::RevealedFromLibraryTop { .. } | PileSource::ExiledThisWay => {
                intrinsic_value(ctx.state, id)
            }
        };

        // CR 700.3a: pile B is `eligible \ pile_a`, so `value(B) = total - a`
        // and the gap is `|a - (total - a)| = |2a - total|`. No set difference
        // is needed, and an object missing from `state.objects` prices at 0.0
        // in both authorities rather than panicking.
        let total: f64 = eligible.iter().map(|&id| value(id)).sum();
        let in_pile_a: f64 = pile_a.iter().map(|&id| value(id)).sum();
        let gap = (2.0 * in_pile_a - total).abs();

        PolicyVerdict::score(
            -gap,
            PolicyReason::new("pile_partition_gap").with_fact("gap_x100", (gap * 100.0) as i64),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect};
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;

    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::policies::registry::PolicyRegistry;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    /// A revealed library-top card whose only value signal is its mana cost —
    /// `intrinsic_value` prices it at `mana_value * 0.5`.
    fn revealed_card(state: &mut GameState, index: u64, generic: u32) -> ObjectId {
        let id = create_object(
            state,
            CardId(600 + index),
            AI,
            format!("Revealed {index}"),
            Zone::Library,
        );
        state.objects.get_mut(&id).unwrap().mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic,
        };
        id
    }

    /// A 2/2 on the battlefield, optionally flying. Both bodies price alike
    /// under `intrinsic_value` (same P/T, same cost) and differently under
    /// `permanent_board_value`, which is what makes the source dispatch
    /// observable.
    fn battlefield_two_two(state: &mut GameState, index: u64, flying: bool) -> ObjectId {
        let id = create_object(
            state,
            CardId(700 + index),
            AI,
            format!("Bear {index}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: 2,
        };
        if flying {
            obj.keywords.push(Keyword::Flying);
        }
        id
    }

    fn partition_prompt(
        subject: PlayerId,
        eligible: &[ObjectId],
        pile_source: PileSource,
    ) -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::SeparatePilesPartition {
                player: subject,
                eligible: eligible.iter().copied().collect(),
                remaining_subjects: engine::im::Vector::new(),
                completed: engine::im::Vector::new(),
                chooser: OPP,
                chosen_pile_effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Proliferate,
                )),
                unchosen_pile_effect: None,
                source_id: ObjectId(1),
                pile_source,
            },
            candidates: Vec::new(),
        }
    }

    fn partition_candidate(pile_a: Vec<ObjectId>) -> CandidateAction {
        CandidateAction {
            action: GameAction::SubmitPilePartition { pile_a },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Selection),
        }
    }

    fn verdict_of(
        state: &GameState,
        decision: &AiDecisionContext,
        candidate: &CandidateAction,
    ) -> PolicyVerdict {
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision,
            candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        PilePartitionPolicy.verdict(&ctx)
    }

    fn delta_of(verdict: PolicyVerdict) -> f64 {
        match verdict {
            PolicyVerdict::Score { delta, .. } => delta,
            PolicyVerdict::Reject { reason } => panic!("unexpected reject: {}", reason.kind),
        }
    }

    fn assert_na(verdict: PolicyVerdict) {
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "pile_partition_na", "reason kind");
                assert_eq!(delta, 0.0, "delta");
            }
            PolicyVerdict::Reject { reason } => panic!("unexpected reject: {}", reason.kind),
        }
    }

    /// The reported 5-0 split. Values are 3.0 / 0.5 / 0.5 / 0.5 / 0.5 (total
    /// 5.0), so an empty pile A opens a 5.0 gap against the balanced split's
    /// 1.0 — and the balanced candidate must therefore outscore it.
    #[test]
    fn empty_pile_a_scores_below_balanced() {
        let mut state = GameState::new_two_player(42);
        let eligible: Vec<ObjectId> = [6u32, 1, 1, 1, 1]
            .into_iter()
            .enumerate()
            .map(|(index, mana_value)| revealed_card(&mut state, index as u64, mana_value))
            .collect();
        let decision = partition_prompt(
            AI,
            &eligible,
            PileSource::RevealedFromLibraryTop { count: 5 },
        );

        let empty = delta_of(verdict_of(
            &state,
            &decision,
            &partition_candidate(Vec::new()),
        ));
        let balanced = delta_of(verdict_of(
            &state,
            &decision,
            &partition_candidate(vec![eligible[0]]),
        ));

        assert_eq!(empty, -5.0, "an empty pile A hands the chooser everything");
        assert_eq!(balanced, -1.0, "3.0 against 2.0 is a 1.0 gap");
        assert!(
            empty < balanced,
            "the 5-0 split must lose to the balanced one"
        );
    }

    /// The count-balanced half split is not the value-balanced one when the
    /// values skew: with one 3.0 card among four 0.5s, `[heavy]` (gap 1.0)
    /// beats `[heavy, light]` (gap 2.0) even though the latter holds more cards.
    #[test]
    fn value_balanced_beats_count_balanced_when_values_skew() {
        let mut state = GameState::new_two_player(42);
        let eligible: Vec<ObjectId> = [6u32, 1, 1, 1, 1]
            .into_iter()
            .enumerate()
            .map(|(index, mana_value)| revealed_card(&mut state, index as u64, mana_value))
            .collect();
        let decision = partition_prompt(
            AI,
            &eligible,
            PileSource::RevealedFromLibraryTop { count: 5 },
        );

        let value_balanced = delta_of(verdict_of(
            &state,
            &decision,
            &partition_candidate(vec![eligible[0]]),
        ));
        let count_balanced = delta_of(verdict_of(
            &state,
            &decision,
            &partition_candidate(vec![eligible[0], eligible[1]]),
        ));

        assert!(
            count_balanced < value_balanced,
            "count-balanced {count_balanced} must lose to value-balanced {value_balanced}"
        );
    }

    /// A battlefield pile is a give-up decision, so it must be priced by
    /// `permanent_board_value`, not `intrinsic_value`. Two 2/2 bodies with the
    /// same mana cost are indistinguishable to `intrinsic_value` — the split
    /// would read as a perfectly balanced 0.0 gap — while the give-up authority
    /// sees the flier as the better body and opens a real gap.
    #[test]
    fn battlefield_source_prices_creatures_by_board_value() {
        let mut state = GameState::new_two_player(42);
        let flier = battlefield_two_two(&mut state, 0, true);
        let vanilla = battlefield_two_two(&mut state, 1, false);
        let decision = partition_prompt(AI, &[flier, vanilla], PileSource::Battlefield);

        let delta = delta_of(verdict_of(
            &state,
            &decision,
            &partition_candidate(vec![flier]),
        ));

        assert!(
            delta < 0.0,
            "a flier against a vanilla bear is not a balanced split; got {delta}"
        );
    }

    /// Another subject's partition prompt belongs to that seat.
    #[test]
    fn other_players_partition_is_na() {
        let mut state = GameState::new_two_player(42);
        let eligible = vec![
            revealed_card(&mut state, 0, 6),
            revealed_card(&mut state, 1, 1),
        ];
        let decision = partition_prompt(
            OPP,
            &eligible,
            PileSource::RevealedFromLibraryTop { count: 2 },
        );

        assert_na(verdict_of(
            &state,
            &decision,
            &partition_candidate(Vec::new()),
        ));
    }

    /// Every other `ActivateAbility` candidate exits before any value lookup.
    #[test]
    fn non_partition_action_is_na() {
        let mut state = GameState::new_two_player(42);
        let eligible = vec![revealed_card(&mut state, 0, 6)];
        let decision = partition_prompt(
            AI,
            &eligible,
            PileSource::RevealedFromLibraryTop { count: 1 },
        );
        let candidate = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Pass),
        };

        assert_na(verdict_of(&state, &decision, &candidate));
    }

    #[test]
    fn registry_registers_pile_partition() {
        assert!(PolicyRegistry::default().has_policy(PolicyId::PilePartition));
    }
}
