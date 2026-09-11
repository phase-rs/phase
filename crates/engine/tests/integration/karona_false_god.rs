//! Regression for Karona, False God's phase-triggered control handoff.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const KARONA_ORACLE: &str = "Haste\n\
    At the beginning of each player's upkeep, that player untaps Karona and gains control of it.\n\
    Whenever Karona attacks, creatures of the creature type of your choice get +3/+3 until end of turn.";

/// Drive normal game actions until P1's upkeep trigger has resolved, while
/// recording that the asserted handoff actually occurred during P1's upkeep.
fn advance_until_karona_controlled_by_p1(
    runner: &mut GameRunner,
    karona: engine::types::identifiers::ObjectId,
) -> bool {
    let mut reached_p1_upkeep = false;
    for _ in 0..240 {
        reached_p1_upkeep |=
            runner.state().active_player == P1 && runner.state().phase == Phase::Upkeep;
        if reached_p1_upkeep && runner.state().objects[&karona].controller == P1 {
            return true;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return false;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .is_err()
                {
                    return false;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .is_err()
                {
                    return false;
                }
            }
            // Keep this exhaustive: adding a new interaction state must make
            // this driver choose an action deliberately.
            WaitingFor::ResolveAllConsent { .. }
            | WaitingFor::ResolveAllReady { .. }
            | WaitingFor::MeldPairChoice { .. }
            | WaitingFor::MeldAttackTargetChoice { .. }
            | WaitingFor::EntryAttackTargetChoice { .. }
            | WaitingFor::MulliganDecision { .. }
            | WaitingFor::OpeningHandBottomCards { .. }
            | WaitingFor::ManaPayment { .. }
            | WaitingFor::ManaSourceSelection { .. }
            | WaitingFor::AssistChoosePlayer { .. }
            | WaitingFor::AssistPayment { .. }
            | WaitingFor::ChooseXValue { .. }
            | WaitingFor::TargetSelection { .. }
            | WaitingFor::UntapChoice { .. }
            | WaitingFor::ChooseUntapSubset { .. }
            | WaitingFor::ExertChoice { .. }
            | WaitingFor::EnlistChoice { .. }
            | WaitingFor::GameOver { .. }
            | WaitingFor::ReplacementChoice { .. }
            | WaitingFor::EntryControllerChoice { .. }
            | WaitingFor::OrderTriggers { .. }
            | WaitingFor::CopyTargetChoice { .. }
            | WaitingFor::ExploreChoice { .. }
            | WaitingFor::ReturnAsAuraTarget { .. }
            | WaitingFor::EquipTarget { .. }
            | WaitingFor::CrewVehicle { .. }
            | WaitingFor::StationTarget { .. }
            | WaitingFor::SaddleMount { .. }
            | WaitingFor::ScryChoice { .. }
            | WaitingFor::RippleRevealChoice { .. }
            | WaitingFor::RippleBottomOrder { .. }
            | WaitingFor::ArrangePlanarDeckTopChoice { .. }
            | WaitingFor::RedistributeLifeTotals { .. }
            | WaitingFor::CoinFlipKeepChoice { .. }
            | WaitingFor::DieKeepChoice { .. }
            | WaitingFor::DigChoice { .. }
            | WaitingFor::SurveilChoice { .. }
            | WaitingFor::RevealChoice { .. }
            | WaitingFor::SearchChoice { .. }
            | WaitingFor::SearchPartitionChoice { .. }
            | WaitingFor::OutsideGameChoice { .. }
            | WaitingFor::ChooseFromZoneChoice { .. }
            | WaitingFor::BeholdChoice { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
            | WaitingFor::ConniveDiscard { .. }
            | WaitingFor::DiscardChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
            | WaitingFor::LearnChoice { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::BetweenGamesSideboard { .. }
            | WaitingFor::BetweenGamesChoosePlayDraw { .. }
            | WaitingFor::NamedChoice { .. }
            | WaitingFor::OpponentGuess { .. }
            | WaitingFor::SpellbookDraft { .. }
            | WaitingFor::DamageSourceChoice { .. }
            | WaitingFor::ModeChoice { .. }
            | WaitingFor::DiscardToHandSize { .. }
            | WaitingFor::OptionalCostChoice { .. }
            | WaitingFor::ChooseGiftRecipient { .. }
            | WaitingFor::SpliceOffer { .. }
            | WaitingFor::DefilerPayment { .. }
            | WaitingFor::CastOffer { .. }
            | WaitingFor::ModalFaceChoice { .. }
            | WaitingFor::AlternativeCastChoice { .. }
            | WaitingFor::MutateMergeChoice { .. }
            | WaitingFor::CipherEncodeChoice { .. }
            | WaitingFor::CastingVariantChoice { .. }
            | WaitingFor::ChoosePermanentTypeSlot { .. }
            | WaitingFor::MultiTargetSelection { .. }
            | WaitingFor::AbilityModeChoice { .. }
            | WaitingFor::OptionalEffectChoice { .. }
            | WaitingFor::ResolutionOptionalPaymentChoice { .. }
            | WaitingFor::PairChoice { .. }
            | WaitingFor::TributeChoice { .. }
            | WaitingFor::MiracleReveal { .. }
            | WaitingFor::OpponentMayChoice { .. }
            | WaitingFor::LoopShortcut { .. }
            | WaitingFor::RespondToShortcut { .. }
            | WaitingFor::PrecastCopyShortcutOffer { .. }
            | WaitingFor::RespondToPrecastCopyShortcut { .. }
            | WaitingFor::UnlessPayment { .. }
            | WaitingFor::UnlessPaymentChooseCost { .. }
            | WaitingFor::WardDiscardChoice { .. }
            | WaitingFor::WardSacrificeChoice { .. }
            | WaitingFor::UnlessBounceChoice { .. }
            | WaitingFor::ChooseRingBearer { .. }
            | WaitingFor::ChooseRoomDoor { .. }
            | WaitingFor::ChooseDungeon { .. }
            | WaitingFor::ChooseDungeonRoom { .. }
            | WaitingFor::SpecializeColor { .. }
            | WaitingFor::PayCost { .. }
            | WaitingFor::ActivationCostOneOfChoice { .. }
            | WaitingFor::CostTypeChoice { .. }
            | WaitingFor::BlightChoice { .. }
            | WaitingFor::PayManaAbilityMana { .. }
            | WaitingFor::ChooseManaColor { .. }
            | WaitingFor::CollectEvidenceChoice { .. }
            | WaitingFor::HarmonizeTapChoice { .. }
            | WaitingFor::RevealUntilKeptChoice { .. }
            | WaitingFor::RepeatDecision { .. }
            | WaitingFor::TopOrBottomChoice { .. }
            | WaitingFor::PopulateChoice { .. }
            | WaitingFor::ClashChooseOpponent { .. }
            | WaitingFor::ChooseFromZoneOpponentChooser { .. }
            | WaitingFor::ChooseAnnouncingOpponent { .. }
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
            | WaitingFor::ChooseObjectsSelection { .. }
            | WaitingFor::CategoryChoice { .. }
            | WaitingFor::EachPlayerCopyChosenSelection { .. }
            | WaitingFor::KeepWithinTotalPowerChoice { .. }
            | WaitingFor::KeepExactPermanentsChoice { .. }
            | WaitingFor::CopyRetarget { .. }
            | WaitingFor::AssignCombatDamage { .. }
            | WaitingFor::AssignBlockerDamage { .. }
            | WaitingFor::DistributeAmong { .. }
            | WaitingFor::MoveCountersDistribution { .. }
            | WaitingFor::RemoveCountersChoice { .. }
            | WaitingFor::PayAmountChoice { .. }
            | WaitingFor::RetargetChoice { .. }
            | WaitingFor::CombatTaxPayment { .. }
            | WaitingFor::PhyrexianPayment { .. } => return false,
        }
    }
    false
}

/// CR 608.2c: P1 is the scoped player on P1's upkeep, so Karona's printed
/// untap and control instructions resolve for P1 in order.
#[test]
fn karona_upkeep_untaps_and_transfers_to_the_upkeep_player() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &player in &[P0, P1] {
        scenario.with_library_top(player, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    let karona = scenario
        .add_creature_from_oracle(P0, "Karona, False God", 5, 5, KARONA_ORACLE)
        .id();
    let mut runner = scenario.build();

    assert!(
        runner.state().objects[&karona]
            .trigger_definitions
            .iter_unchecked()
            .any(|entry| {
                entry.definition.mode == TriggerMode::Phase
                    && entry.definition.phase == Some(Phase::Upkeep)
            }),
        "complete Karona Oracle text must provide its phase/upkeep trigger"
    );
    runner.state_mut().objects.get_mut(&karona).unwrap().tapped = true;

    assert!(
        advance_until_karona_controlled_by_p1(&mut runner, karona),
        "must reach and resolve Karona's trigger during P1's upkeep"
    );
    assert_eq!(
        runner.state().active_player,
        P1,
        "handoff must occur on P1's turn"
    );
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "handoff must occur during upkeep"
    );
    let object = &runner.state().objects[&karona];
    assert_eq!(object.controller, P1, "P1 must gain control of Karona");
    assert!(
        !object.tapped,
        "Karona's trigger must untap it before the handoff"
    );
}
