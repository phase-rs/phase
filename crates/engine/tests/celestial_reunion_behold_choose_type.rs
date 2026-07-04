//! Celestial Reunion — behold-choose-a-creature-type COST subsystem + the
//! search→conditional-destination resolution.
//!
//! Oracle: "As an additional cost to cast this spell, you may choose a creature
//! type and behold two creatures of that type. Search your library for a
//! creature card with mana value X or less, reveal it, put it into your hand,
//! then shuffle. If this spell's additional cost was paid and the revealed card
//! is the chosen type, put that card onto the battlefield instead of putting it
//! into your hand."
//!
//! These tests drive the REAL cast pipeline through the new
//! `WaitingFor::CostTypeChoice` round-trip, the behold cost, and the
//! `SearchChoice`-scoped conditional-destination deferral. The destination
//! (battlefield vs hand) is the discriminating assertion:
//!   * Case 1 (positive): cost paid + found card IS the chosen type -> battlefield.
//!   * Case A (type-leg negative): found card is NOT the chosen type -> hand.
//!   * Case B (cost-leg negative): additional cost declined -> hand.
//!
//! Each of the And gate's two legs, the provenance write, and the deferral
//! disjunct independently flips case 1 to hand when reverted.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "As an additional cost to cast this spell, you may choose a creature type \
and behold two creatures of that type.\nSearch your library for a creature card with mana value \
X or less, reveal it, put it into your hand, then shuffle. If this spell's additional cost was \
paid and the revealed card is the chosen type, put that card onto the battlefield instead of \
putting it into your hand.";

fn x_green_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Green],
        generic: 0,
    }
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Add a creature card to P0's library with the given subtype and mana value.
fn add_library_creature(
    runner: &mut engine::game::scenario::GameRunner,
    name: &str,
    subtype: &str,
    cmc: u32,
) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        name.to_string(),
        Zone::Library,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.subtypes.push(subtype.to_string());
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::generic(cmc);
    id
}

struct Setup {
    runner: engine::game::scenario::GameRunner,
    spell: ObjectId,
    behold_elves: Vec<ObjectId>,
    lib_elf: ObjectId,
    lib_goblin: ObjectId,
}

/// Build P0 with: the spell in hand, two Elf creatures in hand (beholdable),
/// and one Elf + one Goblin creature (MV 2) in library. `all_creature_types`
/// is exactly ["Elf","Goblin"].
fn setup() -> Setup {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let elf_a = scenario
        .add_creature_to_hand(P0, "Beholdable Elf A", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let elf_b = scenario
        .add_creature_to_hand(P0, "Beholdable Elf B", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();

    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Celestial Reunion", false, ORACLE);
    builder.with_mana_cost(x_green_cost());
    let spell = builder.id();

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Elf".into(), "Goblin".into()];

    let lib_elf = add_library_creature(&mut runner, "Library Elf", "Elf", 2);
    let lib_goblin = add_library_creature(&mut runner, "Library Goblin", "Goblin", 2);

    // X=3 -> total {3}{G} = 4 green.
    add_mana(&mut runner, 4);

    Setup {
        runner,
        spell,
        behold_elves: vec![elf_a, elf_b],
        lib_elf,
        lib_goblin,
    }
}

/// Drive the full cast. `pay_cost` decides the optional additional cost;
/// `chosen_type` answers `CostTypeChoice`; `found` is the SearchChoice pick
/// (None = whiff/empty). Returns after the stack empties.
fn drive(s: &mut Setup, pay_cost: bool, chosen_type: &str, found: Option<ObjectId>) {
    let card_id = s.runner.state().objects[&s.spell].card_id;
    s.runner
        .act(GameAction::CastSpell {
            object_id: s.spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("casting Celestial Reunion must be accepted");

    for _ in 0..40 {
        match s.runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                s.runner
                    .act(GameAction::ChooseX { value: 3 })
                    .expect("choose X=3");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                s.runner
                    .act(GameAction::DecideOptionalCost { pay: pay_cost })
                    .expect("decide optional behold cost");
            }
            WaitingFor::CostTypeChoice { options, .. } => {
                assert!(
                    options.iter().any(|o| o == chosen_type),
                    "chosen type {chosen_type} must be an offered option: {options:?}"
                );
                s.runner
                    .act(GameAction::ChooseOption {
                        choice: chosen_type.to_string(),
                    })
                    .expect("choose creature type");
            }
            WaitingFor::PayCost { .. } => {
                s.runner
                    .act(GameAction::SelectCards {
                        cards: s.behold_elves.clone(),
                    })
                    .expect("behold the two Elf cards from hand");
            }
            WaitingFor::SearchChoice { .. } => {
                let cards = found.map_or_else(Vec::new, |f| vec![f]);
                s.runner
                    .act(GameAction::SelectCards { cards })
                    .expect("resolve the search selection");
            }
            WaitingFor::Priority { .. } => {
                if s.runner.state().stack.is_empty() {
                    return;
                }
                if s.runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            other => panic!("unexpected prompt during Celestial Reunion cast: {other:?}"),
        }
    }
    panic!("cast pipeline did not settle within the prompt budget");
}

fn zone_of(s: &Setup, id: ObjectId) -> Zone {
    s.runner.state().objects[&id].zone
}

/// Case 1 (positive): pay the cost, choose Elf, find the Elf card ->
/// the found Elf enters the BATTLEFIELD (both And legs true).
#[test]
fn cost_paid_and_found_is_chosen_type_enters_battlefield() {
    let mut s = setup();
    let lib_elf = s.lib_elf;
    drive(&mut s, true, "Elf", Some(lib_elf));
    assert_eq!(
        zone_of(&s, lib_elf),
        Zone::Battlefield,
        "cost paid + found card IS the chosen type (Elf) -> battlefield, not hand"
    );
}

/// Case A (type-leg negative): pay the cost, choose Elf, but find the GOBLIN ->
/// goes to HAND (TargetMatchesFilter leg false). Isolates the chosen-type leg.
#[test]
fn cost_paid_but_found_is_not_chosen_type_goes_to_hand() {
    let mut s = setup();
    let lib_goblin = s.lib_goblin;
    drive(&mut s, true, "Elf", Some(lib_goblin));
    assert_eq!(
        zone_of(&s, lib_goblin),
        Zone::Hand,
        "cost paid but found Goblin (not the chosen Elf) -> hand"
    );
}

/// Case B (cost-leg negative): DECLINE the additional cost, find the Elf ->
/// goes to HAND (AdditionalCostPaid leg false). Isolates the cost leg.
#[test]
fn cost_declined_then_found_chosen_type_still_goes_to_hand() {
    let mut s = setup();
    let lib_elf = s.lib_elf;
    // When declined there is no type choice; pass a dummy chosen_type (unused).
    drive(&mut s, false, "Elf", Some(lib_elf));
    assert_eq!(
        zone_of(&s, lib_elf),
        Zone::Hand,
        "additional cost declined -> found Elf goes to hand even though it is the chosen type"
    );
}

/// Whiff: pay the cost, choose Elf, but select NOTHING at SearchChoice ->
/// no found card is injected, the And's TargetMatchesFilter leg reads empty
/// targets and is false, the else (hand) branch runs with no object, no panic.
#[test]
fn whiff_empty_search_does_not_panic_and_moves_nothing() {
    let mut s = setup();
    let lib_elf = s.lib_elf;
    let lib_goblin = s.lib_goblin;
    drive(&mut s, true, "Elf", None);
    assert_eq!(
        zone_of(&s, lib_elf),
        Zone::Library,
        "no card moved on a whiff"
    );
    assert_eq!(
        zone_of(&s, lib_goblin),
        Zone::Library,
        "no card moved on a whiff"
    );
    assert!(
        s.runner.state().stack.is_empty(),
        "spell resolved cleanly with no found card"
    );
}

/// Feasibility (case 8): with two beholdable Elves and ZERO beholdable Goblins,
/// `CostTypeChoice.options == ["Elf"]` — Goblin is NOT offered. Reverting the
/// option list to the unfiltered `all_creature_types` reintroduces "Goblin".
#[test]
fn cost_type_choice_offers_only_feasible_types() {
    let mut s = setup();
    let card_id = s.runner.state().objects[&s.spell].card_id;
    s.runner
        .act(GameAction::CastSpell {
            object_id: s.spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast accepted");
    // Advance to the CostTypeChoice prompt.
    for _ in 0..10 {
        match s.runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                s.runner.act(GameAction::ChooseX { value: 3 }).unwrap();
            }
            WaitingFor::OptionalCostChoice { .. } => {
                s.runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .unwrap();
            }
            WaitingFor::CostTypeChoice { options, .. } => {
                assert_eq!(
                    options,
                    vec!["Elf".to_string()],
                    "only Elf is feasible (2 beholdable); Goblin (0 beholdable) must be excluded"
                );
                return;
            }
            other => panic!("expected CostTypeChoice, got {other:?}"),
        }
    }
    panic!("never reached CostTypeChoice");
}

/// AI (case 7): `legal_actions` at `CostTypeChoice` returns exactly one
/// `ChooseOption` per FEASIBLE type (mirrors `options`, not the full catalog).
#[test]
fn ai_legal_actions_mirror_feasible_options() {
    let mut s = setup();
    let card_id = s.runner.state().objects[&s.spell].card_id;
    s.runner
        .act(GameAction::CastSpell {
            object_id: s.spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .unwrap();
    for _ in 0..10 {
        match s.runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                s.runner.act(GameAction::ChooseX { value: 3 }).unwrap();
            }
            WaitingFor::OptionalCostChoice { .. } => {
                s.runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .unwrap();
            }
            WaitingFor::CostTypeChoice { .. } => break,
            other => panic!("expected CostTypeChoice, got {other:?}"),
        }
    }
    let actions = engine::ai_support::legal_actions(s.runner.state());
    let choose: Vec<&GameAction> = actions
        .iter()
        .filter(|a| matches!(a, GameAction::ChooseOption { .. }))
        .collect();
    assert_eq!(
        choose.len(),
        1,
        "exactly one ChooseOption (Elf) — Goblin is not feasible: {actions:?}"
    );
    assert!(
        matches!(choose[0], GameAction::ChooseOption { choice } if choice == "Elf"),
        "the sole ChooseOption must be Elf: {choose:?}"
    );
}

// ── Review round (PR #4990) — CostTypeChoice hardening ───────────────────

/// Finding #1 (review MED): backing out of a cast AFTER the pre-cost creature
/// type was chosen must rewind that choice. The type is recorded as a
/// `ChosenAttribute::CreatureType` on the spell object during cost payment; if it
/// survives the rewind, the `already_chosen` guard in
/// `pay_additional_cost_with_source` skips the `CostTypeChoice` prompt on the
/// NEXT cast and silently reuses the stale type.
///
/// Discriminating on two independent axes, BOTH of which fail if the
/// `retain(!CreatureType)` rewind in `handle_cancel_cast` is reverted:
///   (a) after cancel, the spell object carries NO `CreatureType` chosen attr;
///   (b) the re-cast re-presents `CostTypeChoice` rather than skipping straight
///       to the behold `PayCost` with the stale type.
#[test]
fn cancel_after_type_choice_rewinds_and_recast_reprompts() {
    let mut s = setup();
    let card_id = s.runner.state().objects[&s.spell].card_id;

    // First cast: advance to CostTypeChoice, choose "Elf" (records the attribute),
    // then stop at the behold PayCost.
    s.runner
        .act(GameAction::CastSpell {
            object_id: s.spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast accepted");
    let mut reached_paycost = false;
    for _ in 0..12 {
        match s.runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                s.runner.act(GameAction::ChooseX { value: 3 }).unwrap();
            }
            WaitingFor::OptionalCostChoice { .. } => {
                s.runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .unwrap();
            }
            WaitingFor::CostTypeChoice { .. } => {
                s.runner
                    .act(GameAction::ChooseOption {
                        choice: "Elf".to_string(),
                    })
                    .unwrap();
            }
            WaitingFor::PayCost { .. } => {
                reached_paycost = true;
                break;
            }
            other => panic!("unexpected prompt before behold: {other:?}"),
        }
    }
    assert!(
        reached_paycost,
        "should reach the behold PayCost after choosing the type"
    );
    assert!(
        s.runner.state().objects[&s.spell]
            .chosen_attributes
            .iter()
            .any(|a| matches!(a, engine::types::ability::ChosenAttribute::CreatureType(t) if t == "Elf")),
        "precondition: the chosen creature type is recorded during cost payment"
    );

    // (a) Cancel — the fix rewinds the chosen attribute.
    s.runner
        .act(GameAction::CancelCast)
        .expect("cancel is legal at the behold PayCost");
    assert!(
        s.runner.state().players[0].hand.contains(&s.spell),
        "spell returns to hand on cancel"
    );
    assert!(
        !s.runner.state().objects[&s.spell]
            .chosen_attributes
            .iter()
            .any(|a| matches!(a, engine::types::ability::ChosenAttribute::CreatureType(_))),
        "REVERT-PROOF (a): cancel must remove the stale chosen creature type"
    );

    // (b) Re-cast: the type prompt must re-appear (not be skipped). Mana spend is
    // deferred to finalize, so the pool is intact; top up defensively regardless.
    add_mana(&mut s.runner, 4);
    s.runner
        .act(GameAction::CastSpell {
            object_id: s.spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("re-cast accepted");
    let mut saw_cost_type_choice = false;
    for _ in 0..12 {
        match s.runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                s.runner.act(GameAction::ChooseX { value: 3 }).unwrap();
            }
            WaitingFor::OptionalCostChoice { .. } => {
                s.runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .unwrap();
            }
            WaitingFor::CostTypeChoice { .. } => {
                saw_cost_type_choice = true;
                break;
            }
            // Reaching the behold selection WITHOUT a type prompt means the stale
            // type was silently reused — the bug this fix closes.
            WaitingFor::PayCost { .. } => break,
            other => panic!("unexpected prompt on re-cast: {other:?}"),
        }
    }
    assert!(
        saw_cost_type_choice,
        "REVERT-PROOF (b): re-cast must re-present CostTypeChoice, not skip to behold with the stale type"
    );
}

/// Finding #2 (review MED): `CostTypeChoice.options` is computed from the
/// caster's beholdable cards (including hand), so serializing it in full leaks
/// private hand contents. `filter_state_for_viewer` must empty `options` for a
/// viewer who cannot see the caster's private zones, while the caster keeps the
/// full list. Reverting the redaction arm re-exposes the options to the opponent.
#[test]
fn cost_type_choice_options_redacted_for_opponent_viewer() {
    use engine::game::visibility::filter_state_for_viewer;

    let mut s = setup();
    let card_id = s.runner.state().objects[&s.spell].card_id;
    s.runner
        .act(GameAction::CastSpell {
            object_id: s.spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast accepted");
    for _ in 0..10 {
        match s.runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                s.runner.act(GameAction::ChooseX { value: 3 }).unwrap();
            }
            WaitingFor::OptionalCostChoice { .. } => {
                s.runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .unwrap();
            }
            WaitingFor::CostTypeChoice { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    // Precondition: the real (unfiltered) prompt offers a non-empty option list
    // that reveals a hand-beholdable type.
    match s.runner.state().waiting_for.clone() {
        WaitingFor::CostTypeChoice { options, .. } => assert!(
            options.contains(&"Elf".to_string()),
            "precondition: CostTypeChoice offers the hand-derived type Elf: {options:?}"
        ),
        other => panic!("expected CostTypeChoice, got {other:?}"),
    }

    // Caster (P0) — active player — retains the full option list to choose from.
    let caster_view = filter_state_for_viewer(s.runner.state(), P0);
    match caster_view.waiting_for {
        WaitingFor::CostTypeChoice { ref options, .. } => assert!(
            options.contains(&"Elf".to_string()),
            "caster must retain the full option list: {options:?}"
        ),
        ref other => panic!("expected CostTypeChoice in caster view, got {other:?}"),
    }

    // Opponent (P1) must NOT see any hand-derived options.
    let opponent_view = filter_state_for_viewer(s.runner.state(), P1);
    match opponent_view.waiting_for {
        WaitingFor::CostTypeChoice { ref options, .. } => assert!(
            options.is_empty(),
            "REVERT-PROOF: opponent must see NO hand-derived creature-type options, got {options:?}"
        ),
        ref other => panic!("expected CostTypeChoice in opponent view, got {other:?}"),
    }
}
