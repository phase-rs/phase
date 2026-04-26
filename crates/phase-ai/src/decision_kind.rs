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

use crate::policies::registry::DecisionKind;

/// Classify a decision into the bucket the policy registry uses for routing.
pub fn classify(waiting_for: &WaitingFor, action: &GameAction) -> DecisionKind {
    match waiting_for {
        WaitingFor::MulliganDecision { .. } | WaitingFor::MulliganBottomCards { .. } => {
            DecisionKind::Mulligan
        }
        WaitingFor::ManaPayment { .. } | WaitingFor::PhyrexianPayment { .. } => {
            DecisionKind::ManaPayment
        }
        WaitingFor::ChooseXValue { .. } => DecisionKind::ChooseX,
        WaitingFor::TargetSelection { .. }
        | WaitingFor::TriggerTargetSelection { .. }
        | WaitingFor::MultiTargetSelection { .. }
        | WaitingFor::CopyRetarget { .. }
        | WaitingFor::RetargetChoice { .. }
        | WaitingFor::DistributeAmong { .. } => DecisionKind::SelectTarget,
        WaitingFor::DeclareAttackers { .. } => DecisionKind::DeclareAttackers,
        WaitingFor::DeclareBlockers { .. } => DecisionKind::DeclareBlockers,
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
            GameAction::CastSpell { .. } => DecisionKind::CastSpell,
            GameAction::ActivateAbility { .. } => DecisionKind::ActivateAbility,
            GameAction::TapLandForMana { .. } | GameAction::UntapLandForMana { .. } => {
                DecisionKind::ActivateManaAbility
            }
            // Default: any other priority-time action (PassPriority, special
            // actions, etc.) routes to ActivateAbility — these are activation-
            // adjacent decisions that the same policy population evaluates.
            _ => DecisionKind::ActivateAbility,
        },
        // All other WaitingFor states are mechanical/forced choices that no
        // tactical policy currently routes on. Map them to ActivateAbility as
        // the catch-all bucket so policies that explicitly opt in still run.
        WaitingFor::ReplacementChoice { .. }
        | WaitingFor::CopyTargetChoice { .. }
        | WaitingFor::ExploreChoice { .. }
        | WaitingFor::EquipTarget { .. }
        | WaitingFor::CrewVehicle { .. }
        | WaitingFor::StationTarget { .. }
        | WaitingFor::SaddleMount { .. }
        | WaitingFor::ScryChoice { .. }
        | WaitingFor::DigChoice { .. }
        | WaitingFor::SurveilChoice { .. }
        | WaitingFor::RevealChoice { .. }
        | WaitingFor::SearchChoice { .. }
        | WaitingFor::ChooseFromZoneChoice { .. }
        | WaitingFor::ConniveDiscard { .. }
        | WaitingFor::DiscardChoice { .. }
        | WaitingFor::EffectZoneChoice { .. }
        | WaitingFor::LearnChoice { .. }
        | WaitingFor::ManifestDreadChoice { .. }
        | WaitingFor::BetweenGamesSideboard { .. }
        | WaitingFor::BetweenGamesChoosePlayDraw { .. }
        | WaitingFor::NamedChoice { .. }
        | WaitingFor::ModeChoice { .. }
        | WaitingFor::DiscardToHandSize { .. }
        | WaitingFor::OptionalCostChoice { .. }
        | WaitingFor::DefilerPayment { .. }
        | WaitingFor::AbilityModeChoice { .. }
        | WaitingFor::AdventureCastChoice { .. }
        | WaitingFor::ModalFaceChoice { .. }
        | WaitingFor::WarpCostChoice { .. }
        | WaitingFor::EvokeCostChoice { .. }
        | WaitingFor::OverloadCostChoice { .. }
        | WaitingFor::ChooseRingBearer { .. }
        | WaitingFor::ChooseDungeon { .. }
        | WaitingFor::ChooseDungeonRoom { .. }
        | WaitingFor::DiscardForCost { .. }
        | WaitingFor::SacrificeForCost { .. }
        | WaitingFor::ReturnToHandForCost { .. }
        | WaitingFor::BlightChoice { .. }
        | WaitingFor::TapCreaturesForSpellCost { .. }
        | WaitingFor::TapCreaturesForManaAbility { .. }
        | WaitingFor::ChooseManaColor { .. }
        | WaitingFor::ExileFromGraveyardForCost { .. }
        | WaitingFor::CollectEvidenceChoice { .. }
        | WaitingFor::HarmonizeTapChoice { .. }
        | WaitingFor::OptionalEffectChoice { .. }
        | WaitingFor::OpponentMayChoice { .. }
        | WaitingFor::TributeChoice { .. }
        | WaitingFor::UnlessPayment { .. }
        | WaitingFor::WardDiscardChoice { .. }
        | WaitingFor::WardSacrificeChoice { .. }
        | WaitingFor::DiscoverChoice { .. }
        | WaitingFor::CascadeChoice { .. }
        | WaitingFor::TopOrBottomChoice { .. }
        | WaitingFor::PopulateChoice { .. }
        | WaitingFor::ClashCardPlacement { .. }
        | WaitingFor::VoteChoice { .. }
        | WaitingFor::CompanionReveal { .. }
        | WaitingFor::ChooseLegend { .. }
        | WaitingFor::BattleProtectorChoice { .. }
        | WaitingFor::ProliferateChoice { .. }
        | WaitingFor::CategoryChoice { .. }
        | WaitingFor::AssignCombatDamage { .. }
        // CR 107.1c + CR 107.14: "Pay any amount of X" prompts are forced
        // mid-resolution choices; route to ActivateAbility as a catch-all.
        | WaitingFor::PayAmountChoice { .. }
        | WaitingFor::GameOver { .. }
        // CR 702.xxx: Paradigm (Strixhaven) — modeled as an ability-style
        // offer decision. Assign when WotC publishes SOS CR update.
        | WaitingFor::ParadigmCastOffer { .. }
        // CR 702.94a: Miracle reveal — opt-in cast offer, routed to the
        // ability-offer bucket so activation policies evaluate the candidates.
        | WaitingFor::MiracleReveal { .. }
        | WaitingFor::MiracleCastOffer { .. }
        | WaitingFor::MadnessCastOffer { .. }
        | WaitingFor::DiscardForManaAbility { .. }
        | WaitingFor::PayManaAbilityMana { .. } => DecisionKind::ActivateAbility,
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
        };

        // Mulligan routing.
        assert_eq!(
            classify(
                &WaitingFor::MulliganDecision {
                    player: PlayerId(0),
                    mulligan_count: 0,
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
        // Combat routing.
        assert_eq!(
            classify(
                &WaitingFor::DeclareAttackers {
                    player: PlayerId(0),
                    valid_attacker_ids: vec![],
                    valid_attack_targets: vec![],
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
                    object_id: ObjectId(0)
                }
            ),
            DecisionKind::ActivateManaAbility
        );
    }
}
