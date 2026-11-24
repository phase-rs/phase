use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::{CoreType, ObjectId, PlayerId, Zone};

const P2: PlayerId = PlayerId(2);

fn mana_cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: shards.to_vec(),
    }
}

fn add_sigil(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(
            P0,
            "Tainted Sigil",
            "{T}, Sacrifice this artifact: You gain life equal to the total life lost by all players this turn. (Damage causes loss of life.)",
        )
        .with_mana_cost(mana_cost(1, &[ManaCostShard::White, ManaCostShard::Black]))
        .id()
}

fn add_instant(
    scenario: &mut GameScenario,
    name: &str,
    oracle: &str,
    shard: ManaCostShard,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, true, oracle)
        .with_mana_cost(mana_cost(0, &[shard]))
        .id()
}

fn add_shock(scenario: &mut GameScenario) -> ObjectId {
    add_instant(
        scenario,
        "Shock",
        "Shock deals 2 damage to any target.",
        ManaCostShard::Red,
    )
}

fn add_greed(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Greed", "{B}, Pay 2 life: Draw a card.")
        .with_mana_cost(mana_cost(3, &[ManaCostShard::Black]))
        .id()
}

fn assert_type(runner: &GameRunner, id: ObjectId, card_type: CoreType) {
    let types = &runner.state().objects[&id].base_card_types;
    assert_eq!(types.core_types.as_slice(), &[card_type]);
    assert!(types.supertypes.is_empty());
    assert!(types.subtypes.is_empty());
}

fn assert_life(runner: &GameRunner, expected: [i32; 3]) {
    let actual = [P0, P1, P2].map(|player| runner.state().players[player.0 as usize].life);
    assert_eq!(actual, expected);
}

fn activate_sigil(sigil: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id: sigil,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    }
}

fn announce_sigil(runner: &mut GameRunner, sigil: ObjectId) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    let source = &runner.state().objects[&sigil];
    assert_eq!(source.zone, Zone::Battlefield);
    assert!(!source.tapped);
    let incarnation = source.incarnation;
    runner.act(activate_sigil(sigil)).unwrap();
    let state = runner.state();
    // CR 602.2b: the sacrifice is paid before anyone can respond.
    assert_eq!(state.objects[&sigil].zone, Zone::Graveyard);
    assert_ne!(state.objects[&sigil].incarnation, incarnation);
    assert_eq!(state.stack.len(), 1);
    let ability = state.stack[0].ability().expect("Sigil's activated ability");
    assert_eq!(ability.source_id, sigil);
    assert_eq!(ability.announced_source_incarnation, Some(incarnation));
    assert_eq!(ability.controller, P0);
    let snapshot = &state.lki_by_incarnation[&sigil][&incarnation];
    assert!(snapshot.tapped, "the artifact was tapped before sacrifice");
    assert_eq!(snapshot.card_types, [CoreType::Artifact]);
}

fn resolve_sigil(runner: &mut GameRunner, sigil: ObjectId, expected: [i32; 3]) {
    let mut gain_resolutions = 0;
    for _ in 0..runner.state().players.len() {
        let pass = runner.act(GameAction::PassPriority).unwrap();
        gain_resolutions += pass
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::GainLife,
                        source_id,
                        subject: None,
                    } if *source_id == sigil
                )
            })
            .count();
        if runner.state().stack.is_empty() {
            break;
        }
    }
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&sigil].zone, Zone::Graveyard);
    // Pass to the controller if the ability resolved during another player's turn.
    for _ in 0..2 {
        match runner.state().waiting_for {
            WaitingFor::Priority { player } => {
                if player == P0 {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            ref waiting @ (WaitingFor::ResolveAllConsent { .. }
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
            | WaitingFor::DeclareAttackers { .. }
            | WaitingFor::DeclareBlockers { .. }
            | WaitingFor::UntapChoice { .. }
            | WaitingFor::ChooseUntapSubset { .. }
            | WaitingFor::ExertChoice { .. }
            | WaitingFor::EnlistChoice { .. }
            | WaitingFor::GameOver { .. }
            | WaitingFor::ReplacementChoice { .. }
            | WaitingFor::ReplacementDecisionOrder { .. }
            | WaitingFor::EntryControllerChoice { .. }
            | WaitingFor::OrderTriggers { .. }
            | WaitingFor::CopyTargetChoice { .. }
            | WaitingFor::ExploreChoice { .. }
            | WaitingFor::ReturnAsAuraTarget { .. }
            | WaitingFor::ChooseTokenHost { .. }
            | WaitingFor::EquipTarget { .. }
            | WaitingFor::CrewVehicle { .. }
            | WaitingFor::StationTarget { .. }
            | WaitingFor::SaddleMount { .. }
            | WaitingFor::ScryChoice { .. }
            | WaitingFor::RippleRevealChoice { .. }
            | WaitingFor::RippleBottomOrder { .. }
            | WaitingFor::RevealUntilBottomOrder { .. }
            | WaitingFor::DigBottomOrder { .. }
            | WaitingFor::ArrangePlanarDeckTopChoice { .. }
            | WaitingFor::RedistributeLifeTotals { .. }
            | WaitingFor::CoinFlipKeepChoice { .. }
            | WaitingFor::DieKeepChoice { .. }
            | WaitingFor::DigChoice { .. }
            | WaitingFor::DigRestSplitChoice { .. }
            | WaitingFor::SurveilChoice { .. }
            | WaitingFor::RevealChoice { .. }
            | WaitingFor::SearchChoice { .. }
            | WaitingFor::SearchPartitionChoice { .. }
            | WaitingFor::OutsideGameChoice { .. }
            | WaitingFor::ChooseFromZoneChoice { .. }
            | WaitingFor::BeholdChoice { .. }
            | WaitingFor::EmpowerJaceChoice { .. }
            | WaitingFor::MemberOrderChoice { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
            | WaitingFor::ConniveDiscard { .. }
            | WaitingFor::DiscardChoice { .. }
            | WaitingFor::HeroEventChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
            | WaitingFor::LearnChoice { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::BetweenGamesSideboard { .. }
            | WaitingFor::BetweenGamesChoosePlayDraw { .. }
            | WaitingFor::ChooseStartingPlayer { .. }
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
            | WaitingFor::OrderCostReductions { .. }
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
            | WaitingFor::ManaAbilityManaPayment { .. }
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
            | WaitingFor::PhysicalContest { .. }
            | WaitingFor::VoteChoice { .. }
            | WaitingFor::SeparatePilesChooseOpponent { .. }
            | WaitingFor::SeparatePilesPartition { .. }
            | WaitingFor::SeparatePilesChoice { .. }
            | WaitingFor::SecretAgendaChoice { .. }
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
            | WaitingFor::PhyrexianPayment { .. }) => {
                panic!("unexpected post-resolution prompt: {waiting:?}")
            }
        }
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    // CR 113.7a + CR 113.6: the pending ability survived its source, but
    // a fresh activation from the graveyard is not legal.
    assert!(runner.act(activate_sigil(sigil)).is_err());
    assert!(runner.state().stack.is_empty());
    eprintln!("Sigil sacrificed and unavailable; expected final life {expected:?}");
    assert_life(runner, expected);
    assert_eq!(
        gain_resolutions, 1,
        "Sigil's gain instruction must resolve even at zero"
    );
}

#[test]
fn gross_life_lost_sums_three_players_when_the_sacrificed_ability_resolves() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler"]);
    let sigil = add_sigil(&mut scenario);
    let shock = add_shock(&mut scenario);
    let bolt = add_instant(
        &mut scenario,
        "Lightning Bolt",
        "Lightning Bolt deals 3 damage to any target.",
        ManaCostShard::Red,
    );
    let salve = add_instant(
        &mut scenario,
        "Healing Salve",
        "Choose one —\n• Target player gains 3 life.\n• Prevent the next 3 damage that would be dealt to any target this turn.",
        ManaCostShard::White,
    );
    let greed = add_greed(&mut scenario);
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let plains = scenario.add_basic_land(P0, ManaColor::White);
    let spare_mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    assert_type(&runner, sigil, CoreType::Artifact);
    assert_type(&runner, greed, CoreType::Enchantment);
    for card in [shock, bolt, salve] {
        assert_type(&runner, card, CoreType::Instant);
    }
    assert_life(&runner, [20, 20, 20]);

    runner.activate(swamp, 0).resolve();
    let paid = runner.activate(greed, 0).resolve();
    // CR 119.4: paying life is loss, not damage.
    paid.assert_life_delta(P0, -2);
    assert_eq!(paid.hand_drawn(P0), 1);
    assert!(runner.state().damage_dealt_this_turn.is_empty());
    runner.activate(mountain, 0).resolve();
    runner
        .cast(shock)
        .target_player(P1)
        .resolve()
        .assert_life_delta(P1, -2);
    runner.activate(plains, 0).resolve();
    runner
        .cast(salve)
        .modes(&[0])
        .target_player(P1)
        .resolve()
        .assert_life_delta(P1, 3);
    assert_life(&runner, [18, 21, 20]);
    runner.activate(spare_mountain, 0).resolve();
    announce_sigil(&mut runner, sigil);

    // CR 608.2h: loss in response counts as well as loss before announcement.
    let response = runner.cast(bolt).target_player(P2).commit();
    assert_eq!(response.state().stack.len(), 2);
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Graveyard);
    assert_life(&runner, [18, 21, 17]);
    resolve_sigil(&mut runner, sigil, [25, 21, 17]);
}

#[test]
fn gross_life_lost_includes_a_life_payment_without_any_damage() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler"]);
    let sigil = add_sigil(&mut scenario);
    let greed = add_greed(&mut scenario);
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let mut runner = scenario.build();
    assert_type(&runner, sigil, CoreType::Artifact);
    assert_type(&runner, greed, CoreType::Enchantment);
    runner.activate(swamp, 0).resolve();
    runner
        .activate(greed, 0)
        .resolve()
        .assert_life_delta(P0, -2);
    assert!(runner.state().damage_dealt_this_turn.is_empty());
    assert_life(&runner, [18, 20, 20]);
    announce_sigil(&mut runner, sigil);
    resolve_sigil(&mut runner, sigil, [20, 20, 20]);
}

#[test]
fn gross_life_lost_does_not_read_the_immediately_previous_turn() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::End);
    let sigil = add_sigil(&mut scenario);
    let shock = add_shock(&mut scenario);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    assert_type(&runner, sigil, CoreType::Artifact);
    assert_type(&runner, shock, CoreType::Instant);
    let turn = runner.state().turn_number;
    runner.activate(mountain, 0).resolve();
    runner
        .cast(shock)
        .target_player(P1)
        .resolve()
        .assert_life_delta(P1, -2);
    assert_life(&runner, [20, 18, 20]);

    // End step into the next active player's upkeep, then get P0 priority.
    for _ in 0..6 {
        if runner.state().turn_number > turn
            && matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            )
        {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().turn_number, turn + 1);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().players[P1.0 as usize].life_lost_last_turn, 2);
    assert!(runner
        .state()
        .players
        .iter()
        .all(|p| p.life_lost_this_turn == 0));
    announce_sigil(&mut runner, sigil);
    resolve_sigil(&mut runner, sigil, [20, 18, 20]);
}

#[test]
fn gross_life_lost_still_pays_sacrifice_when_no_player_lost_life() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let sigil = add_sigil(&mut scenario);
    let mut runner = scenario.build();
    assert_type(&runner, sigil, CoreType::Artifact);
    announce_sigil(&mut runner, sigil);
    resolve_sigil(&mut runner, sigil, [20, 20, 20]);
}
