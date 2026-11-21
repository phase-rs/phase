//! Issue #10951: CR 118.12 + CR 118.3 + CR 608.2d — "you may pay <cost> and
//! <B>" is one option, a cost paid on resolution. It can be chosen only if the
//! payment AND the conjoined step can both be performed in full, so with an
//! impossible conjunct the option is not offered, the payment is not made and
//! the "If you do" rider does not happen.
//!
//! Rulings: Wolf of Devil's Breach — "You can't choose to discard a card
//! without also paying {1}{R}." Timothar, Baron of Bats — "If you can't exile
//! the card, perhaps because another effect moved the card before the
//! triggered ability started resolving, you can't pay {1} and won't create a
//! token."

use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::process_triggers;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;
use crate::support::shared_card_db;

const CONSPIRACY_THEORIST: &str = "Whenever this creature attacks, you may pay {1} and discard a card. If you do, draw a card.\nWhenever you discard one or more nonland cards, you may exile one of them from your graveyard. If you do, you may cast it this turn.";

const WOLF_OF_DEVILS_BREACH: &str = "Whenever this creature attacks, you may pay {1}{R} and discard a card. If you do, this creature deals damage to target creature or planeswalker equal to the discarded card's mana value.";

const KINZU: &str = "Flying\nWhenever another nontoken creature you control dies, you may pay 2 life and exile it. If you do, create a token that's a copy of that creature, except it's 1/1 and has toxic 1. (Players dealt combat damage by it also get a poison counter.)";

const TIMOTHAR: &str = "Ward—Discard a card.\nWhenever another nontoken Vampire you control dies, you may pay {1} and exile it. If you do, create a 1/1 black Bat creature token with flying. It gains \"When this token deals combat damage to a player, sacrifice it and return the exiled card to the battlefield tapped.\"";

const OVERABUNDANCE: &str = "Whenever a player taps a land for mana, that player adds one mana of any type that land produced, and this enchantment deals 1 damage to the player.";

fn untapped_lands(runner: &GameRunner, lands: &[ObjectId]) -> usize {
    lands
        .iter()
        .filter(|id| !runner.state().objects[*id].tapped)
        .count()
}

fn tokens_controlled_by(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &runner.state().objects[*id];
            obj.is_token && obj.controller == player
        })
        .count()
}

/// Declares `attacker` attacking P1, answers a trigger target prompt with
/// `target` and stops at the first other prompt or once the attack trigger has
/// left the stack.
fn attack_with(runner: &mut GameRunner, attacker: ObjectId, target: Option<ObjectId>) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare the attack");
    if let (WaitingFor::TriggerTargetSelection { .. }, Some(target)) =
        (&runner.state().waiting_for, target)
    {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("target the creature");
    }
    assert!(
        !runner.state().stack.is_empty(),
        "reach: the attack trigger is on the stack; got {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
}

/// Kills `creature` with a game-effect move and puts the dies triggers on the
/// stack.
fn dies(runner: &mut GameRunner, creature: ObjectId) {
    let mut events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(creature, Zone::Graveyard, creature),
        &mut events,
    ));
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: the dies trigger fired and is on the stack"
    );
}

/// Accepts the offer and answers every payment and discard prompt until the
/// trigger has finished resolving.
fn accept_and_finish(runner: &mut GameRunner) {
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the optional payment");
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay the mana from untapped lands");
            }
            WaitingFor::DiscardChoice { cards, count, .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: cards.into_iter().take(count).collect(),
                    })
                    .expect("discard the chosen card");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => runner.advance_until_stack_empty(),
            other => panic!("unexpected prompt while resolving the accepted offer: {other:?}"),
        }
    }
    panic!(
        "the accepted offer did not finish; waiting_for={:?}",
        runner.state().waiting_for
    );
}

fn assert_resolved_as_declined(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
        "the impossible conjoined payment is not offered; got {:?}",
        state.waiting_for
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the trigger resolved as declined; got {:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty(), "nothing of the trigger is left");
}

/// Conspiracy Theorist with an empty hand and an untapped Mountain: the {1} is
/// payable but no card can be discarded, so "pay {1} and discard a card" is
/// impossible. No prompt, the Mountain stays untapped and no card is drawn.
#[test]
fn conspiracy_theorist_with_empty_hand_is_not_offered_and_pays_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theorist = scenario
        .add_creature(P0, "Conspiracy Theorist", 2, 2)
        .from_oracle_text(CONSPIRACY_THEORIST)
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_spell_to_library_top(P0, "Library Card", false);
    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "reach: the hand is empty"
    );
    let library_before = runner.state().players[P0.0 as usize].library.len();

    attack_with(&mut runner, theorist, None);

    assert_resolved_as_declined(&runner);
    assert_eq!(
        untapped_lands(&runner, &[mountain]),
        1,
        "the {{1}} is not paid alone"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        library_before,
        "the \"If you do\" draw does not happen"
    );
}

/// Control: with a card in hand the same option is offered, and accepting pays
/// {1}, discards the card and draws.
#[test]
fn conspiracy_theorist_with_a_card_in_hand_is_offered_and_completes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theorist = scenario
        .add_creature(P0, "Conspiracy Theorist", 2, 2)
        .from_oracle_text(CONSPIRACY_THEORIST)
        .id();
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    // A land, so the discard does not also fire the second ability's
    // "nonland cards" trigger.
    let card = scenario.add_land_to_hand(P0, "Forest").id();
    let drawn = scenario
        .add_spell_to_library_top(P0, "Library Card", false)
        .id();
    let mut runner = scenario.build();

    attack_with(&mut runner, theorist, None);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "a payable conjunction is offered; got {:?}",
        runner.state().waiting_for
    );
    accept_and_finish(&mut runner);

    assert_eq!(untapped_lands(&runner, &[mountain]), 0, "{{1}} was paid");
    assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&drawn].zone,
        Zone::Hand,
        "the \"If you do\" draw happened"
    );
}

/// Wolf of Devil's Breach with an empty hand and {1}{R} available: the option
/// is not offered (ruling: no discard without paying, and no payment without
/// the discard), no mana is spent and the opposing creature takes no damage.
#[test]
fn wolf_of_devils_breach_with_empty_hand_is_not_offered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wolf = scenario
        .add_creature(P0, "Wolf of Devil's Breach", 5, 5)
        .from_oracle_text(WOLF_OF_DEVILS_BREACH)
        .id();
    let lands = [
        scenario.add_basic_land(P0, ManaColor::Red),
        scenario.add_basic_land(P0, ManaColor::Red),
    ];
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    attack_with(&mut runner, wolf, Some(bear));

    assert_resolved_as_declined(&runner);
    assert_eq!(untapped_lands(&runner, &lands), 2, "{{1}}{{R}} is not paid");
    assert_eq!(runner.state().objects[&bear].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        0,
        "the \"If you do\" damage does not happen"
    );
}

/// Kinzu of the Bleak Coven: the dead creature is exiled by another effect
/// while the trigger waits, so "pay 2 life and exile it" is impossible. No
/// prompt, no life is paid and no token copy is created.
#[test]
fn kinzu_with_the_card_gone_is_not_offered_and_pays_no_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kinzu of the Bleak Coven", 5, 4)
        .from_oracle_text(KINZU);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    dies(&mut runner, bear);
    let mut events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(bear, Zone::Exile, bear),
        &mut events,
    ));
    runner.advance_until_stack_empty();

    assert_resolved_as_declined(&runner);
    assert_eq!(runner.life(P0), life_before, "2 life is not paid alone");
    assert_eq!(tokens_controlled_by(&runner, P0), 0, "no token copy");
}

/// Control: with the card still in the graveyard Kinzu's option is offered,
/// and accepting pays 2 life, exiles the card and creates the copy.
#[test]
fn kinzu_with_the_card_in_the_graveyard_is_offered_and_completes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kinzu of the Bleak Coven", 5, 4)
        .from_oracle_text(KINZU);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    dies(&mut runner, bear);
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "a payable conjunction is offered; got {:?}",
        runner.state().waiting_for
    );
    accept_and_finish(&mut runner);

    assert_eq!(runner.life(P0), life_before - 2, "2 life was paid");
    assert_eq!(runner.state().objects[&bear].zone, Zone::Exile);
    assert_eq!(tokens_controlled_by(&runner, P0), 1, "the copy was created");
}

/// Timothar, Baron of Bats: the dead Vampire is exiled by another effect while
/// the trigger waits (the ruling's own example), so "pay {1} and exile it" is
/// impossible. No prompt, the land stays untapped and no Bat is created.
#[test]
fn timothar_with_the_vampire_gone_is_not_offered_and_pays_no_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Timothar, Baron of Bats", 4, 4)
        .from_oracle_text(TIMOTHAR);
    let vampire = scenario
        .add_creature(P0, "Vampire Lacerator", 2, 2)
        .with_subtypes(vec!["Vampire"])
        .id();
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let mut runner = scenario.build();

    dies(&mut runner, vampire);
    let mut events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(vampire, Zone::Exile, vampire),
        &mut events,
    ));
    runner.advance_until_stack_empty();

    assert_resolved_as_declined(&runner);
    assert_eq!(
        untapped_lands(&runner, &[swamp]),
        1,
        "the {{1}} is not paid alone"
    );
    assert_eq!(tokens_controlled_by(&runner, P0), 0, "no Bat token");
}

/// Control: with the Vampire still in the graveyard Timothar's option is
/// offered, and accepting pays {1}, exiles it and creates the Bat.
#[test]
fn timothar_with_the_vampire_in_the_graveyard_is_offered_and_completes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Timothar, Baron of Bats", 4, 4)
        .from_oracle_text(TIMOTHAR);
    let vampire = scenario
        .add_creature(P0, "Vampire Lacerator", 2, 2)
        .with_subtypes(vec!["Vampire"])
        .id();
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let mut runner = scenario.build();

    dies(&mut runner, vampire);
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "a payable conjunction is offered; got {:?}",
        runner.state().waiting_for
    );
    accept_and_finish(&mut runner);

    assert_eq!(untapped_lands(&runner, &[swamp]), 0, "{{1}} was paid");
    assert_eq!(runner.state().objects[&vampire].zone, Zone::Exile);
    assert_eq!(tokens_controlled_by(&runner, P0), 1, "the Bat was created");
}

const PRISTINE_TALISMAN: &str = "{T}: Add {C}. You gain 1 life.";

const NEFARIOUS_LICH: &str = "If damage would be dealt to you, exile that many cards from your graveyard instead. If you can't, you lose the game.\nIf you would gain life, draw that many cards instead.\nWhen this enchantment leaves the battlefield, you lose the game.";

/// Review F1 on this fix (CR 608.2g + CR 605.3b + CR 614.6): with an empty
/// hand, funding the {1} can still grow the hand, because mana abilities
/// activated to pay it resolve inline and their resolution events meet the
/// replacement pipeline. Pristine Talisman's "You gain 1 life" is replaced by
/// Nefarious Lich's "draw that many cards instead", so the conjoined discard
/// is payable from that draw and the option is offered — and completing it
/// pays the {1}, discards the drawn land and runs the "If you do" rider.
#[test]
fn conspiracy_theorist_funding_draw_offers_and_completes_the_conjunction() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theorist = scenario
        .add_creature(P0, "Conspiracy Theorist", 2, 2)
        .from_oracle_text(CONSPIRACY_THEORIST)
        .id();
    let talisman = scenario
        .add_artifact_from_oracle(P0, "Pristine Talisman", PRISTINE_TALISMAN)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH);
    // Draw order (the last card added to the library top is drawn first): the
    // Lich draw (from funding) takes the land, which the conjoined discard
    // then discards — a land, so the Theorist's separate "whenever you discard
    // one or more nonland cards" trigger does not interfere; the "If you do"
    // rider draw takes the spell underneath it.
    let rider_draw = scenario
        .add_spell_to_library_top(P0, "Library Card", false)
        .id();
    let funded_draw = scenario.add_land_to_library_top(P0, "Forest").id();
    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "reach: the hand is empty"
    );
    let life_before = runner.life(P0);

    attack_with(&mut runner, theorist, None);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "a conjunction funding can complete is offered; got {:?}",
        runner.state().waiting_for
    );
    accept_and_finish(&mut runner);

    assert!(
        runner.state().objects[&talisman].tapped,
        "the {{1}} was paid from the Talisman"
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "the gain was replaced by the draw"
    );
    assert_eq!(
        runner.state().objects[&funded_draw].zone,
        Zone::Graveyard,
        "the funding draw was discarded"
    );
    assert_eq!(
        runner.state().objects[&rider_draw].zone,
        Zone::Hand,
        "the \"If you do\" draw happened"
    );
}

/// Control for the funding probe: Pristine Talisman with NO gain-life
/// replacement on the battlefield cannot grow the hand, so the empty-hand
/// option is still impossible and auto-declines without spending resources.
#[test]
fn conspiracy_theorist_empty_hand_with_only_life_gaining_funding_is_still_not_offered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theorist = scenario
        .add_creature(P0, "Conspiracy Theorist", 2, 2)
        .from_oracle_text(CONSPIRACY_THEORIST)
        .id();
    let talisman = scenario
        .add_artifact_from_oracle(P0, "Pristine Talisman", PRISTINE_TALISMAN)
        .id();
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    attack_with(&mut runner, theorist, None);

    assert_resolved_as_declined(&runner);
    assert!(
        !runner.state().objects[&talisman].tapped,
        "the {{1}} is not paid alone"
    );
    assert_eq!(runner.life(P0), life_before, "no funding happened");
}

/// The land beside Pristine Talisman on [`talisman_board`].
#[derive(Clone, Copy)]
enum OtherSource {
    /// A basic Forest, which the automatic planner ranks after the Talisman
    /// for a generic {1}.
    Forest,
    /// Wastes ("{T}: Add {C}."), a free colorless land the automatic planner
    /// prefers to the Talisman for a generic {1}.
    Wastes,
}

/// Builds the review witness board: Conspiracy Theorist, Pristine Talisman
/// and `other`, optionally Nefarious Lich, an empty hand, and a land (drawn
/// first) on a spell. Returns the runner and (Theorist, Talisman, `other`,
/// land drawn first, spell drawn second).
fn talisman_board(with_lich: bool, other: OtherSource) -> (GameRunner, [ObjectId; 5]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theorist = scenario
        .add_creature(P0, "Conspiracy Theorist", 2, 2)
        .from_oracle_text(CONSPIRACY_THEORIST)
        .id();
    let talisman = scenario
        .add_artifact_from_oracle(P0, "Pristine Talisman", PRISTINE_TALISMAN)
        .id();
    if with_lich {
        scenario.add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH);
    }
    let land = match other {
        OtherSource::Forest => scenario.add_basic_land(P0, ManaColor::Green),
        OtherSource::Wastes => scenario.add_real_card(
            P0,
            "Wastes",
            Zone::Battlefield,
            shared_card_db().expect("the checked-in fixture contains Wastes"),
        ),
    };
    let second_draw = scenario
        .add_spell_to_library_top(P0, "Library Card", false)
        .id();
    let first_draw = scenario.add_land_to_library_top(P0, "Forest").id();
    let runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "reach: the hand is empty"
    );
    (runner, [theorist, talisman, land, first_draw, second_draw])
}

/// Declares the Theorist's attack and stops at P0's priority with the attack
/// trigger still on the stack.
fn attack_and_hold_priority(runner: &mut GameRunner, theorist: ObjectId) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(theorist, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare the attack");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "reach: P0 holds priority; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: the attack trigger waits on the stack"
    );
}

/// Floats `land`'s mana through the public mana action.
fn float_land(runner: &mut GameRunner, land: ObjectId) {
    let selection =
        engine::game::mana_sources::activatable_mana_actions_for_player(runner.state(), P0)
            .into_iter()
            .find_map(|action| match action {
                GameAction::TapLandForMana { selection } if selection.source.object_id == land => {
                    Some(selection)
                }
                _ => None,
            })
            .expect("the land exposes a mana action");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("float the land's mana");
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        1,
        "reach: one mana floats while the trigger waits"
    );
}

fn assert_talisman_lich_conjunction_completed(
    runner: &GameRunner,
    [_, talisman, _, first_draw, second_draw]: [ObjectId; 5],
    life_before: i32,
) {
    assert!(
        runner.state().objects[&talisman].tapped,
        "the Talisman was activated before the payment"
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "the gain was replaced by the draw"
    );
    assert_eq!(
        runner.state().objects[&first_draw].zone,
        Zone::Graveyard,
        "the card the Talisman drew was discarded"
    );
    assert_eq!(
        runner.state().objects[&second_draw].zone,
        Zone::Hand,
        "the \"If you do\" draw happened"
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Review F1 (CR 608.2g + CR 605.3b + CR 614.6): {G} already floats when the
/// trigger resolves, so the automatic planner needs no source for the {1}.
/// CR 608.2g still lets the payer activate Pristine Talisman before paying;
/// Nefarious Lich turns its life gain into a draw, which the conjoined discard
/// then takes. The option is offered and completes.
#[test]
fn conspiracy_theorist_with_floating_mana_still_offers_the_talisman_draw() {
    let (mut runner, ids) = talisman_board(true, OtherSource::Forest);
    let [theorist, _, forest, ..] = ids;
    let life_before = runner.life(P0);

    attack_and_hold_priority(&mut runner, theorist);
    float_land(&mut runner, forest);
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the conjunction a Talisman activation completes is offered; got {:?}",
        runner.state().waiting_for
    );
    accept_and_finish(&mut runner);

    assert_talisman_lich_conjunction_completed(&runner, ids, life_before);
}

/// The sibling of review F1: with an empty pool and an untapped Wastes, the
/// automatic planner prefers the Wastes to the Talisman for the {1}, so its
/// own plan draws nothing. Another legal funding — the Talisman — does, so the
/// option is offered, and the payment funds through the Talisman, leaving the
/// Wastes untapped.
#[test]
fn conspiracy_theorist_funds_through_the_talisman_over_a_preferred_land() {
    let (mut runner, ids) = talisman_board(true, OtherSource::Wastes);
    let [theorist, _, wastes, ..] = ids;
    let life_before = runner.life(P0);

    attack_with(&mut runner, theorist, None);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the conjunction a Talisman activation completes is offered; got {:?}",
        runner.state().waiting_for
    );
    accept_and_finish(&mut runner);

    assert_talisman_lich_conjunction_completed(&runner, ids, life_before);
    assert!(
        !runner.state().objects[&wastes].tapped,
        "the Talisman's {{C}} paid the {{1}}; the Wastes was not needed"
    );
}

/// Control for both fundings: without Nefarious Lich no funding can grow the
/// empty hand, so with one mana floating and an untapped Talisman the option is
/// still impossible — beside a Forest the planner ranks after the Talisman and
/// beside a Wastes it ranks before it. It auto-declines, the Talisman stays
/// untapped, the floating mana is not spent and no card is drawn.
#[test]
fn conspiracy_theorist_with_floating_mana_and_no_lich_is_not_offered() {
    for other in [OtherSource::Forest, OtherSource::Wastes] {
        let (mut runner, ids) = talisman_board(false, other);
        let [theorist, talisman, land, first_draw, second_draw] = ids;
        let life_before = runner.life(P0);

        attack_and_hold_priority(&mut runner, theorist);
        float_land(&mut runner, land);
        runner.act(GameAction::PassPriority).expect("pass priority");
        runner.act(GameAction::PassPriority).expect("P1 passes");

        assert_resolved_as_declined(&runner);
        assert!(
            !runner.state().objects[&talisman].tapped,
            "the Talisman is not activated"
        );
        assert_eq!(
            runner.state().players[P0.0 as usize].mana_pool.total(),
            1,
            "the floating mana is not spent"
        );
        assert_eq!(runner.life(P0), life_before, "no funding happened");
        assert_eq!(runner.state().objects[&first_draw].zone, Zone::Library);
        assert_eq!(runner.state().objects[&second_draw].zone, Zone::Library);
    }
}

/// Review F1 on the funding search (CR 608.2d + CR 608.2g): the conjunction
/// verdict probes fundings with a bounded amount of work. Builds Conspiracy
/// Theorist with an empty hand, `lands` untapped basic lands and `talismans`
/// Pristine Talismans (optionally beside Nefarious Lich), a land drawn first
/// and a spell drawn second, then declares the attack and stops with the
/// trigger waiting on the stack. Returns the runner, the Theorist's lands and
/// Talismans, and the two library cards.
struct LargeBoard {
    runner: GameRunner,
    lands: Vec<ObjectId>,
    talismans: Vec<ObjectId>,
    first_draw: ObjectId,
    second_draw: ObjectId,
}

fn large_board(lands: usize, talismans: usize, with_lich: bool) -> LargeBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theorist = scenario
        .add_creature(P0, "Conspiracy Theorist", 2, 2)
        .from_oracle_text(CONSPIRACY_THEORIST)
        .id();
    let land_ids: Vec<ObjectId> = (0..lands)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    let talisman_ids: Vec<ObjectId> = (0..talismans)
        .map(|_| {
            scenario
                .add_artifact_from_oracle(P0, "Pristine Talisman", PRISTINE_TALISMAN)
                .id()
        })
        .collect();
    if with_lich {
        scenario.add_enchantment_from_oracle(P0, "Nefarious Lich", NEFARIOUS_LICH);
    }
    let second_draw = scenario
        .add_spell_to_library_top(P0, "Library Card", false)
        .id();
    let first_draw = scenario.add_land_to_library_top(P0, "Forest").id();
    let mut runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "reach: the hand is empty"
    );
    attack_and_hold_priority(&mut runner, theorist);
    LargeBoard {
        runner,
        lands: land_ids,
        talismans: talisman_ids,
        first_draw,
        second_draw,
    }
}

/// Resolves the waiting attack trigger and returns how many funding probes
/// its resolution ran.
fn resolve_trigger_counting_probes(runner: &mut GameRunner) -> u64 {
    perf_counters::reset();
    runner.act(GameAction::PassPriority).expect("pass priority");
    runner.act(GameAction::PassPriority).expect("P1 passes");
    perf_counters::effect_mana_funding_probe_snapshot().probes
}

/// Tapping a basic land only adds mana, so activating one first decides
/// nothing the planner's own funding does not: the verdict runs one probe
/// however many lands there are. On a 2-land and a 60-land board alike the
/// empty-hand option is still impossible and auto-declines without tapping a
/// land or drawing.
#[test]
fn conspiracy_theorist_funding_probes_do_not_grow_with_plain_lands() {
    let mut probes = Vec::new();
    for lands in [2, 60] {
        let mut board = large_board(lands, 0, false);
        probes.push(resolve_trigger_counting_probes(&mut board.runner));

        assert_resolved_as_declined(&board.runner);
        assert_eq!(
            untapped_lands(&board.runner, &board.lands),
            lands,
            "the {{1}} is not paid alone"
        );
        assert_eq!(
            board.runner.state().objects[&board.first_draw].zone,
            Zone::Library
        );
        assert_eq!(
            board.runner.state().objects[&board.second_draw].zone,
            Zone::Library
        );
    }
    assert_eq!(
        probes,
        vec![1, 1],
        "only the planner's own funding is probed, once per resolution"
    );
}

/// A Talisman's activation does more than add mana, so each is a funding to
/// probe — up to the budget. Below it, the empty-hand option without Nefarious
/// Lich is proved impossible and auto-declines. Past it, the probes stop at the
/// budget and the search, cut short, decides nothing: the option is offered
/// (as the reservation search's exhaustion is) and declining it pays nothing.
#[test]
fn conspiracy_theorist_funding_probes_stop_at_the_budget() {
    use engine::game::casting::EFFECT_MANA_FUNDING_PROBE_BUDGET;

    let few = EFFECT_MANA_FUNDING_PROBE_BUDGET / 4;
    let mut board = large_board(0, few, false);
    let probes = resolve_trigger_counting_probes(&mut board.runner);
    assert_eq!(
        probes,
        1 + few as u64,
        "the planner's funding and each Talisman"
    );
    assert_resolved_as_declined(&board.runner);
    assert_eq!(untapped_lands(&board.runner, &board.talismans), few);

    let many = EFFECT_MANA_FUNDING_PROBE_BUDGET * 3;
    let mut board = large_board(0, many, false);
    let life_before = board.runner.life(P0);
    let probes = resolve_trigger_counting_probes(&mut board.runner);
    assert_eq!(
        probes,
        1 + EFFECT_MANA_FUNDING_PROBE_BUDGET as u64,
        "the probes stop at the budget"
    );
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "an exhausted search leaves the option offered; got {:?}",
        board.runner.state().waiting_for
    );
    board
        .runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the option");
    assert!(board.runner.state().stack.is_empty());
    assert_eq!(
        untapped_lands(&board.runner, &board.talismans),
        many,
        "declining pays nothing"
    );
    assert_eq!(board.runner.life(P0), life_before);
    assert_eq!(
        board.runner.state().objects[&board.second_draw].zone,
        Zone::Library
    );
}

/// The bound keeps the Talisman/Lich witness: beside 60 basic lands the
/// Talisman is the only funding probed past the planner's own, so the
/// conjunction its Lich draw completes is offered and completes.
#[test]
fn conspiracy_theorist_large_land_board_still_offers_the_talisman_draw() {
    let mut board = large_board(60, 1, true);
    let life_before = board.runner.life(P0);
    let probes = resolve_trigger_counting_probes(&mut board.runner);
    assert!(
        probes <= 2,
        "at most the planner's funding and the Talisman's; got {probes}"
    );
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the conjunction a Talisman activation completes is offered; got {:?}",
        board.runner.state().waiting_for
    );
    accept_and_finish(&mut board.runner);

    let talisman = board.talismans[0];
    assert_talisman_lich_conjunction_completed(
        &board.runner,
        [
            ObjectId(0),
            talisman,
            ObjectId(0),
            board.first_draw,
            board.second_draw,
        ],
        life_before,
    );
    assert_eq!(
        untapped_lands(&board.runner, &board.lands),
        60,
        "the Talisman's {{C}} paid the {{1}}"
    );
}

/// Review F1 on the exhaustion fallback (CR 118.3 + CR 608.2d): Conspiracy
/// Theorist with an empty hand beside Overabundance and one more
/// Mountain than the probe budget admits. Overabundance gives every Mountain's
/// mana ability a trigger with a non-mana effect, so each is a funding to
/// probe, the search is cut short, and the empty-hand option is offered
/// undecided. No funding can grow the hand — there is no life-gain replacement
/// here — so "pay {1} and discard a card" is impossible however it is funded.
struct ExhaustedBoard {
    runner: GameRunner,
    theorist: ObjectId,
    lands: Vec<ObjectId>,
    first_draw: ObjectId,
    second_draw: ObjectId,
    life_before: i32,
    library_before: usize,
}

fn exhausted_board() -> ExhaustedBoard {
    use engine::game::casting::EFFECT_MANA_FUNDING_PROBE_BUDGET;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theorist = scenario
        .add_creature(P0, "Conspiracy Theorist", 2, 2)
        .from_oracle_text(CONSPIRACY_THEORIST)
        .id();
    let lands: Vec<ObjectId> = (0..=EFFECT_MANA_FUNDING_PROBE_BUDGET)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    scenario.add_enchantment_from_oracle(P0, "Overabundance", OVERABUNDANCE);
    let second_draw = scenario
        .add_spell_to_library_top(P0, "Library Card", false)
        .id();
    let first_draw = scenario.add_land_to_library_top(P0, "Forest").id();
    let runner = scenario.build();
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "reach: the hand is empty"
    );
    let life_before = runner.life(P0);
    let library_before = runner.state().players[P0.0 as usize].library.len();
    ExhaustedBoard {
        runner,
        theorist,
        lands,
        first_draw,
        second_draw,
        life_before,
        library_before,
    }
}

/// Requires the exhausted offer to be open, answers it with `answer`, and
/// returns how many funding probes the answer ran.
fn answer_exhausted_offer(runner: &mut GameRunner, answer: GameAction) -> u64 {
    let WaitingFor::OptionalEffectChoice { .. } = runner.state().waiting_for else {
        panic!(
            "reach: the exhausted search offers the option; got {:?}",
            runner.state().waiting_for
        )
    };
    let before = perf_counters::effect_mana_funding_probe_snapshot().probes;
    runner.act(answer).expect("accept the offered option");
    perf_counters::effect_mana_funding_probe_snapshot().probes - before
}

/// Nothing of the accepted option was performed: no Mountain tapped for the
/// {1} (so Overabundance never dealt its damage), no card discarded or drawn,
/// and the attack trigger is gone with P0 holding priority.
fn assert_nothing_of_the_option_was_paid(board: &ExhaustedBoard) {
    let runner = &board.runner;
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the accepted option resolved declined; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing of the trigger is left"
    );
    assert_eq!(
        untapped_lands(runner, &board.lands),
        board.lands.len(),
        "the {{1}} is not paid alone"
    );
    assert_eq!(
        runner.life(P0),
        board.life_before,
        "no Mountain was tapped, so Overabundance's damage never fires"
    );
    assert!(runner.state().players[P0.0 as usize].hand.is_empty());
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        board.library_before,
        "the \"If you do\" draw does not happen"
    );
    assert_eq!(
        runner.state().objects[&board.first_draw].zone,
        Zone::Library
    );
    assert_eq!(
        runner.state().objects[&board.second_draw].zone,
        Zone::Library
    );
}

/// ACCEPTING the undecided offer — through the prompt's own answer and
/// through the answer-and-remember action — must not spend the {1}: no probed
/// funding confirmed the conjoined discard, so the acceptance resolves the
/// option declined (CR 118.3: a player can't pay a cost without the resources
/// to pay it fully). The acceptance reruns only the bounded search. (A later
/// remembered answer reaches the same acceptance authority,
/// `resolve_optional_effect_decision_step`.)
#[test]
fn conspiracy_theorist_accepting_after_budget_exhaustion_does_not_spend_the_head() {
    use engine::game::casting::EFFECT_MANA_FUNDING_PROBE_BUDGET;
    use engine::types::game_state::{AutoMayChoice, MayTriggerAutoChoiceScope};

    let bounded_search = 1 + EFFECT_MANA_FUNDING_PROBE_BUDGET as u64;
    for answer in [
        GameAction::DecideOptionalEffect { accept: true },
        GameAction::DecideOptionalEffectAndRemember {
            choice: AutoMayChoice::Accept,
            scope: MayTriggerAutoChoiceScope::ExactInstance,
        },
    ] {
        let mut board = exhausted_board();
        attack_and_hold_priority(&mut board.runner, board.theorist);
        assert_eq!(
            resolve_trigger_counting_probes(&mut board.runner),
            bounded_search,
            "the offer runs one bounded search: the planner's funding and one \
             per in-budget Mountain"
        );
        let accept_probes = answer_exhausted_offer(&mut board.runner, answer.clone());
        assert_nothing_of_the_option_was_paid(&board);
        assert_eq!(
            accept_probes, bounded_search,
            "{answer:?}: the acceptance reruns one bounded search and probes nothing more"
        );
    }
}
