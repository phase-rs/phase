//! Regression test for issue #4271: Birthing Ritual must offer creatures up to
//! mana value (sacrificed creature's MV + 1), not always mana value 1.
//!
//! Previously, the chain ordered the dig choice BEFORE the sacrifice, so when
//! `selectable_cards` was computed, `effect_context_object` (the sacrificed
//! creature snapshot) was None. `ObjectScope::CostPaidObject` fell back to
//! `unwrap_or(0)`, yielding X = 0 + 1 = 1.
//!
//! Fixed in #4289: the sacrifice now precedes the PriorLook Dig in the chain.
//! When the filter is evaluated, `effect_context_object` holds the sacrificed
//! creature's snapshot, so X = sacrificed_mv + 1 and the player can choose any
//! creature with mana value ≤ X from among the looked-at cards.
//!
//! https://github.com/phase-rs/phase/issues/4271
//!
//! Issue #7163: with two or more creatures to choose from, the sacrifice paused
//! on an `EffectZoneChoice`, and answering it ended the controller's private
//! look at the seven cards (CR 701.20e + CR 701.20a), so the resumed "from among
//! those cards" Dig read an empty set and the trigger ended without a choice.
//! The look now survives every action boundary while a parked continuation
//! still holds a `DigSource::PriorLook` reader. Separately, "put the rest on the
//! bottom of your library in a random order" now reaches the PriorLook Digs that
//! actually dispose of the looked-at cards.
//!
//! https://github.com/phase-rs/phase/issues/7163

use std::collections::HashSet;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{DigRestOrder, EffectKind};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BIRTHING_RITUAL_ORACLE: &str = "At the beginning of your end step, if you control a creature, look at the top seven cards of your library. Then you may sacrifice a creature. If you do, you may put a creature card with mana value X or less from among those cards onto the battlefield, where X is 1 plus the sacrificed creature's mana value. Put the rest on the bottom of your library in a random order.";

const PREFERRED_SELECTION_ORACLE: &str = "At the beginning of your upkeep, look at the top two cards of your library. You may sacrifice this enchantment and pay {2}{G}{G}. If you do, put one of those cards into your hand. If you don't, put one of those cards on the bottom of your library.";

/// Drive the game to the End step phase, waiting for Birthing Ritual's
/// beginning-of-end-step trigger to appear on the stack.
fn reach_end_step_with_trigger(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("empty attack");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => return,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            _ if runner.state().phase == Phase::End => return,
            _ => runner.pass_both_players(),
        }
    }
}

/// Drive the game from End-step priority (with Birthing Ritual's trigger on
/// the stack) through accepting the optional sacrifice and the sacrifice
/// resolution, stopping just before or at `WaitingFor::DigChoice`.
///
/// Returns the `WaitingFor` state at the point when `DigChoice` is reached
/// (or panics if it isn't reached within the iteration budget).
fn drive_to_dig_choice(runner: &mut GameRunner) -> WaitingFor {
    for _ in 0..50 {
        match runner.state().waiting_for.clone() {
            // Already at DigChoice — done.
            WaitingFor::DigChoice { .. } => return runner.state().waiting_for.clone(),
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare empty attackers");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            // "you may sacrifice a creature" — accept.
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept sacrifice");
            }
            // "choose a creature to sacrifice" — pick the first eligible.
            // With only one eligible creature this prompt may be skipped.
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Sacrifice,
                cards,
                ..
            } => {
                let victim = cards
                    .first()
                    .copied()
                    .expect("at least one eligible creature");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![victim],
                    })
                    .expect("sacrifice creature");
            }
            _ => {
                runner.act(GameAction::PassPriority).ok();
            }
        }
    }
    panic!(
        "never reached WaitingFor::DigChoice; last state = {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn birthing_ritual_cmc_filter_uses_sacrificed_creature_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Birthing Ritual as a permanent enchantment.
    let _ritual = scenario
        .add_creature(P0, "Birthing Ritual", 0, 0)
        .as_enchantment()
        .from_oracle_text(BIRTHING_RITUAL_ORACLE)
        .id();

    // The creature P0 will sacrifice: mana value 3.
    // X = 3 + 1 = 4, so the filter must be CmcLE(4).
    let _victim = scenario
        .add_creature(P0, "Victim Creature", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    // Top-7 library cards (added last = ends up on top):
    //   position 0 (top): MV5 Dragon   — creature, MV 5 > 4 → NOT selectable
    //   position 1:       MV4 Angel    — creature, MV 4 ≤ 4 → selectable
    //   positions 2-6:    non-creature fillers (not selectable regardless)
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Library Filler {i}"));
    }
    let mv4_angel = scenario
        .add_spell_to_library_top(P0, "MV4 Angel", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mv5_dragon = scenario
        .add_spell_to_library_top(P0, "MV5 Dragon", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(5))
        .id();

    let mut runner = scenario.build();

    reach_end_step_with_trigger(&mut runner);
    assert_eq!(runner.state().phase, Phase::End, "must reach the end step");

    let dig_choice = drive_to_dig_choice(&mut runner);

    // The DigChoice must offer only the MV4 Angel; the MV5 Dragon exceeds
    // the bound X = (sacrificed MV 3) + 1 = 4.
    let WaitingFor::DigChoice {
        selectable_cards,
        cards,
        ..
    } = dig_choice
    else {
        panic!("expected DigChoice, got {dig_choice:?}");
    };

    assert_eq!(
        cards.len(),
        7,
        "all 7 looked-at cards must appear in the DigChoice"
    );
    assert!(
        selectable_cards.contains(&mv4_angel),
        "MV4 Angel (MV 4 ≤ X=4) must be selectable; selectable = {selectable_cards:?}"
    );
    assert!(
        !selectable_cards.contains(&mv5_dragon),
        "MV5 Dragon (MV 5 > X=4) must NOT be selectable; selectable = {selectable_cards:?}"
    );
    assert_eq!(
        selectable_cards.len(),
        1,
        "exactly one card (MV4 Angel) must be selectable; selectable = {selectable_cards:?}"
    );
}

// ---------------------------------------------------------------------------
// Issue #7163: multi-candidate sacrifice, remainder disposition, look window.
// ---------------------------------------------------------------------------

/// Birthing Ritual board for the #7163 rows. P0 controls the enchantment and
/// the given sacrifice candidates; P1 controls a higher-MV creature that must
/// never be a sacrifice candidate. P0's library, top first: Dragon, Angel, Elf,
/// four non-creature fillers (the seven looked-at cards), then two deep cards
/// (a creature and a filler) the look must never reach.
struct RitualFixture {
    runner: GameRunner,
    ritual: ObjectId,
    candidates: Vec<ObjectId>,
    opponent_creature: ObjectId,
    elf: ObjectId,
    angel: ObjectId,
    dragon: ObjectId,
    top_seven: Vec<ObjectId>,
    deep: Vec<ObjectId>,
}

/// Default looked-at creature mana values: Elf 2, Angel 4, Dragon 5.
const DEFAULT_LOOKED_AT_MVS: [u32; 3] = [2, 4, 5];

/// Build the board. `candidates` are P0's creatures as `(name, mana value)`;
/// `[elf_mv, angel_mv, dragon_mv]` re-costs the three looked-at creatures.
fn build_ritual_fixture(candidates: &[(&str, u32)], looked_at_mvs: [u32; 3]) -> RitualFixture {
    let [elf_mv, angel_mv, dragon_mv] = looked_at_mvs;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ritual = scenario
        .add_enchantment_from_oracle(P0, "Birthing Ritual", BIRTHING_RITUAL_ORACLE)
        .id();
    let candidates = candidates
        .iter()
        .map(|&(name, mv)| {
            scenario
                .add_creature(P0, name, 2, 2)
                .with_mana_cost(ManaCost::generic(mv))
                .id()
        })
        .collect();
    let opponent_creature = scenario
        .add_creature(P1, "Opp Creature", 6, 6)
        .with_mana_cost(ManaCost::generic(6))
        .id();

    // Staged bottom-up: each call puts the card on top of the library.
    let deep_bear = scenario
        .add_spell_to_library_top(P0, "Deep Bear", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let deep_filler = scenario.add_card_to_library_top(P0, "Deep Filler");
    let mut top_seven: Vec<ObjectId> = (0..4)
        .map(|i| scenario.add_card_to_library_top(P0, &format!("Look Filler {i}")))
        .collect();
    let mut library_creature = |name: &str, mv: u32| {
        scenario
            .add_spell_to_library_top(P0, name, false)
            .as_creature()
            .with_mana_cost(ManaCost::generic(mv))
            .id()
    };
    let elf = library_creature("Look Elf", elf_mv);
    let angel = library_creature("Look Angel", angel_mv);
    let dragon = library_creature("Look Dragon", dragon_mv);
    top_seven.extend([elf, angel, dragon]);

    RitualFixture {
        runner: scenario.build(),
        ritual,
        candidates,
        opponent_creature,
        elf,
        angel,
        dragon,
        top_seven,
        deep: vec![deep_bear, deep_filler],
    }
}

fn id_set<'a>(ids: impl IntoIterator<Item = &'a ObjectId>) -> HashSet<ObjectId> {
    ids.into_iter().copied().collect()
}

fn library(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().library_of(player).iter().copied().collect()
}

/// Reach the end step and pass priority until Birthing Ritual's trigger has
/// resolved its look and asks "you may sacrifice a creature". Reach guard: the
/// controller is privately looking at exactly the top seven cards.
fn reach_ritual_sacrifice_offer(fx: &mut RitualFixture) {
    reach_end_step_with_trigger(&mut fx.runner);
    for _ in 0..8 {
        match fx.runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                fx.runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the trigger");
            }
            ref other => panic!("unexpected prompt before the sacrifice offer: {other:?}"),
        }
    }
    assert!(
        matches!(
            fx.runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "must reach the optional sacrifice; got {:?}",
        fx.runner.state().waiting_for
    );
    assert_eq!(
        id_set(&fx.runner.state().private_look_ids),
        id_set(&fx.top_seven),
        "the controller privately looks at exactly the top seven cards"
    );
    assert_eq!(fx.runner.state().private_look_player, Some(P0));
}

/// Accept the sacrifice and, at the multi-candidate `EffectZoneChoice`, pick
/// `victim`. Asserts the prompt offers exactly P0's creatures.
fn accept_and_sacrifice(fx: &mut RitualFixture, victim: ObjectId) {
    fx.runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the sacrifice");
    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count,
        effect_kind: EffectKind::Sacrifice,
        ..
    } = fx.runner.state().waiting_for.clone()
    else {
        panic!(
            "multiple candidates must open a sacrifice EffectZoneChoice; got {:?}",
            fx.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(count, 1);
    assert_eq!(
        id_set(&cards),
        id_set(&fx.candidates),
        "every creature P0 controls (and nothing else) is a sacrifice candidate"
    );
    assert!(!cards.contains(&fx.ritual) && !cards.contains(&fx.opponent_creature));
    assert_eq!(
        fx.runner.state().private_look_ids.len(),
        7,
        "the look is still open while the sacrifice is being chosen"
    );
    fx.runner
        .act(GameAction::SelectCards {
            cards: vec![victim],
        })
        .expect("sacrifice the chosen creature");
}

fn assert_at_empty_priority(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the trigger must finish resolving; got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "the trigger has resolved");
}

/// The deep cards are now on top and exactly `expected_rest` sits beneath them.
fn assert_rest_on_bottom(fx: &RitualFixture, expected_rest: &HashSet<ObjectId>) {
    let lib = library(&fx.runner, P0);
    assert_eq!(lib.len(), 2 + expected_rest.len(), "library = {lib:?}");
    assert_eq!(
        id_set(&lib[..2]),
        id_set(&fx.deep),
        "the never-looked-at cards are now on top"
    );
    assert_eq!(
        id_set(&lib[2..]),
        *expected_rest,
        "the rest of the looked-at cards are on the bottom"
    );
}

fn assert_look_cleared(runner: &GameRunner) {
    assert!(runner.state().private_look_ids.is_empty());
    assert!(runner.state().private_look_player.is_none());
}

/// CR 701.20e + CR 701.20a + CR 608.2c (#7163 R1): with several sacrifice
/// candidates, answering the sacrifice `EffectZoneChoice` must not end the
/// private look — the parked "from among those cards" Dig still reads it.
/// Sacrificing the MV3 creature sets X = 4 from its last known information
/// (CR 608.2h): Elf (2) and Angel (4) qualify; Dragon (5) does not.
#[test]
fn birthing_ritual_multi_candidate_sacrifice_keeps_looked_at_cards() {
    let mut fx = build_ritual_fixture(
        &[
            ("Small Victim", 1),
            ("Big Victim", 3),
            ("Tall Bystander", 4),
        ],
        DEFAULT_LOOKED_AT_MVS,
    );
    let big = fx.candidates[1];
    reach_ritual_sacrifice_offer(&mut fx);
    accept_and_sacrifice(&mut fx, big);

    let WaitingFor::DigChoice {
        player,
        cards,
        keep_count,
        up_to,
        selectable_cards,
        kept_destination,
        rest_destination,
        ..
    } = fx.runner.state().waiting_for.clone()
    else {
        panic!(
            "after the sacrifice the controller chooses from among the looked-at cards; got {:?}",
            fx.runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(id_set(&cards), id_set(&fx.top_seven));
    assert_eq!(
        id_set(&selectable_cards),
        id_set(&[fx.elf, fx.angel]),
        "X = 1 + 3 admits Elf (2) and Angel (4) only"
    );
    assert!(!selectable_cards.contains(&fx.dragon));
    assert!(!selectable_cards.contains(&fx.deep[0]));
    assert_eq!(keep_count, 1);
    assert!(up_to);
    assert_eq!(kept_destination, Some(Zone::Battlefield));
    assert_eq!(rest_destination, Some(Zone::Library));
    assert_eq!(fx.runner.state().objects[&big].zone, Zone::Graveyard);
}

/// #7163 R2: the chosen card enters the battlefield and every other looked-at
/// card goes to the bottom; the look window then ends.
#[test]
fn birthing_ritual_multi_candidate_puts_chosen_card_and_rest_on_bottom() {
    let mut fx = build_ritual_fixture(
        &[
            ("Small Victim", 1),
            ("Big Victim", 3),
            ("Tall Bystander", 4),
        ],
        DEFAULT_LOOKED_AT_MVS,
    );
    let (small, big, bystander) = (fx.candidates[0], fx.candidates[1], fx.candidates[2]);
    reach_ritual_sacrifice_offer(&mut fx);
    accept_and_sacrifice(&mut fx, big);
    assert!(
        matches!(fx.runner.state().waiting_for, WaitingFor::DigChoice { .. }),
        "reach guard: DigChoice; got {:?}",
        fx.runner.state().waiting_for
    );

    fx.runner
        .act(GameAction::SelectCards {
            cards: vec![fx.angel],
        })
        .expect("put the Angel onto the battlefield");

    assert_at_empty_priority(&fx.runner);
    let objects = &fx.runner.state().objects;
    assert_eq!(objects[&fx.angel].zone, Zone::Battlefield);
    assert_eq!(objects[&big].zone, Zone::Graveyard);
    assert_eq!(objects[&small].zone, Zone::Battlefield);
    assert_eq!(objects[&bystander].zone, Zone::Battlefield);
    let mut rest = id_set(&fx.top_seven);
    rest.remove(&fx.angel);
    assert_rest_on_bottom(&fx, &rest);
    assert_look_cleared(&fx.runner);
}

/// #7163 R3: "Put the rest on the bottom of your library in a random order"
/// reaches the PriorLook choice Dig that disposes of the looked-at cards.
#[test]
fn birthing_ritual_choice_puts_rest_in_random_order() {
    let mut fx = build_ritual_fixture(
        &[
            ("Small Victim", 1),
            ("Big Victim", 3),
            ("Tall Bystander", 4),
        ],
        DEFAULT_LOOKED_AT_MVS,
    );
    let big = fx.candidates[1];
    reach_ritual_sacrifice_offer(&mut fx);
    accept_and_sacrifice(&mut fx, big);
    let WaitingFor::DigChoice { rest_order, .. } = fx.runner.state().waiting_for.clone() else {
        panic!(
            "reach guard: DigChoice; got {:?}",
            fx.runner.state().waiting_for
        );
    };
    assert_eq!(rest_order, DigRestOrder::Random);
}

/// #7163 R4: declining the sacrifice puts all seven looked-at cards on the
/// bottom with no further prompt.
#[test]
fn birthing_ritual_decline_puts_all_seven_on_bottom() {
    let mut fx = build_ritual_fixture(
        &[
            ("Small Victim", 1),
            ("Big Victim", 3),
            ("Tall Bystander", 4),
        ],
        DEFAULT_LOOKED_AT_MVS,
    );
    reach_ritual_sacrifice_offer(&mut fx);
    fx.runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the sacrifice");

    assert_at_empty_priority(&fx.runner);
    for &creature in &fx.candidates {
        assert_eq!(fx.runner.state().objects[&creature].zone, Zone::Battlefield);
    }
    let top_seven = id_set(&fx.top_seven);
    assert_rest_on_bottom(&fx, &top_seven);
    assert_look_cleared(&fx.runner);
}

/// #7163 R5: a single candidate is sacrificed without a prompt and the choice
/// follows directly, with a random-order remainder.
#[test]
fn birthing_ritual_single_candidate_goes_straight_to_choice() {
    let mut fx = build_ritual_fixture(&[("Lone Victim", 3)], DEFAULT_LOOKED_AT_MVS);
    let lone = fx.candidates[0];
    reach_ritual_sacrifice_offer(&mut fx);
    fx.runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the sacrifice");

    let WaitingFor::DigChoice {
        selectable_cards,
        rest_order,
        ..
    } = fx.runner.state().waiting_for.clone()
    else {
        panic!(
            "one candidate goes straight to the choice (no EffectZoneChoice); got {:?}",
            fx.runner.state().waiting_for
        );
    };
    assert_eq!(fx.runner.state().objects[&lone].zone, Zone::Graveyard);
    assert_eq!(id_set(&selectable_cards), id_set(&[fx.elf, fx.angel]));
    assert_eq!(rest_order, DigRestOrder::Random);
}

/// #7163 R6: accepting the sacrifice and then putting nothing leaves all seven
/// looked-at cards for the bottom.
#[test]
fn birthing_ritual_sacrifice_then_put_nothing_puts_all_seven_on_bottom() {
    let mut fx = build_ritual_fixture(
        &[("Small Victim", 1), ("Big Victim", 3)],
        DEFAULT_LOOKED_AT_MVS,
    );
    let big = fx.candidates[1];
    reach_ritual_sacrifice_offer(&mut fx);
    accept_and_sacrifice(&mut fx, big);
    assert!(
        matches!(fx.runner.state().waiting_for, WaitingFor::DigChoice { .. }),
        "reach guard: DigChoice; got {:?}",
        fx.runner.state().waiting_for
    );
    assert_eq!(fx.runner.state().private_look_ids.len(), 7);

    fx.runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("put nothing onto the battlefield");

    assert_at_empty_priority(&fx.runner);
    assert_eq!(fx.runner.state().objects[&big].zone, Zone::Graveyard);
    let top_seven = id_set(&fx.top_seven);
    assert_rest_on_bottom(&fx, &top_seven);
    assert_look_cleared(&fx.runner);
}

/// #7163 R7: sacrificing the MV1 creature sets X = 2 and none of the looked-at
/// creatures qualifies (Elf 6, Angel 7, Dragon 5), so no choice is offered and
/// all seven still go to the bottom.
#[test]
fn birthing_ritual_sacrifice_with_no_qualifying_card_puts_all_seven_on_bottom() {
    let mut fx = build_ritual_fixture(&[("Small Victim", 1), ("Big Victim", 3)], [6, 7, 5]);
    let small = fx.candidates[0];
    reach_ritual_sacrifice_offer(&mut fx);
    accept_and_sacrifice(&mut fx, small);

    assert!(
        !matches!(fx.runner.state().waiting_for, WaitingFor::DigChoice { .. }),
        "no looked-at creature qualifies for X = 2"
    );
    assert_at_empty_priority(&fx.runner);
    assert_eq!(fx.runner.state().objects[&small].zone, Zone::Graveyard);
    let top_seven = id_set(&fx.top_seven);
    assert_rest_on_bottom(&fx, &top_seven);
    assert_look_cleared(&fx.runner);
}

/// Advance turn by turn to `player`'s next upkeep, declaring no attackers when
/// a declare-attackers step asks for them.
fn advance_to_upkeep_of(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..8 {
        runner.advance_to_upkeep();
        if runner.state().phase == Phase::Upkeep && runner.state().active_player == player {
            return;
        }
        if runner.waiting_for_kind() == "DeclareAttackers" {
            runner.declare_attackers(&[]).expect("declare no attackers");
        } else {
            runner.advance_to_end_step();
        }
    }
    panic!("did not reach {player:?}'s upkeep");
}

/// CR 701.20e + CR 701.20a (#7163 R8): the look window is kept only while a
/// parked continuation still holds a PriorLook reader. Answering a PriorLook
/// `DigChoice` whose parked tail reads nothing from the look ends the window.
#[test]
fn private_look_ends_when_no_parked_prior_look_reader_remains() {
    // Depends on Preferred Selection's current misparse: the parked
    // `PayCost → PutAtLibraryPosition` tail exists only because of it; when that
    // misparse is fixed, move this row to a durable fixture.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Preferred Selection", PREFERRED_SELECTION_ORACLE);
    scenario.with_library_top(
        P1,
        &["P1 Filler 0", "P1 Filler 1", "P1 Filler 2", "P1 Filler 3"],
    );
    scenario.with_library_top(
        P0,
        &["Top Card 0", "Top Card 1", "Deep 0", "Deep 1", "Deep 2"],
    );
    let mut runner = scenario.build();
    advance_to_upkeep_of(&mut runner, P0);

    for _ in 0..8 {
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the trigger");
            }
            ref other => panic!("unexpected prompt before the optional cost: {other:?}"),
        }
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the optional cost");

    let WaitingFor::DigChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "reach guard: PriorLook DigChoice; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(runner.state().private_look_ids.len(), 2);
    assert!(
        runner.state().active_ability_continuation().is_some(),
        "reach guard: a continuation is parked behind the DigChoice"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("choose one of the looked-at cards");

    assert_look_cleared(&runner);
}
