//! Yuna's Whistle / Calibrated Blast: CR 603.12 reflexive "When you reveal a
//! <filter> card this way" trigger after a reveal-until.
//!
//! Every test casts through the production path (`GameScenario` + `cast`) and
//! carries a positive reach guard so a pass cannot be an empty-set accident.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ParentTargetMissingReason, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const YUNAS_WHISTLE: &str = "Reveal cards from the top of your library until you reveal a creature card. Put that card into your hand and the rest on the bottom of your library in a random order. When you reveal a creature card this way, put X +1/+1 counters on target creature you control, where X is the mana value of that card.";

/// Accepted synthetic shape through the same production dispatch: identical to
/// Yuna's Whistle except the bottom order is chosen, so the reveal pauses on a
/// production `WaitingFor::RevealUntilBottomOrder` with two or more misses.
const WHISTLE_ANY_ORDER: &str = "Reveal cards from the top of your library until you reveal a creature card. Put that card into your hand and the rest on the bottom of your library in any order. When you reveal a creature card this way, put X +1/+1 counters on target creature you control, where X is the mana value of that card.";

const CALIBRATED_BLAST: &str = "Reveal cards from the top of your library until you reveal a nonland card. Put the revealed cards on the bottom of your library in a random order. When you reveal a nonland card this way, Calibrated Blast deals damage equal to that card's mana value to any target.";

/// Same `RevealUntil` → `ChangeZone { target: ParentTarget }` shape as
/// Sibylline Soothsayer's trigger, as an instant.
const EXILE_THAT_CARD: &str = "Reveal cards from the top of your library until you reveal a nonland card. Exile that card. Put the rest of the revealed cards on the bottom of your library in a random order.";

/// What a library card is, bottom-up call order (the LAST added is on top).
enum LibCard {
    Land(&'static str),
    Creature(&'static str, ManaCost),
    Sorcery(&'static str, ManaCost),
}

struct Fixture {
    runner: GameRunner,
    spell: ObjectId,
    bear: Option<ObjectId>,
    opp_bear: ObjectId,
    library: Vec<ObjectId>,
}

/// `library` lists cards TOP FIRST.
fn fixture(name: &str, oracle: &str, own_creature: bool, library: &[LibCard]) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = own_creature.then(|| scenario.add_creature(P0, "Bear", 2, 2).id());
    // A second creature P0 controls, so the reflexive's single legal target is
    // never auto-selected and the target prompt is observable.
    if own_creature {
        scenario.add_creature(P0, "Cub", 1, 1);
    }
    let opp_bear = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, true, oracle)
        .id();
    let mut ids = Vec::new();
    for card in library.iter().rev() {
        let id = match card {
            LibCard::Land(n) => scenario
                .add_spell_to_library_top(P0, n, false)
                .as_land()
                .id(),
            LibCard::Creature(n, cost) => scenario
                .add_spell_to_library_top(P0, n, false)
                .as_creature()
                .with_mana_cost(cost.clone())
                .id(),
            LibCard::Sorcery(n, cost) => scenario
                .add_spell_to_library_top(P0, n, false)
                .with_mana_cost(cost.clone())
                .id(),
        };
        ids.push(id);
    }
    ids.reverse();
    Fixture {
        runner: scenario.build(),
        spell,
        bear,
        opp_bear,
        library: ids,
    }
}

fn zone(state: &GameState, id: ObjectId) -> Zone {
    state.objects[&id].zone
}

fn p1p1(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn spell_ability_has_no_targets(state: &GameState) -> bool {
    let Some(entry) = state.stack.last() else {
        return false;
    };
    let StackEntryKind::Spell {
        ability: Some(ability),
        ..
    } = &entry.kind
    else {
        return false;
    };
    ability.targets.is_empty()
        && ability
            .sub_ability
            .as_ref()
            .is_none_or(|sub| sub.targets.is_empty())
}

/// Pass priority until the stack is empty or a non-priority prompt opens.
/// Returns whether a `TriggerTargetSelection` prompt was seen.
fn pass_until_prompt_or_empty(runner: &mut GameRunner) -> bool {
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => return true,
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return false,
        }
    }
    panic!("stack never settled: {:?}", runner.state().waiting_for);
}

fn legal_targets(state: &GameState) -> Vec<TargetRef> {
    let WaitingFor::TriggerTargetSelection { target_slots, .. } = &state.waiting_for else {
        panic!(
            "expected TriggerTargetSelection, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1, "one reflexive target slot");
    target_slots[0].legal_targets.clone()
}

fn choose(runner: &mut GameRunner, target: TargetRef) {
    runner
        .act(GameAction::ChooseTarget {
            target: Some(target),
        })
        .expect("choose reflexive target");
}

/// CR 603.12 + CR 603.3 + CR 115.1: no target is announced at cast; the
/// reflexive trigger is created by the reveal, targets as it goes on the stack,
/// and players may respond to it.
#[test]
fn whistle_reflexive_targets_after_reveal_and_uses_revealed_mana_value() {
    let Fixture {
        mut runner,
        spell,
        bear,
        opp_bear,
        library,
    } = fixture(
        "Yuna's Whistle",
        YUNAS_WHISTLE,
        true,
        &[
            LibCard::Land("Forest"),
            LibCard::Land("Island"),
            LibCard::Creature("Library Beast", ManaCost::generic(4)),
            LibCard::Land("Deep Card"),
        ],
    );
    let bear = bear.unwrap();
    runner.cast(spell).commit();
    assert!(
        spell_ability_has_no_targets(runner.state()),
        "reach guard: Whistle is on the stack with no announced target"
    );

    assert!(
        pass_until_prompt_or_empty(&mut runner),
        "reflexive target prompt"
    );
    let st = runner.state();
    assert_eq!(zone(st, library[2]), Zone::Hand, "creature card to hand");
    assert_eq!(zone(st, library[0]), Zone::Library);
    assert_eq!(zone(st, library[1]), Zone::Library);
    let legal = legal_targets(st);
    assert!(legal.contains(&TargetRef::Object(bear)), "{legal:?}");
    assert!(!legal.contains(&TargetRef::Object(opp_bear)), "{legal:?}");

    choose(&mut runner, TargetRef::Object(bear));
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the reflexive trigger is on the stack"
    );
    assert!(matches!(
        runner.state().stack[0].kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    // Each player may respond to it.
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));
    assert_eq!(p1p1(runner.state(), bear), 0, "not resolved yet");
    runner.act(GameAction::PassPriority).unwrap();
    assert!(runner.state().stack.is_empty());
    assert_eq!(p1p1(runner.state(), bear), 4);
    assert_eq!(p1p1(runner.state(), opp_bear), 0);
}

/// CR 603.12: no creature card revealed → the trigger event never happened.
#[test]
fn whistle_without_a_creature_revealed_does_not_trigger() {
    let Fixture {
        mut runner,
        spell,
        bear,
        library,
        ..
    } = fixture(
        "Yuna's Whistle",
        YUNAS_WHISTLE,
        true,
        &[
            LibCard::Land("Forest"),
            LibCard::Land("Island"),
            LibCard::Land("Plains"),
        ],
    );
    runner.cast(spell).commit();
    let saw_prompt = pass_until_prompt_or_empty(&mut runner);
    let st = runner.state();
    // Reach guard: the reveal ran and whiffed.
    assert_eq!(
        st.last_revealed_ids.len(),
        library.len(),
        "every library card was revealed"
    );
    assert!(!saw_prompt, "no reflexive target prompt");
    assert!(st.stack.is_empty());
    assert_eq!(p1p1(st, bear.unwrap()), 0);
    assert_eq!(zone(st, spell), Zone::Graveyard);
}

/// Ruling 2025-06-06 + CR 202.3e: {X} in the revealed card's cost is 0.
#[test]
fn whistle_counts_x_as_zero_for_the_revealed_mana_value() {
    let x_g = ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Green],
        generic: 0,
    };
    let Fixture {
        mut runner,
        spell,
        bear,
        ..
    } = fixture(
        "Yuna's Whistle",
        YUNAS_WHISTLE,
        true,
        &[LibCard::Creature("X Beast", x_g)],
    );
    let bear = bear.unwrap();
    runner.cast(spell).commit();
    assert!(pass_until_prompt_or_empty(&mut runner));
    choose(&mut runner, TargetRef::Object(bear));
    pass_until_prompt_or_empty(&mut runner);
    assert_eq!(p1p1(runner.state(), bear), 1);
}

/// CR 603.3d: no legal target → the reflexive is removed; the reveal still
/// happened.
#[test]
fn whistle_with_no_creature_you_control_drops_the_reflexive() {
    let Fixture {
        mut runner,
        spell,
        opp_bear,
        library,
        ..
    } = fixture(
        "Yuna's Whistle",
        YUNAS_WHISTLE,
        false,
        &[
            LibCard::Land("Forest"),
            LibCard::Creature("Library Beast", ManaCost::generic(3)),
        ],
    );
    runner.cast(spell).commit();
    assert!(!pass_until_prompt_or_empty(&mut runner));
    let st = runner.state();
    assert_eq!(
        zone(st, library[1]),
        Zone::Hand,
        "reach guard: the hit went to hand"
    );
    assert!(st.stack.is_empty());
    assert!(matches!(st.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(p1p1(st, opp_bear), 0);
}

/// Calibrated Blast is the same class: no target at cast; the reflexive
/// targets after the reveal and deals damage equal to the hit's mana value.
#[test]
fn calibrated_blast_reflexive_fires_after_the_reveal() {
    let Fixture {
        mut runner, spell, ..
    } = fixture(
        "Calibrated Blast",
        CALIBRATED_BLAST,
        false,
        &[
            LibCard::Land("Forest"),
            LibCard::Sorcery("Three Drop", ManaCost::generic(3)),
        ],
    );
    let life_before = runner.state().players[1].life;
    runner.cast(spell).commit();
    assert!(
        spell_ability_has_no_targets(runner.state()),
        "reach guard: no target announced at cast"
    );
    assert!(
        pass_until_prompt_or_empty(&mut runner),
        "reflexive target prompt"
    );
    assert!(legal_targets(runner.state()).contains(&TargetRef::Player(P1)));
    choose(&mut runner, TargetRef::Player(P1));
    pass_until_prompt_or_empty(&mut runner);
    assert_eq!(runner.state().players[1].life, life_before - 3);
}

#[test]
fn calibrated_blast_without_a_nonland_card_does_not_trigger() {
    let Fixture {
        mut runner, spell, ..
    } = fixture(
        "Calibrated Blast",
        CALIBRATED_BLAST,
        false,
        &[LibCard::Land("Forest"), LibCard::Land("Island")],
    );
    let life_before = runner.state().players[1].life;
    runner.cast(spell).commit();
    assert!(!pass_until_prompt_or_empty(&mut runner));
    assert_eq!(runner.state().last_revealed_ids.len(), 2, "reach guard");
    assert_eq!(runner.state().players[1].life, life_before);
}

/// Serialize and restore the full `GameState` (reconnect / persistence / P2P
/// resume), then keep playing from the restored state.
fn round_trip(runner: &mut GameRunner) {
    let json = serde_json::to_string(runner.state()).expect("serialize GameState");
    let restored: GameState = serde_json::from_str(&json).expect("deserialize GameState");
    *runner.state_mut() = restored;
}

fn bottom_order_cards(state: &GameState) -> Vec<ObjectId> {
    let WaitingFor::RevealUntilBottomOrder { cards, .. } = &state.waiting_for else {
        panic!(
            "expected RevealUntilBottomOrder, got {:?}",
            state.waiting_for
        );
    };
    cards.clone()
}

/// CR 603.12: a whiff that pauses on the production bottom-order prompt, with
/// the full GameState round-tripped through JSON before the answer, must not
/// mint the reflexive on resume.
#[test]
fn paused_whiff_survives_a_state_round_trip_without_triggering() {
    let Fixture {
        mut runner,
        spell,
        bear,
        library,
        ..
    } = fixture(
        "Yuna's Whistle",
        WHISTLE_ANY_ORDER,
        true,
        &[
            LibCard::Land("Forest"),
            LibCard::Land("Island"),
            LibCard::Land("Plains"),
        ],
    );
    runner.cast(spell).commit();
    assert!(!pass_until_prompt_or_empty(&mut runner));
    let cards = bottom_order_cards(runner.state());
    assert_eq!(
        cards.len(),
        library.len(),
        "reach guard: every card is a miss"
    );

    round_trip(&mut runner);
    runner
        .act(GameAction::SelectCards { cards })
        .expect("answer bottom order");
    assert!(
        !pass_until_prompt_or_empty(&mut runner),
        "no reflexive prompt"
    );
    let st = runner.state();
    assert!(st.stack.is_empty());
    assert_eq!(p1p1(st, bear.unwrap()), 0);
}

/// Positive twin: a hit that pauses on the bottom-order prompt, round-tripped,
/// still triggers with the revealed card's mana value.
#[test]
fn paused_hit_survives_a_state_round_trip_and_triggers() {
    let Fixture {
        mut runner,
        spell,
        bear,
        library,
        ..
    } = fixture(
        "Yuna's Whistle",
        WHISTLE_ANY_ORDER,
        true,
        &[
            LibCard::Land("Forest"),
            LibCard::Land("Island"),
            LibCard::Creature("Library Beast", ManaCost::generic(3)),
        ],
    );
    let bear = bear.unwrap();
    runner.cast(spell).commit();
    assert!(!pass_until_prompt_or_empty(&mut runner));
    let cards = bottom_order_cards(runner.state());
    assert_eq!(
        cards,
        vec![library[0], library[1]],
        "reach guard: the two misses"
    );
    assert_eq!(zone(runner.state(), library[2]), Zone::Hand);

    round_trip(&mut runner);
    runner
        .act(GameAction::SelectCards { cards })
        .expect("answer bottom order");
    assert!(
        pass_until_prompt_or_empty(&mut runner),
        "reflexive prompt on resume"
    );
    assert!(legal_targets(runner.state()).contains(&TargetRef::Object(bear)));
    choose(&mut runner, TargetRef::Object(bear));
    pass_until_prompt_or_empty(&mut runner);
    assert_eq!(p1p1(runner.state(), bear), 3);
}

/// Instance binding at the production entry: a stale whiff verdict left in the
/// slot before this resolution cannot suppress this reveal's hit. (The depth-0
/// resolution reset clears it first; the reveal's own hit-clear is pinned
/// separately by `reveal_until::tests::verdict_is_this_reveals_own_outcome_*`.)
#[test]
fn a_stale_whiff_verdict_cannot_suppress_this_reveals_hit() {
    let Fixture {
        mut runner,
        spell,
        bear,
        ..
    } = fixture(
        "Yuna's Whistle",
        YUNAS_WHISTLE,
        true,
        &[LibCard::Creature("Library Beast", ManaCost::generic(2))],
    );
    let bear = bear.unwrap();
    runner.cast(spell).commit();
    runner.state_mut().last_parent_target_missing_reason =
        Some(ParentTargetMissingReason::RevealUntil);
    assert!(pass_until_prompt_or_empty(&mut runner), "this reveal hit");
    choose(&mut runner, TargetRef::Object(bear));
    pass_until_prompt_or_empty(&mut runner);
    assert_eq!(p1p1(runner.state(), bear), 2);
}

/// And the converse: a stale "no reason" slot cannot make this reveal's whiff
/// look like a hit.
#[test]
fn a_cleared_slot_cannot_turn_this_reveals_whiff_into_a_hit() {
    let Fixture {
        mut runner,
        spell,
        bear,
        ..
    } = fixture(
        "Yuna's Whistle",
        YUNAS_WHISTLE,
        true,
        &[LibCard::Land("Forest"), LibCard::Land("Island")],
    );
    runner.cast(spell).commit();
    runner.state_mut().last_parent_target_missing_reason = None;
    assert!(!pass_until_prompt_or_empty(&mut runner));
    assert_eq!(runner.state().last_revealed_ids.len(), 2, "reach guard");
    assert_eq!(p1p1(runner.state(), bear.unwrap()), 0);
}

/// Changed consumer (CR 608.2c): after a reveal-until whiff, a `ParentTarget`
/// `ChangeZone` child ("Exile that card") has no referent and does nothing —
/// it must not fall back to the resolving spell itself.
#[test]
fn exile_that_card_after_a_whiff_moves_nothing() {
    let Fixture {
        mut runner,
        spell,
        library,
        ..
    } = fixture(
        "Exile Probe",
        EXILE_THAT_CARD,
        false,
        &[LibCard::Land("Forest"), LibCard::Land("Island")],
    );
    runner.cast(spell).commit();
    pass_until_prompt_or_empty(&mut runner);
    let st = runner.state();
    assert_eq!(
        st.last_revealed_ids.len(),
        2,
        "reach guard: the reveal whiffed"
    );
    assert_eq!(zone(st, spell), Zone::Graveyard, "the spell is not exiled");
    for id in library {
        assert_eq!(zone(st, id), Zone::Library);
    }
}

/// Adjacent control for the changed consumer: on a hit, "Exile that card"
/// exiles the revealed card.
#[test]
fn exile_that_card_after_a_hit_exiles_the_hit() {
    let Fixture {
        mut runner,
        spell,
        library,
        ..
    } = fixture(
        "Exile Probe",
        EXILE_THAT_CARD,
        false,
        &[
            LibCard::Land("Forest"),
            LibCard::Sorcery("Three Drop", ManaCost::generic(3)),
        ],
    );
    runner.cast(spell).commit();
    pass_until_prompt_or_empty(&mut runner);
    let st = runner.state();
    assert_eq!(zone(st, library[1]), Zone::Exile);
    assert_eq!(zone(st, spell), Zone::Graveyard);
}

const TUNNEL_VISION: &str = "Choose a card name. Target player reveals cards from the top of their library until a card with that name is revealed. If it is, that player puts the rest of the revealed cards into their graveyard and puts the card with the chosen name on top of their library. Otherwise, the player shuffles.";

/// Unchanged consumer: `put_on_top` no-ops only on the exact `Dig` reason, so
/// Tunnel Vision's `PutAtLibraryPosition { ParentTarget }` child is untouched by
/// the new `RevealUntil` whiff reason. This outcome is pinned identically with
/// the verdict producer reverted (see the FIRED table).
#[test]
fn tunnel_vision_whiff_is_unchanged_by_the_reveal_until_reason() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tunnel Vision", false, TUNNEL_VISION)
        .id();
    let island = scenario
        .add_spell_to_library_top(P1, "Island", false)
        .as_land()
        .id();
    let forest = scenario
        .add_spell_to_library_top(P1, "Forest", false)
        .as_land()
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_card_names = vec!["Llanowar Elves".to_string()].into();
    runner.cast(spell).target_player(P1).commit();
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { options, .. } => {
                let choice = options
                    .iter()
                    .find(|name| *name != "Forest" && *name != "Island")
                    .cloned()
                    .unwrap_or_else(|| "Llanowar Elves".to_string());
                runner
                    .act(GameAction::ChooseOption { choice })
                    .expect("name a card");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    let st = runner.state();
    assert_eq!(
        st.last_revealed_ids.len(),
        2,
        "reach guard: the reveal whiffed"
    );
    assert_eq!(zone(st, forest), Zone::Graveyard);
    assert_eq!(zone(st, island), Zone::Graveyard);
    // Pre-existing behaviour, pinned as-is: the `ParentTarget` placement is not
    // no-op'd by a non-`Dig` reason and falls through to its generic zone
    // choice. Identical with the reveal-until verdict producer reverted.
    assert!(
        matches!(
            &st.waiting_for,
            WaitingFor::EffectZoneChoice { cards, .. } if cards.len() == 2
        ),
        "{:?}",
        st.waiting_for
    );
}
