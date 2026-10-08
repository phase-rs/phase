//! Runtime regression for Drake Familiar (verified MTGJSON Oracle text:
//! "Flying\nWhen this creature enters, sacrifice it unless you return an
//! enchantment to its owner's hand.").
//!
//! Reported bug: Drake Familiar always sacrificed itself, even with an
//! eligible enchantment on the battlefield. Root cause was two-layered:
//!
//! 1. Parser (`parse_unless_return_to_hand`, `oracle_trigger.rs`) forced an
//!    implicit controller scope onto the returned-object filter whenever the
//!    printed text lacked "you control", a heuristic meant for possessive-zone
//!    phrasing ("a basic land card from your graveyard"). Drake Familiar's
//!    clause names no ownership restriction at all — any enchantment on the
//!    battlefield, yours or an opponent's, is eligible (CR 118.12a) — so the
//!    forced scope made the cost unpayable whenever the caster didn't already
//!    control a matching enchantment.
//! 2. Runtime (`handle_unless_payment`'s `ReturnToHand` arm,
//!    `engine_payment_choices.rs`) additionally hardcoded `obj.controller ==
//!    player` on every eligible-object scan, which would have kept an
//!    opponent's enchantment ineligible even after the parser fix.
//!
//! A follow-up review round found the same class of bug one level down, in
//! the ZONE-possessive grammar ("...from a graveyard" vs "...from your
//! graveyard"): a first-pass fix force-added ownership scoping to every
//! zone-qualified return cost, which wrongly narrowed an UNQUALIFIED zone
//! phrase ("a graveyard" — any player's) down to only the payer's own zone.
//! `unqualified_graveyard_return_may_use_any_players_graveyard` below is the
//! production-pipeline discriminator for that case, run alongside the
//! existing "from your graveyard" coverage (`unless_return_to_hand_from_
//! graveyard_collects_graveyard_cards`, `engine_payment_choices.rs`).
//!
//! This drives the REAL parse → cast → ETB trigger → unless-payment →
//! bounce-choice pipeline (per the `card-test` skill), so it catches runtime
//! defects an AST-shape test cannot.
//!
//! CR ANCHORS (verified against docs/MagicCompRules.txt):
//!   * CR 118.12a — "[Do something] unless [a player does something else]."
//!   * CR 603.6a — a "when it enters" trigger fires when the permanent enters.
//!   * CR 701.21a — sacrifice moves the permanent to its owner's graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DRAKE_FAMILIAR: &str = "Flying\nWhen this creature enters, sacrifice it \
    unless you return an enchantment to its owner's hand.";

/// CR 118.12: Glint Hawk's controller-scoped sibling — "you return an
/// artifact YOU CONTROL to its owner's hand" — used as the discriminating
/// control group: the control restriction must remain enforced where the
/// Oracle text actually prints it.
const GLINT_HAWK: &str = "Flying\nWhen this creature enters, sacrifice it unless you \
    return an artifact you control to its owner's hand.";

/// Synthetic building-block card (not a printed card — this Oracle grammar
/// generalizes the "unless you return ... from your graveyard" shape, and no
/// printed card happens to omit the possessive) exercising an UNQUALIFIED
/// source zone: "a graveyard" names no owner, unlike Harvest Wurm's "your
/// graveyard". Per `parse_zone_suffix` (`oracle_target.rs`), a bare/
/// indefinite zone phrase carries no ownership restriction at all — any
/// player's graveyard is eligible.
const UNQUALIFIED_GRAVEYARD_RETURN: &str = "When ~ enters, sacrifice it \
    unless you return a card from a graveyard to your hand.";

struct Board {
    runner: GameRunner,
    drake: ObjectId,
}

/// Cast Drake Familiar with `enchantment` already on the battlefield, and run
/// the ETB trigger up to the `UnlessPayment` decision (CR 118.12a).
fn cast_drake_and_reach_payment_prompt(
    enchantment_controller: engine::types::player::PlayerId,
) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(enchantment_controller, "Test Enchantment", 0, 0)
        .as_enchantment();

    let drake = scenario
        .add_creature_to_hand_from_oracle(P0, "Drake Familiar", 2, 2, DRAKE_FAMILIAR)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(drake).resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player, .. } if player == P0
        ),
        "Drake Familiar's ETB must offer the unless-payment to its controller, got {:?}",
        runner.state().waiting_for
    );

    Board { runner, drake }
}

/// CR 118.12a: Paying the cost with an OPPONENT-CONTROLLED enchantment must
/// succeed — Drake Familiar's clause names no "you control" restriction. This
/// is the reported bug: before the fix, the parser's forced controller scope
/// (and the runtime's redundant `obj.controller == player` check) made this
/// enchantment ineligible, so the payment always failed and Drake sacrificed
/// itself regardless.
#[test]
fn drake_familiar_may_return_an_opponents_enchantment() {
    let mut board = cast_drake_and_reach_payment_prompt(P1);
    let enchantment = *board
        .runner
        .state()
        .battlefield
        .iter()
        .find(|id| board.runner.state().objects[id].name == "Test Enchantment")
        .expect("the opponent's enchantment must be on the battlefield");

    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the controller may choose to pay");

    match &board.runner.state().waiting_for {
        WaitingFor::UnlessBounceChoice { permanents, .. } => {
            assert!(
                permanents.contains(&enchantment),
                "the opponent's enchantment must be offered — Drake Familiar's \
                 clause has no controller restriction, got {:?}",
                permanents
            );
        }
        other => panic!("expected UnlessBounceChoice, got {:?}", other),
    }

    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![enchantment],
        })
        .expect("returning the opponent's enchantment pays the unless-cost");

    assert_eq!(
        board.runner.state().objects[&enchantment].zone,
        Zone::Hand,
        "the returned enchantment must reach its owner's hand"
    );
    assert_eq!(
        board.runner.state().objects[&board.drake].zone,
        Zone::Battlefield,
        "Drake Familiar must survive once the unless-cost is paid"
    );
}

/// CR 118.12a: DECLINING the return still sacrifices Drake Familiar, even
/// with an eligible enchantment sitting right there — declining is always a
/// legal choice regardless of what could have been paid.
#[test]
fn drake_familiar_declining_the_return_sacrifices_it() {
    let mut board = cast_drake_and_reach_payment_prompt(P1);

    board
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("the controller may decline the return");

    assert_eq!(
        board.runner.state().objects[&board.drake].zone,
        Zone::Graveyard,
        "declining the unless-cost must sacrifice Drake Familiar"
    );
}

/// CR 118.12a discriminating control group: Glint Hawk's EXPLICIT "you
/// control" restriction must remain enforced. With only an OPPONENT-controlled
/// artifact on the battlefield, the cost is unpayable, so even choosing to pay
/// falls through to the sacrifice — proving the fix did not accidentally erase
/// the controller scope for clauses that actually print "you control".
#[test]
fn glint_hawk_return_cost_still_requires_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P1, "Opponent's Artifact", 0, 0)
        .as_artifact();

    let glint_hawk = scenario
        .add_creature_to_hand_from_oracle(P0, "Glint Hawk", 2, 2, GLINT_HAWK)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(glint_hawk).resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player, .. } if player == P0
        ),
        "Glint Hawk's ETB must offer the unless-payment, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing to pay is always a legal action");

    assert_eq!(
        runner.state().objects[&glint_hawk].zone,
        Zone::Graveyard,
        "with no controlled artifact available, the cost is unpayable and \
         Glint Hawk must be sacrificed even though its controller chose to pay"
    );
}

/// CR 118.12a: the explicit-control sibling must accept a controlled artifact.
#[test]
fn glint_hawk_can_pay_with_a_controlled_artifact() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_creature(P0, "Controlled Artifact", 0, 0)
        .as_artifact()
        .id();
    let hawk = scenario
        .add_creature_to_hand_from_oracle(P0, "Glint Hawk", 2, 2, GLINT_HAWK)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(hawk).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessPayment { player: P0, .. }
    ));
    runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::UnlessBounceChoice { permanents, .. } if permanents.contains(&artifact))
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![artifact],
        })
        .unwrap();
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&hawk].zone, Zone::Battlefield);
}

/// CR 108.4 + CR 118.12a: a parsed graveyard return uses object ownership.
#[test]
fn harvest_wurm_can_pay_with_an_owned_graveyard_basic_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let opponent_land = scenario.add_basic_land(P1, ManaColor::Green);
    let wurm = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Harvest Wurm",
            3,
            2,
            "When this creature enters, sacrifice it unless you return a basic land card from your graveyard to your hand.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let mut events = Vec::new();
    for id in [land, opponent_land] {
        engine::game::zones::move_to_zone(runner.state_mut(), id, Zone::Graveyard, &mut events);
    }
    runner.cast(wurm).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessPayment { player: P0, .. }
    ));
    runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    let WaitingFor::UnlessBounceChoice { permanents, .. } = &runner.state().waiting_for else {
        panic!("the parsed owned graveyard filter must offer payment");
    };
    assert!(permanents.contains(&land));
    assert!(!permanents.contains(&opponent_land));
    runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .unwrap();
    assert_eq!(runner.state().objects[&land].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&opponent_land].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&wurm].zone, Zone::Battlefield);
}

/// CR 118.12a: An UNQUALIFIED source zone ("a graveyard", no possessive)
/// carries no ownership restriction, so a card in the OPPONENT's graveyard
/// must be an eligible payment — even though `from_zone` alone can't tell the
/// runtime zone scan whose graveyard it's scanning. This is the production
/// counterpart to `unless_return_to_hand_from_graveyard_collects_graveyard_
/// cards` (which only ever staged the payer's own graveyard, so it could not
/// have caught this): before the fix, the parser force-added
/// `FilterProp::Owned{You}` to every zone-qualified filter, and the runtime
/// additionally pre-restricted the zone scan to the payer's own graveyard —
/// either bug alone would have kept the opponent's card ineligible.
#[test]
fn unqualified_graveyard_return_may_use_any_players_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let opponents_card = scenario
        .add_creature_to_graveyard(P1, "Opponent's Graveyard Card", 1, 1)
        .id();

    let creature = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Unqualified Graveyard Return Test",
            2,
            2,
            UNQUALIFIED_GRAVEYARD_RETURN,
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(creature).resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player, .. } if player == P0
        ),
        "the ETB must offer the unless-payment, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the controller may choose to pay");

    match &runner.state().waiting_for {
        WaitingFor::UnlessBounceChoice { permanents, .. } => {
            assert!(
                permanents.contains(&opponents_card),
                "a card in the OPPONENT's graveyard must be offered — the \
                 unqualified zone phrase names no owner, got {:?}",
                permanents
            );
        }
        other => panic!("expected UnlessBounceChoice, got {:?}", other),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![opponents_card],
        })
        .expect("returning the opponent's graveyard card pays the unless-cost");

    assert_eq!(
        runner.state().objects[&opponents_card].zone,
        Zone::Hand,
        "the returned card must reach its owner's hand"
    );
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "the creature must survive once the unless-cost is paid"
    );
}
