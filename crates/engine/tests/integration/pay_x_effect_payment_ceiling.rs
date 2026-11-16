//! Resolution-time pay-{X} offers the mana the payer can really produce.
//!
//! Electro, Assaulting Battery: "When Electro leaves the battlefield, you may
//! pay {X}. When you do, he deals X damage to target player." The `{X}` prompt
//! (`PayableResource::Mana { composition: ConcretizeX }`) is bounded by
//! `casting::effect_payment_ceiling` (the simulated effect-context auto-tap and
//! inline-trigger pipeline, floored by the announcement-time `max_x_value`
//! estimate) and then gated by the exact affordability check, so replacement
//! multipliers (Nyxbloom Ancient, CR 106.12b) and bonus mana (Fertile Ground,
//! CR 605.1b) are offered and the offer is really payable.
//!
//! Every row drives the real stack: P0 casts Vendetta (paid from a seeded
//! floating {B}; a Mountain never produces {B}) on its own Electro. Casting an
//! instant triggers Electro's "add {R}", Vendetta destroys Electro, and the
//! leaves-the-battlefield trigger reaches the optional `{X}` payment.
//!
//! Electro and Vendetta are absent from the integration card fixture, so they
//! are built from their verbatim Oracle text by the real parser; every other
//! card is the printed card.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, ManaAmountComposition, PayableResource, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const ELECTRO: &str = "Flying\nYou don't lose unspent red mana as steps and phases end.\nWhenever you cast an instant or sorcery spell, add {R}.\nWhen Electro leaves the battlefield, you may pay {X}. When you do, he deals X damage to target player.";
const VENDETTA: &str = "Destroy target nonblack creature. It can't be regenerated. You lose life equal to that creature's toughness.";

/// Electro's printed toughness: Vendetta's life loss reads it.
const ELECTRO_TOUGHNESS: i32 = 3;

struct Board {
    runner: GameRunner,
    /// Parallel to the `cards` passed to [`electro_board`].
    ids: Vec<ObjectId>,
}

/// P0 controls Electro ({1}{R}{R} 2/3) plus `cards` ("Mountain" is the scenario
/// basic, anything else is the printed card), holds Vendetta ({B}) and has a
/// single floating {B}. Vendetta is committed to the stack targeting Electro.
/// Fertile Ground is attached to P0's first Mountain before Vendetta is cast
/// (CR 704.5m: an unattached Aura would go to the graveyard). Returns `None`
/// without a card database.
fn electro_board(cards: &[&str]) -> Option<Board> {
    let db = shared_card_db()?;
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let electro = scenario
        .add_creature_from_oracle(P0, "Electro, Assaulting Battery", 2, 3, ELECTRO)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 1,
        })
        .id();
    let vendetta = scenario
        .add_spell_to_hand_from_oracle(P0, "Vendetta", true, VENDETTA)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    let ids: Vec<ObjectId> = cards
        .iter()
        .map(|&name| match name {
            "Mountain" => scenario.add_basic_land(P0, ManaColor::Red),
            _ => scenario.add_real_card(P0, name, Zone::Battlefield, db),
        })
        .collect();
    let mut runner = scenario.build();
    let first_mountain = cards
        .iter()
        .zip(&ids)
        .find(|(name, _)| **name == "Mountain")
        .map(|(_, &id)| id);
    let state = runner.state_mut();
    for (&name, &id) in cards.iter().zip(&ids) {
        if name == "Fertile Ground" {
            let land = first_mountain.expect("Fertile Ground needs a Mountain");
            state.objects.get_mut(&id).expect("the Aura").attached_to =
                Some(AttachTarget::Object(land));
            state
                .objects
                .get_mut(&land)
                .expect("the Mountain")
                .attachments
                .push(id);
        }
    }
    state.layers_dirty.mark_full();
    runner.cast(vendetta).target_object(electro).commit();
    Some(Board { runner, ids })
}

fn pool_total(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .map_or(0, |p| p.mana_pool.total())
}

fn pool_red(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .map_or(0, |p| p.mana_pool.count_color(ManaType::Red))
}

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

/// Passes priority until Vendetta resolves, Electro leaves, and its optional
/// `{X}` payment is accepted; returns the prompt's `max`.
fn drive_to_x_prompt(runner: &mut GameRunner) -> u32 {
    let mut decided = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(player, P0, "the optional payment belongs to the controller");
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional payment");
                decided = true;
            }
            WaitingFor::PayAmountChoice {
                player,
                resource,
                min,
                max,
                ..
            } => {
                assert_eq!(player, P0);
                assert!(
                    matches!(
                        resource,
                        PayableResource::Mana {
                            composition: ManaAmountComposition::ConcretizeX,
                            ..
                        }
                    ),
                    "the prompt pays {{X}}: {resource:?}"
                );
                assert_eq!(min, 0, "CR 107.1c: zero is a legal amount");
                return max;
            }
            WaitingFor::Priority { .. } => {
                assert!(
                    decided || !runner.state().stack.is_empty(),
                    "the stack emptied without Electro's payment decision"
                );
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt before the payment: {other:?}"),
        }
    }
    panic!("Electro's leave trigger never reached its {{X}} payment");
}

/// Reach guards at the `{X}` prompt: Vendetta resolved (Electro is in the
/// graveyard and P0 lost its toughness), Vendetta's {B} was spent, and the
/// only floating mana is Electro's cast-trigger {R} (CR 500.5: nothing has
/// emptied the pool since).
fn assert_reached_x_prompt(board: &Board) {
    let runner = &board.runner;
    assert_eq!(runner.life(P0), 20 - ELECTRO_TOUGHNESS, "Vendetta resolved");
    assert_eq!(
        pool_red(runner),
        1,
        "Electro's cast trigger added one red; Vendetta's {{B}} was spent"
    );
    assert_eq!(
        pool_total(runner),
        1,
        "nothing but the trigger's red is floating"
    );
}

/// The offer is payable as promised: one above `max` is outside the legal
/// range, `max` itself is accepted, the "When you do" damage targets P1 and
/// deals exactly `max`, every produced mana was spent, and Priority returns
/// with an empty stack.
fn assert_x_offer_is_payable(runner: &mut GameRunner, max: u32) {
    let p1_life_before = runner.life(P1);
    assert!(
        runner
            .act(GameAction::SubmitPayAmount { amount: max + 1 })
            .is_err(),
        "{} is outside the legal range [0, {max}]",
        max + 1
    );
    runner
        .act(GameAction::SubmitPayAmount { amount: max })
        .expect("the offered maximum is accepted");
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("choose the target player");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt after the payment: {other:?}"),
        }
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.life(P1),
        p1_life_before - i32::try_from(max).expect("small max"),
        "Electro deals X damage, X being the amount paid"
    );
    assert_eq!(
        pool_total(runner),
        0,
        "the maximum is the whole payable mana, so none is left floating"
    );
}

/// Row E0 (sibling of E1, no multiplier): the trigger's floating {R} plus a
/// Mountain's {R} make 2. Passes without the simulated ceiling.
#[test]
fn electro_pay_x_mountain_offers_pool_plus_land() {
    let Some(mut board) = electro_board(&["Mountain"]) else {
        return;
    };
    let max = drive_to_x_prompt(&mut board.runner);
    assert_reached_x_prompt(&board);
    assert!(
        !tapped(&board.runner, board.ids[0]),
        "reach: the Mountain is untapped"
    );
    assert_eq!(max, 2, "1 floating {{R}} plus the Mountain's {{R}}");
    assert_x_offer_is_payable(&mut board.runner, max);
    assert!(
        tapped(&board.runner, board.ids[0]),
        "the Mountain was tapped to pay"
    );
}

/// Row E1: CR 106.12b, Nyxbloom Ancient triples the Mountain's mana, so the
/// offer is the floating {R} plus RRR. Red before the simulated ceiling: the
/// announcement-time estimate counts the Mountain once.
#[test]
fn electro_pay_x_counts_nyxbloom_tripling() {
    let Some(mut board) = electro_board(&["Nyxbloom Ancient", "Mountain"]) else {
        return;
    };
    let max = drive_to_x_prompt(&mut board.runner);
    assert_reached_x_prompt(&board);
    let nyxbloom = &board.runner.state().objects[&board.ids[0]];
    assert_eq!(nyxbloom.zone, Zone::Battlefield);
    assert!(
        !nyxbloom.replacement_definitions.is_empty(),
        "reach: Nyxbloom carries its ProduceMana replacement"
    );
    assert!(
        !tapped(&board.runner, board.ids[1]),
        "reach: the Mountain is untapped"
    );
    assert!(max >= 4, "1 floating + a tripled Mountain: {max}");
    assert_x_offer_is_payable(&mut board.runner, max);
    assert!(
        tapped(&board.runner, board.ids[1]),
        "the Mountain was tapped to pay"
    );
}

/// Row E2 (negative, pair E1): an opponent's Nyxbloom Ancient does not triple my
/// Mountain ("If you tap a permanent for mana" names its controller), so the
/// offer stays at the floating {R} plus the Mountain's {R}.
#[test]
fn electro_pay_x_ignores_an_opponents_nyxbloom() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let electro = scenario
        .add_creature_from_oracle(P0, "Electro, Assaulting Battery", 2, 3, ELECTRO)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 1,
        })
        .id();
    let vendetta = scenario
        .add_spell_to_hand_from_oracle(P0, "Vendetta", true, VENDETTA)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    let nyxbloom = scenario.add_real_card(P1, "Nyxbloom Ancient", Zone::Battlefield, db);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    runner.cast(vendetta).target_object(electro).commit();

    let max = drive_to_x_prompt(&mut runner);
    let opposing = &runner.state().objects[&nyxbloom];
    assert_eq!(opposing.zone, Zone::Battlefield);
    assert_eq!(opposing.controller, P1, "reach: the opponent controls it");
    assert!(
        !tapped(&runner, mountain),
        "reach: the Mountain is untapped"
    );
    assert_eq!(pool_red(&runner), 1, "reach: the trigger's red is floating");
    assert_eq!(max, 2);
    assert_x_offer_is_payable(&mut runner, max);
}

/// Row E3: Fertile Ground adds one additional mana of any color when the
/// enchanted Mountain is tapped (CR 605.1b), so the offer is the floating {R},
/// the Mountain's {R} and the Aura's mana.
#[test]
fn electro_pay_x_counts_fertile_ground_bonus() {
    let Some(mut board) = electro_board(&["Mountain", "Fertile Ground"]) else {
        return;
    };
    let max = drive_to_x_prompt(&mut board.runner);
    assert_reached_x_prompt(&board);
    assert_eq!(
        board.runner.state().objects[&board.ids[1]].attached_to,
        Some(AttachTarget::Object(board.ids[0])),
        "reach: Fertile Ground is attached to the Mountain"
    );
    assert!(
        !tapped(&board.runner, board.ids[0]),
        "reach: the enchanted Mountain is untapped"
    );
    assert!(max >= 3, "1 floating + Mountain + Aura bonus: {max}");
    assert_x_offer_is_payable(&mut board.runner, max);
    assert!(
        tapped(&board.runner, board.ids[0]),
        "the enchanted Mountain was tapped to pay"
    );
}

/// Announcement-time X is untouched (CR 601.2b): with three Mountains and no
/// multiplier, Fireball ({X}{R}) announces X up to 2, exactly as before the
/// resolution-time ceiling existed. `max_x_value` and spell castability are not
/// changed by this work.
#[test]
fn announcement_time_x_maximum_is_unchanged() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let fireball = scenario.add_real_card(P0, "Fireball", Zone::Hand, db);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&fireball].card_id;
    assert_eq!(
        runner.state().objects[&fireball].zone,
        Zone::Hand,
        "reach: Fireball is in hand"
    );
    runner
        .act(GameAction::CastSpell {
            object_id: fireball,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin casting Fireball");
    let WaitingFor::ChooseXValue {
        player, min, max, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "X is announced first (CR 601.2b), got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(min, 0);
    assert_eq!(max, 2, "{{X}}{{R}} with three Mountains: X is at most 2");
}
