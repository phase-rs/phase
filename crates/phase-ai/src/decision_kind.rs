//! Classify a `(WaitingFor, GameAction)` pair into a coarse `DecisionKind`.
//!
//! This is the routing key for `PolicyRegistry`: each policy declares which
//! `DecisionKind`s it fires for, and the registry only invokes policies whose
//! list contains the classified kind for the current candidate. The match
//! over `WaitingFor` is exhaustive — adding a new `WaitingFor` variant forces
//! a compile error here, ensuring no decision can silently bypass policy
//! routing.

use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;

use crate::cast_facts::is_cast_family_action;
use crate::policies::registry::DecisionKind;
#[cfg(test)]
use engine::types::game_state::CastPaymentMode;

/// Classify a decision into the bucket the policy registry uses for routing.
pub fn classify(waiting_for: &WaitingFor, action: &GameAction) -> DecisionKind {
    match waiting_for {
        WaitingFor::MulliganDecision { .. } | WaitingFor::OpeningHandBottomCards { .. } => {
            DecisionKind::Mulligan
        }
        WaitingFor::ManaPayment { .. }
        | WaitingFor::ManaSourceSelection { .. }
        | WaitingFor::PhyrexianPayment { .. } => {
            DecisionKind::ManaPayment
        }
        WaitingFor::ChooseXValue { .. } => DecisionKind::ChooseX,
        WaitingFor::TargetSelection { .. }
        | WaitingFor::TriggerTargetSelection { .. }
        | WaitingFor::MultiTargetSelection { .. }
        | WaitingFor::CopyRetarget { .. }
        | WaitingFor::RetargetChoice { .. }
        | WaitingFor::DistributeAmong { .. }
        | WaitingFor::MoveCountersDistribution { .. }
        | WaitingFor::RemoveCountersChoice { .. } => DecisionKind::SelectTarget,
        WaitingFor::DeclareAttackers { .. } => DecisionKind::DeclareAttackers,
        WaitingFor::DeclareBlockers { .. } => DecisionKind::DeclareBlockers,
        WaitingFor::UntapChoice { .. } => DecisionKind::ActivateAbility,
        // CR 502.3: the bounded untap-subset selection under a MaxUntapPerType
        // cap is a mechanical untap-step choice; route it to the same catch-all
        // bucket as the optional-decline UntapChoice.
        WaitingFor::ChooseUntapSubset { .. } => DecisionKind::ActivateAbility,
        // CR 508.1g: exert-as-attack is part of the attack declaration; route it
        // to the attack policy population.
        WaitingFor::ExertChoice { .. } | WaitingFor::EnlistChoice { .. } => {
            DecisionKind::DeclareAttackers
        }
        // CR 508.1d + CR 509.1c: Combat tax — route by context so the attack-tax
        // policy sees `DeclareAttackers` candidates and the block-tax policy sees
        // `DeclareBlockers` candidates.
        WaitingFor::CombatTaxPayment { context, .. } => match context {
            engine::types::game_state::CombatTaxContext::Attacking => {
                DecisionKind::DeclareAttackers
            }
            engine::types::game_state::CombatTaxContext::Blocking => DecisionKind::DeclareBlockers,
        },
        // Priority — dispatch on the action being scored.
        WaitingFor::Priority { .. } => match action {
            GameAction::PlayLand { .. } => DecisionKind::PlayLand,
            GameAction::ActivateAbility { .. } => DecisionKind::ActivateAbility,
            GameAction::TapLandForMana { .. }
            | GameAction::ActivateManaSource { .. }
            | GameAction::UntapLandForMana { .. } => {
                DecisionKind::ActivateManaAbility
            }
            // CR 601.2 + CR 118.9: the plain announcement AND every dedicated
            // alternative/free-cast variant put a spell on the stack, so they
            // all belong to the cast policy population. Leaving the alternative
            // variants in the catch-all below made every cast policy blind to
            // them (they scored as activations).
            action if is_cast_family_action(action) => DecisionKind::CastSpell,
            // Default: any other priority-time action (PassPriority, special
            // actions, etc.) routes to ActivateAbility — these are activation-
            // adjacent decisions that the same policy population evaluates.
            _ => DecisionKind::ActivateAbility,
        },
        // CR 702.94a + CR 603.11: Miracle reveal, and CR 715.3a + CR 702.35a +
        // CR 702.85a + CR 701.57a: the cast offers. Each of these prompts mixes
        // a real cast candidate with a decline candidate, so route on the ACTION
        // rather than the prompt: `CastSpellAsMiracle` / `CastSpellAsMadness`
        // are casts and reach the cast policies, while the shared decline
        // (`DecideOptionalEffect { accept: false }`) and the non-family offer
        // responses (Adventure's `ChooseAdventureFace`, Cascade/Discover/Ripple's
        // `*Choice`, Paradigm's `CastParadigmCopy`, the free-cast window's
        // `FreeCastWindowChoice`) stay in the ability catch-all. (The Pass
        // handicap on the decline is keyed on `TacticalClass::Pass` in the
        // planner, independent of this routing.)
        WaitingFor::MiracleReveal { .. } | WaitingFor::CastOffer { .. } => {
            if is_cast_family_action(action) {
                DecisionKind::CastSpell
            } else {
                DecisionKind::ActivateAbility
            }
        }
        // All other WaitingFor states are mechanical/forced choices that no
        // tactical policy currently routes on. Map them to ActivateAbility as
        // the catch-all bucket so policies that explicitly opt in still run.
        WaitingFor::ResolveAllConsent { .. }
        | WaitingFor::ResolveAllReady { .. }
        | WaitingFor::ReplacementChoice { .. }
        | WaitingFor::MeldPairChoice { .. }
        | WaitingFor::MeldAttackTargetChoice { .. }
        | WaitingFor::EntryAttackTargetChoice { .. }
        | WaitingFor::OrderTriggers { .. }
        | WaitingFor::CopyTargetChoice { .. }
        | WaitingFor::ExploreChoice { .. }
        | WaitingFor::ReturnAsAuraTarget { .. }
        | WaitingFor::EquipTarget { .. }
        | WaitingFor::CrewVehicle { .. }
        | WaitingFor::StationTarget { .. }
        | WaitingFor::SaddleMount { .. }
        | WaitingFor::ScryChoice { .. }
        | WaitingFor::ArrangePlanarDeckTopChoice { .. }
        // CR 119.7 + CR 119.8: redistribute life totals is a forced mid-resolution
        // selection; route to the ability catch-all bucket.
        | WaitingFor::RedistributeLifeTotals { .. }
        | WaitingFor::DigChoice { .. }
        | WaitingFor::SurveilChoice { .. }
        | WaitingFor::RevealChoice { .. }
        | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
        | WaitingFor::DamageSourceChoice { .. }
        | WaitingFor::SearchChoice { .. }
        | WaitingFor::SearchPartitionChoice { .. }
        | WaitingFor::OutsideGameChoice { .. }
        | WaitingFor::ChooseFromZoneChoice { .. }
        | WaitingFor::BeholdChoice { .. }
        | WaitingFor::ConniveDiscard { .. }
        | WaitingFor::DiscardChoice { .. }
        | WaitingFor::EffectZoneChoice { .. }
        | WaitingFor::LearnChoice { .. }
        | WaitingFor::ManifestDreadChoice { .. }
        | WaitingFor::BetweenGamesSideboard { .. }
        | WaitingFor::BetweenGamesChoosePlayDraw { .. }
        | WaitingFor::NamedChoice { .. }
        | WaitingFor::OpponentGuess { .. }
        | WaitingFor::SpellbookDraft { .. }
        | WaitingFor::ModeChoice { .. }
        | WaitingFor::DiscardToHandSize { .. }
        | WaitingFor::OptionalCostChoice { .. }
        | WaitingFor::SpliceOffer { .. }
        | WaitingFor::DefilerPayment { .. }
        | WaitingFor::AbilityModeChoice { .. }
        | WaitingFor::ModalFaceChoice { .. }
        | WaitingFor::AlternativeCastChoice { .. }
        | WaitingFor::CastingVariantChoice { .. }
        | WaitingFor::ChoosePermanentTypeSlot { .. }
        | WaitingFor::ChooseRingBearer { .. }
        | WaitingFor::ChooseRoomDoor { .. }
        | WaitingFor::ChooseDungeon { .. }
        | WaitingFor::ChooseDungeonRoom { .. }
        | WaitingFor::SpecializeColor { .. }
        | WaitingFor::PayCost { .. }
        | WaitingFor::BlightChoice { .. }
        | WaitingFor::ChooseManaColor { .. }
        | WaitingFor::CollectEvidenceChoice { .. }
        | WaitingFor::HarmonizeTapChoice { .. }
        | WaitingFor::OptionalEffectChoice { .. }
        | WaitingFor::ResolutionOptionalPaymentChoice { .. }
        | WaitingFor::PairChoice { .. }
        | WaitingFor::OpponentMayChoice { .. }
        | WaitingFor::TributeChoice { .. }
        | WaitingFor::UnlessPayment { .. }
        | WaitingFor::UnlessPaymentChooseCost { .. }
        | WaitingFor::WardDiscardChoice { .. }
        | WaitingFor::WardSacrificeChoice { .. }
        | WaitingFor::UnlessBounceChoice { .. }
        | WaitingFor::RevealUntilKeptChoice { .. }
        | WaitingFor::RepeatDecision { .. }
        | WaitingFor::TopOrBottomChoice { .. }
        // CR 702.140c + CR 730.2a: mutate top/bottom merge side — a forced
        // mid-resolution choice; route to the ability catch-all.
        | WaitingFor::MutateMergeChoice { .. }
        // CR 702.99a: cipher encode-on-resolve — a mid-resolution selection;
        // route to the same ability catch-all as the mutate merge choice.
        | WaitingFor::CipherEncodeChoice { .. }
        | WaitingFor::PopulateChoice { .. }
        | WaitingFor::ClashChooseOpponent { .. }
        | WaitingFor::ChooseFromZoneOpponentChooser { .. }
        | WaitingFor::ChooseAnnouncingOpponent { .. }
        | WaitingFor::ChooseGiftRecipient { .. }
        | WaitingFor::ClashCardPlacement { .. }
        | WaitingFor::VoteChoice { .. }
        | WaitingFor::SeparatePilesChooseOpponent { .. }
        | WaitingFor::SeparatePilesPartition { .. }
        | WaitingFor::SeparatePilesChoice { .. }
        | WaitingFor::CompanionReveal { .. }
        | WaitingFor::ChooseLegend { .. }
        | WaitingFor::CommanderZoneChoice { .. }
        | WaitingFor::BattleProtectorChoice { .. }
        | WaitingFor::ProliferateChoice { .. }
        | WaitingFor::TimeTravelChoice { .. }
        // CR 702.132a: Assist offer / payment — casting-payment-adjacent choices,
        // routed to the ability catch-all bucket like the other opt-in cast steps.
        | WaitingFor::AssistChoosePlayer { .. }
        | WaitingFor::AssistPayment { .. }
        | WaitingFor::ChooseObjectsSelection { .. }
        | WaitingFor::CategoryChoice { .. }
        | WaitingFor::EachPlayerCopyChosenSelection { .. }
        | WaitingFor::KeepWithinTotalPowerChoice { .. }
        | WaitingFor::KeepExactPermanentsChoice { .. }
        | WaitingFor::AssignCombatDamage { .. }
        // CR 510.1d + CR 702.22k: active player divides a banded blocker's
        // damage — a forced mid-combat choice, routed to the ability catch-all.
        | WaitingFor::AssignBlockerDamage { .. }
        // CR 107.1c + CR 107.14: "Pay any amount of X" prompts are forced
        // mid-resolution choices; route to ActivateAbility as a catch-all.
        | WaitingFor::PayAmountChoice { .. }
        | WaitingFor::GameOver { .. }
        | WaitingFor::ChooseOneOfBranch { .. }
        | WaitingFor::PayManaAbilityMana { .. }
        // CR 705.1 + CR 614.1a: Krark's Thumb keep choice is a forced
        // mid-resolution selection; route to the ability catch-all.
        | WaitingFor::CoinFlipKeepChoice { .. }
        | WaitingFor::ActivationCostOneOfChoice { .. }
        // CR 601.2b: choosing an additional cost's mode (e.g. behold a chosen
        // creature type) is a casting-cost-phase step; route to the ability bucket.
        | WaitingFor::CostTypeChoice { .. }
        // CR 732.2a/b/c: shortcut protocols are forced mid-flow decisions;
        // route both the legacy loop and finite pre-cast families to the same
        // ability catch-all like the other opt-in choices.
        | WaitingFor::LoopShortcut { .. }
        | WaitingFor::RespondToShortcut { .. }
        | WaitingFor::PrecastCopyShortcutOffer { .. }
        | WaitingFor::RespondToPrecastCopyShortcut { .. }
        | WaitingFor::EntryControllerChoice { .. } => DecisionKind::ActivateAbility,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;

    /// Confirms `classify` covers every routable `WaitingFor` variant. The
    /// compile-time exhaustiveness of the match in `classify` is the real
    /// guarantee — this test additionally verifies behavior on the variants
    /// that route to a non-default `DecisionKind`.
    #[test]
    fn classify_is_exhaustive() {
        let dummy_action = GameAction::CastSpell {
            object_id: ObjectId(0),
            card_id: CardId(0),
            targets: Vec::new(),

            payment_mode: CastPaymentMode::Auto,
        };

        // Mulligan routing.
        assert_eq!(
            classify(
                &WaitingFor::MulliganDecision {
                    pending: vec![engine::types::game_state::MulliganDecisionEntry {
                        player: PlayerId(0),
                        mulligan_count: 0,
                        phase: engine::types::game_state::MulliganDecisionPhase::Declare,
                    }],
                    free_first_mulligan: false,
                },
                &dummy_action
            ),
            DecisionKind::Mulligan
        );
        // Mana payment routing.
        assert_eq!(
            classify(
                &WaitingFor::ManaPayment {
                    player: PlayerId(0),
                    convoke_mode: None,
                },
                &dummy_action
            ),
            DecisionKind::ManaPayment
        );
        // Finite pre-cast shortcut waits use the same tactical-policy bucket
        // as the legacy shortcut protocol.
        assert_eq!(
            classify(
                &WaitingFor::PrecastCopyShortcutOffer {
                    proposer: PlayerId(0),
                    epoch: 1,
                    route_count: 1,
                },
                &dummy_action
            ),
            DecisionKind::ActivateAbility
        );
        assert_eq!(
            classify(
                &WaitingFor::RespondToPrecastCopyShortcut {
                    player: PlayerId(1),
                    epoch: 1,
                    breakpoint_ids: vec![2],
                    remaining_players: Vec::new(),
                },
                &dummy_action
            ),
            DecisionKind::ActivateAbility
        );
        // Combat routing.
        assert_eq!(
            classify(
                &WaitingFor::DeclareAttackers {
                    player: PlayerId(0),
                    valid_attacker_ids: vec![],
                    valid_attack_targets: vec![],
                    valid_attack_targets_by_attacker: None,
                    attacker_constraints: Default::default(),
                },
                &dummy_action
            ),
            DecisionKind::DeclareAttackers
        );
        assert_eq!(
            classify(
                &WaitingFor::DeclareBlockers {
                    player: PlayerId(0),
                    valid_blocker_ids: vec![],
                    valid_block_targets: std::collections::HashMap::new(),
                    block_requirements: std::collections::HashMap::new(),
                    blocker_constraints: Default::default(),
                },
                &dummy_action
            ),
            DecisionKind::DeclareBlockers
        );

        // Priority dispatches on the action.
        let priority = WaitingFor::Priority {
            player: PlayerId(0),
        };
        assert_eq!(
            classify(
                &priority,
                &GameAction::PlayLand {
                    object_id: ObjectId(0),
                    card_id: CardId(0),
                },
            ),
            DecisionKind::PlayLand
        );
        assert_eq!(classify(&priority, &dummy_action), DecisionKind::CastSpell);
        assert_eq!(
            classify(
                &priority,
                &GameAction::ActivateAbility {
                    source_id: ObjectId(0),
                    ability_index: 0
                }
            ),
            DecisionKind::ActivateAbility
        );
        assert_eq!(
            classify(
                &priority,
                &GameAction::TapLandForMana {
                    selection: engine::types::mana::ManaSourceSelection {
                        source: engine::types::identifiers::ObjectIncarnationRef {
                            object_id: ObjectId(0),
                            incarnation: 0,
                        },
                        ability_index: None,
                        mana_type: engine::types::mana::ManaType::Green,
                        output: engine::types::mana::ManaSourceOutput::Concrete(
                            engine::types::mana::ManaType::Green,
                        ),
                        atomic_combination: None,
                        restrictions: Vec::new(),
                        penalty: engine::types::mana::ManaSourcePenalty::None,
                        taps_for_mana: Vec::new(),
                    },
                }
            ),
            DecisionKind::ActivateManaAbility
        );
    }

    /// CR 601.2 + CR 118.9: every cast-family action reaches the cast policy
    /// population, from Priority and from the two opt-in cast prompts alike;
    /// the shared decline stays in the ability catch-all so the Pass handicap
    /// keeps applying to it.
    #[test]
    fn cast_family_actions_route_to_cast_spell() {
        let object_id = ObjectId(3);
        let card_id = CardId(3);
        let family: Vec<GameAction> = vec![
            GameAction::CastSpell {
                object_id,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            GameAction::CastSpellForFree {
                object_id,
                card_id,
                source_id: ObjectId(9),
                payment_mode: CastPaymentMode::Auto,
            },
            GameAction::CastSpellAsMiracle {
                object_id,
                card_id,
                payment_mode: CastPaymentMode::Auto,
            },
            GameAction::CastSpellAsMadness {
                object_id,
                card_id,
                payment_mode: CastPaymentMode::Auto,
            },
            GameAction::CastSpellAsSneak {
                hand_object: object_id,
                card_id,
                creature_to_return: ObjectId(9),
                payment_mode: CastPaymentMode::Auto,
            },
            GameAction::CastSpellAsWebSlinging {
                hand_object: object_id,
                card_id,
                creature_to_return: ObjectId(9),
                payment_mode: CastPaymentMode::Auto,
            },
        ];

        let priority = WaitingFor::Priority {
            player: PlayerId(0),
        };
        for action in &family {
            assert_eq!(
                classify(&priority, action),
                DecisionKind::CastSpell,
                "{action:?} under Priority"
            );
        }

        let miracle_reveal = WaitingFor::MiracleReveal {
            player: PlayerId(0),
            object_id,
            cost: engine::types::mana::ManaCost::generic(1),
        };
        assert_eq!(
            classify(
                &miracle_reveal,
                &GameAction::CastSpellAsMiracle {
                    object_id,
                    card_id,
                    payment_mode: CastPaymentMode::Auto,
                }
            ),
            DecisionKind::CastSpell
        );
        assert_eq!(
            classify(
                &miracle_reveal,
                &GameAction::DecideOptionalEffect { accept: false }
            ),
            DecisionKind::ActivateAbility
        );

        let madness_offer = WaitingFor::CastOffer {
            player: PlayerId(0),
            kind: engine::types::game_state::CastOfferKind::Madness {
                object_id,
                cost: engine::types::mana::ManaCost::generic(1),
            },
        };
        assert_eq!(
            classify(
                &madness_offer,
                &GameAction::CastSpellAsMadness {
                    object_id,
                    card_id,
                    payment_mode: CastPaymentMode::Auto,
                }
            ),
            DecisionKind::CastSpell
        );
        assert_eq!(
            classify(
                &madness_offer,
                &GameAction::DecideOptionalEffect { accept: false }
            ),
            DecisionKind::ActivateAbility
        );

        // A non-family offer response keeps its catch-all routing.
        let adventure_offer = WaitingFor::CastOffer {
            player: PlayerId(0),
            kind: engine::types::game_state::CastOfferKind::Adventure {
                object_id,
                card_id,
                payment_mode: CastPaymentMode::Auto,
            },
        };
        assert_eq!(
            classify(
                &adventure_offer,
                &GameAction::ChooseAdventureFace { creature: true }
            ),
            DecisionKind::ActivateAbility
        );
    }
}
