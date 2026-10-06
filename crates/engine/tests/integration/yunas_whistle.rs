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
    /// Extra instants added to hands, in the order given to `fixture_with`.
    extras: Vec<ObjectId>,
}

/// `library` lists cards TOP FIRST.
fn fixture(name: &str, oracle: &str, own_creature: bool, library: &[LibCard]) -> Fixture {
    fixture_with(name, oracle, own_creature, library, &[])
}

/// As [`fixture`], plus extra instants `(owner, name, oracle)` in hand.
fn fixture_with(
    name: &str,
    oracle: &str,
    own_creature: bool,
    library: &[LibCard],
    extras: &[(engine::types::player::PlayerId, &str, &str)],
) -> Fixture {
    fixture_shifted(name, oracle, own_creature, library, extras, 0)
}

/// As [`fixture_with`], after first putting `id_shift` inert lands into P1's
/// library, which shifts every later object id by `id_shift`.
fn fixture_shifted(
    name: &str,
    oracle: &str,
    own_creature: bool,
    library: &[LibCard],
    extras: &[(engine::types::player::PlayerId, &str, &str)],
    id_shift: usize,
) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..id_shift {
        scenario
            .add_spell_to_library_top(P1, "Filler", false)
            .as_land();
    }
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
    let extras = extras
        .iter()
        .map(|(owner, extra_name, extra_oracle)| {
            scenario
                .add_spell_to_hand_from_oracle(*owner, extra_name, true, extra_oracle)
                .id()
        })
        .collect();
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
        extras,
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
        ..
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

/// A generic "When you do" after a reveal-until carries no reveal-until-hit
/// guard: the instruction was performed even when no matching card was
/// revealed, so a whiff still triggers it (base behaviour). The Whistle and
/// Calibrated Blast whiff tests above are the guarded counterparts.
const REVEAL_THEN_WHEN_YOU_DO: &str = "Reveal cards from the top of your library until you reveal a creature card. When you do, draw a card.";

fn hand_after_generic_when_you_do(library: &[LibCard]) -> (usize, usize) {
    let Fixture {
        mut runner, spell, ..
    } = fixture("Probe", REVEAL_THEN_WHEN_YOU_DO, false, library);
    runner.cast(spell).commit();
    let revealed_before = runner.state().players[0].hand.len();
    pass_until_prompt_or_empty(&mut runner);
    assert!(runner.state().stack.is_empty());
    (revealed_before, runner.state().players[0].hand.len())
}

#[test]
fn generic_when_you_do_after_a_reveal_until_whiff_still_triggers() {
    // Whiff: no card to hand, one draw.
    let (before, after) = hand_after_generic_when_you_do(&[
        LibCard::Land("Forest"),
        LibCard::Land("Island"),
        LibCard::Land("Swamp"),
    ]);
    assert_eq!(after, before + 1, "the reflexive draw fired after a whiff");
    // Hit control: the creature to hand plus the draw.
    let (before, after) = hand_after_generic_when_you_do(&[
        LibCard::Land("Forest"),
        LibCard::Creature("Beast", ManaCost::generic(2)),
        LibCard::Land("Swamp"),
    ]);
    assert_eq!(after, before + 2);
}

const CAIT_SITH: &str = "Lucky Slots — At the beginning of combat on your turn, scry 1, then exile the top card of your library. You may play that card this turn. When you exile a card this way, target creature you control gets +X/+0 until end of turn, where X is that card's mana value.";

/// CR 603.12 (Cait Sith's 2025-06-06 ruling): its reflexive's target timing is
/// not modeled, so the clause is honestly unsupported. Production combat
/// trigger: no target is announced with the trigger (no early prompt), and no
/// pump lands, while the trigger itself still scries and exiles.
#[test]
fn cait_sith_announces_no_target_with_its_combat_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Cait Sith, Fortune Teller", 2, 2, CAIT_SITH);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let top = scenario
        .add_spell_to_library_top(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mut runner = scenario.build();
    let mut saw_scry = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                panic!("no target may be announced with Cait Sith's trigger")
            }
            WaitingFor::ScryChoice { .. } => {
                saw_scry = true;
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("keep the scried card on top");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().phase == Phase::DeclareAttackers {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    let st = runner.state();
    assert!(saw_scry, "reach guard: the combat trigger resolved");
    assert_eq!(
        zone(st, top),
        Zone::Exile,
        "reach guard: the top card was exiled"
    );
    assert_eq!(st.objects[&bear].power, Some(2), "no pump");
}

// ---------------------------------------------------------------------------
// CR 701.20a: "If revealing a card causes a triggered ability to trigger, the
// card remains revealed until that triggered ability leaves the stack."
// ---------------------------------------------------------------------------

const STIFLE: &str = "Counter target activated or triggered ability.";
const THRILL_OF_POSSIBILITY: &str =
    "As an additional cost to cast this spell, discard a card.\nDraw two cards.";
const COPY_TRIGGER: &str =
    "Copy target triggered ability you control. You may choose new targets for the copy.";
const HIDDEN: &str = "Hidden Card";

fn name_seen_by(
    state: &GameState,
    viewer: engine::types::player::PlayerId,
    id: ObjectId,
) -> String {
    engine::game::visibility::filter_state_for_viewer(state, viewer).objects[&id]
        .name
        .clone()
}

/// The engine-authored, unindexed "revealed" presentation a seated viewer
/// receives: stack entry → card names.
fn stack_revealed_seen_by(
    state: &GameState,
    viewer: engine::types::player::PlayerId,
) -> std::collections::BTreeMap<ObjectId, Vec<String>> {
    let projected = engine::game::visibility::filter_state_for_viewer(state, viewer);
    engine::game::derived_views::derive_filtered_views(state, &projected, Some(viewer))
        .stack_revealed_cards
}

/// The stack display groups a seated viewer receives, as member-id lists.
fn stack_groups_seen_by(
    state: &GameState,
    viewer: engine::types::player::PlayerId,
) -> Vec<Vec<ObjectId>> {
    let projected = engine::game::visibility::filter_state_for_viewer(state, viewer);
    engine::game::derived_views::derive_filtered_views(state, &projected, Some(viewer))
        .stack_display_groups
        .into_iter()
        .map(|group| group.member_ids)
        .collect()
}

fn reflexive_entries(state: &GameState) -> Vec<ObjectId> {
    state
        .stack
        .iter()
        .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        .map(|entry| entry.id)
        .collect()
}

fn pass_once_each(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).expect("P0 passes");
    runner.act(GameAction::PassPriority).expect("P1 passes");
}

/// Whistle with its reflexive on the stack, P0's Bear chosen, P0 holding
/// priority. Library: Forest, Library Beast (MV 4), then three spares.
fn whistle_with_reflexive_on_stack(
    extras: &[(engine::types::player::PlayerId, &str, &str)],
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let Fixture {
        mut runner,
        spell,
        bear,
        library,
        extras,
        ..
    } = fixture_with(
        "Yuna's Whistle",
        YUNAS_WHISTLE,
        true,
        &[
            LibCard::Land("Forest"),
            LibCard::Creature("Library Beast", ManaCost::generic(4)),
            LibCard::Land("Spare A"),
            LibCard::Land("Spare B"),
            LibCard::Land("Spare C"),
        ],
        extras,
    );
    let bear = bear.unwrap();
    runner.cast(spell).commit();
    assert!(pass_until_prompt_or_empty(&mut runner));
    let hit = library[1];
    assert_eq!(
        zone(runner.state(), hit),
        Zone::Hand,
        "reach guard: hit in hand"
    );
    assert_eq!(
        name_seen_by(runner.state(), P1, hit),
        "Library Beast",
        "the opponent sees the hit at target selection"
    );
    choose(&mut runner, TargetRef::Object(bear));
    assert_eq!(
        runner.state().stack_bound_reveals.len(),
        1,
        "reach guard: one lease"
    );
    (runner, hit, bear, extras)
}

#[test]
fn whistle_hit_stays_public_until_its_reflexive_resolves() {
    let (mut runner, hit, bear, _) = whistle_with_reflexive_on_stack(&[]);
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    assert_eq!(
        name_seen_by(runner.state(), P1, hit),
        "Library Beast",
        "P1 sees the revealed card while the reflexive is on the stack"
    );
    runner.act(GameAction::PassPriority).unwrap();
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        p1p1(runner.state(), bear),
        4,
        "reach guard: the reflexive resolved"
    );
    assert_eq!(
        name_seen_by(runner.state(), P1, hit),
        HIDDEN,
        "hidden once it left the stack"
    );
    assert_eq!(
        name_seen_by(runner.state(), P0, hit),
        "Library Beast",
        "the owner still sees it"
    );
    assert!(runner.state().stack_bound_reveals.is_empty());
}

/// The non-library control for the CR 401.2 redactions: Whistle's hit is in a
/// hand, whose order is not hidden, so while its reflexive waits every carrier
/// keeps pairing it with its id for the opponent. The misses, bottomed at
/// random, lose their ids from the reveal event.
#[test]
fn whistle_hand_hit_keeps_its_id_in_every_viewer_carrier() {
    use engine::types::events::GameEvent;
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
            LibCard::Creature("Library Beast", ManaCost::generic(4)),
        ],
    );
    let (miss, hit) = (library[0], library[1]);
    let mut steps = Vec::new();
    runner.cast(spell).commit();
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        act_logged(&mut runner, &mut steps, GameAction::PassPriority);
    }
    act_logged(
        &mut runner,
        &mut steps,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear.unwrap())),
        },
    );
    assert_eq!(zone(runner.state(), hit), Zone::Hand, "reach guard");
    assert_eq!(zone(runner.state(), miss), Zone::Library, "reach guard");

    let events: Vec<GameEvent> = steps
        .iter()
        .flat_map(|step| {
            engine::game::visibility::filter_events_for_viewer(&step.events, &step.state, P1)
        })
        .collect();
    let (card_ids, card_names) = events
        .iter()
        .find_map(|event| match event {
            GameEvent::CardsRevealed {
                card_ids,
                card_names,
                ..
            } => Some((card_ids.clone(), card_names.clone())),
            _ => None,
        })
        .expect("reach guard: the reveal event reached P1");
    assert_eq!(card_names, vec!["Forest", "Library Beast"]);
    assert_eq!(
        card_ids,
        vec![hit],
        "the hand hit keeps its id; the randomly bottomed miss loses its id"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { subject: Some(subject), .. }
                if subject.identity.object_id == hit
        )),
        "the hand hit's EffectResolved subject is kept"
    );

    let projected = engine::game::visibility::filter_state_for_viewer(runner.state(), P1);
    assert_eq!(projected.objects[&hit].name, "Library Beast");
    let reflexive = reflexive_entries(runner.state())[0];
    let ability_context = projected
        .stack
        .iter()
        .find(|entry| entry.id == reflexive)
        .and_then(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { ability, .. } => {
                ability.effect_context_object.as_ref().map(|s| s.object_id)
            }
            _ => None,
        });
    assert_eq!(
        ability_context,
        Some(hit),
        "the public hand referent keeps its effect-context snapshot"
    );
    assert_eq!(
        stack_revealed_seen_by(runner.state(), P1),
        [(reflexive, vec!["Library Beast".to_string()])].into()
    );
}

#[test]
fn whistle_hit_stays_public_across_a_state_round_trip_mid_trigger() {
    let (mut runner, hit, _, _) = whistle_with_reflexive_on_stack(&[]);
    round_trip(&mut runner);
    assert_eq!(name_seen_by(runner.state(), P1, hit), "Library Beast");
    pass_once_each(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(name_seen_by(runner.state(), P1, hit), HIDDEN);
}

#[test]
fn whistle_hit_is_hidden_after_its_reflexive_is_countered() {
    let (mut runner, hit, bear, extras) =
        whistle_with_reflexive_on_stack(&[(P1, "Stifle", STIFLE)]);
    let reflexive = reflexive_entries(runner.state())[0];
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(extras[0]).target_object(reflexive).commit();
    assert_eq!(
        name_seen_by(runner.state(), P1, hit),
        "Library Beast",
        "still on the stack"
    );
    pass_until_prompt_or_empty(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        p1p1(runner.state(), bear),
        0,
        "reach guard: the reflexive was countered"
    );
    assert_eq!(name_seen_by(runner.state(), P1, hit), HIDDEN);
    assert!(runner.state().stack_bound_reveals.is_empty());
}

#[test]
fn whistle_hit_is_hidden_when_its_reflexive_has_no_legal_target() {
    let Fixture {
        mut runner,
        spell,
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
    assert_eq!(zone(runner.state(), library[1]), Zone::Hand, "reach guard");
    assert_eq!(name_seen_by(runner.state(), P1, library[1]), HIDDEN);
    assert!(runner.state().stack_bound_reveals.is_empty());
}

#[test]
fn a_copy_of_the_reflexive_does_not_extend_the_reveal() {
    let (mut runner, hit, bear, extras) =
        whistle_with_reflexive_on_stack(&[(P0, "Resonance", COPY_TRIGGER)]);
    let original = reflexive_entries(runner.state())[0];
    runner.cast(extras[0]).target_object(original).commit();
    // Resolve the copy spell; the copy of the reflexive goes on the stack.
    pass_once_each(&mut runner);
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
    {
        // Keep the copy's target (Bear) if it offers a retarget prompt.
        if runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bear)),
            })
            .is_err()
        {
            break;
        }
    }
    assert_eq!(
        reflexive_entries(runner.state()).len(),
        2,
        "reach guard: copy + original"
    );
    assert_eq!(
        runner.state().stack_bound_reveals.len(),
        1,
        "only the original owns a lease"
    );
    pass_once_each(&mut runner);
    assert_eq!(
        reflexive_entries(runner.state()),
        vec![original],
        "the copy resolved first"
    );
    assert_eq!(
        name_seen_by(runner.state(), P1, hit),
        "Library Beast",
        "original still on the stack"
    );
    pass_once_each(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        p1p1(runner.state(), bear),
        8,
        "reach guard: copy and original both resolved"
    );
    assert_eq!(name_seen_by(runner.state(), P1, hit), HIDDEN);
}

/// CR 400.7 + CR 701.20a: the hit is discarded (as Thrill of Possibility's
/// additional cost) while its reflexive is on the stack. The lease ends live,
/// and a cold replay of the journaled zone change from the pre-cast state ends
/// it identically.
#[test]
fn discarding_the_hit_ends_its_lease_live_and_on_cold_replay() {
    use engine::game::zones::apply_resolved_zone_change;
    use engine::types::resolved_commands::ResolvedRulesCommand;

    let (mut runner, hit, bear, extras) =
        whistle_with_reflexive_on_stack(&[(P0, "Thrill of Possibility", THRILL_OF_POSSIBILITY)]);
    let pre_cast: GameState = runner.state().clone();
    assert!(
        pre_cast.holds_stack_bound_reveal(hit),
        "reach guard: leased before"
    );
    runner.cast(extras[0]).pay_cost_with(&[hit]).commit();
    assert_eq!(
        zone(runner.state(), hit),
        Zone::Graveyard,
        "reach guard: discarded as a cost"
    );
    assert!(
        runner.state().stack_bound_reveals.is_empty(),
        "live: the lease ended"
    );

    let command = runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.as_ref())
        .find_map(|command| match command {
            ResolvedRulesCommand::ZoneChange(command)
                if command.object.object_id == hit
                    && command.from == Zone::Hand
                    && command.to == Zone::Graveyard =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .expect("the discard journals its zone command");
    let mut replay = pre_cast;
    apply_resolved_zone_change(&mut replay, &command).expect("hand → graveyard replays");
    assert_eq!(
        replay.stack_bound_reveals,
        runner.state().stack_bound_reveals,
        "cold replay ends the lease like the live path"
    );

    pass_until_prompt_or_empty(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        p1p1(runner.state(), bear),
        4,
        "X still reads the hit's mana value"
    );
}

/// One wire transition: the events and log a client receives for an action,
/// filtered against the state right after it (as the server does).
struct Step {
    events: Vec<engine::types::events::GameEvent>,
    logs: Vec<engine::types::log::GameLogEntry>,
    state: GameState,
}

fn act_logged(runner: &mut GameRunner, steps: &mut Vec<Step>, action: GameAction) {
    let result = runner.act(action).expect("action applies");
    steps.push(Step {
        events: result.events,
        logs: result.log_entries,
        state: runner.state().clone(),
    });
}

/// A wire id for a spectator: not a seat, so the event filter treats it as an
/// unseated audience.
const SPECTATOR: engine::types::player::PlayerId = engine::types::player::PlayerId(99);

/// The actual transport shape a viewer holds after `steps`: the current
/// projected state and its derived views, plus every event and log entry it
/// was sent along the way (a hostile client keeps them all). `None` is the
/// unseated audience.
fn transport_payload(
    state: &GameState,
    steps: &[Step],
    viewer: Option<engine::types::player::PlayerId>,
) -> serde_json::Value {
    use engine::game::visibility::{
        filter_events_for_viewer, filter_state_for_unseated_viewer, filter_state_for_viewer,
    };
    let projected = match viewer {
        Some(viewer) => filter_state_for_viewer(state, viewer),
        None => filter_state_for_unseated_viewer(state),
    };
    let derived = engine::game::derived_views::derive_filtered_views(state, &projected, viewer);
    let events: Vec<_> = steps
        .iter()
        .flat_map(|step| {
            filter_events_for_viewer(&step.events, &step.state, viewer.unwrap_or(SPECTATOR))
        })
        .collect();
    let logs: Vec<_> = steps.iter().flat_map(|step| step.logs.clone()).collect();
    serde_json::json!({
        "state": projected,
        "derived": derived,
        "events": events,
        "logs": logs,
    })
}

/// Every path in `value` that carries `id` as a number or a map key. Numeric
/// map keys are normalized (`<ID>` for `id`, `<K>` for any other), so paths
/// from two boards whose object ids differ by a constant shift line up.
fn id_paths(value: &serde_json::Value, id: u64, path: &str, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Number(n) if n.as_u64() == Some(id) => out.push(path.to_string()),
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                id_paths(item, id, &format!("{path}[{index}]"), out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                let child = match key.parse::<u64>() {
                    Ok(key) if key == id => {
                        out.push(format!("{path}.<ID>#key"));
                        format!("{path}.<ID>")
                    }
                    Ok(_) => format!("{path}.<K>"),
                    Err(_) => format!("{path}.{key}"),
                };
                id_paths(item, id, &child, out);
            }
        }
        _ => {}
    }
}

/// Every path in `value` whose string contains `name`.
fn name_paths(value: &serde_json::Value, name: &str, path: &str, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) if text.contains(name) => out.push(path.to_string()),
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                name_paths(item, name, &format!("{path}[{index}]"), out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                name_paths(item, name, &format!("{path}.{key}"), out);
            }
        }
        _ => {}
    }
}

/// CR 401.2 + CR 701.20a: the secrecy check over one viewer's payload, taken
/// on the same board built twice with object ids shifted by a constant (so a
/// number that merely equals the hit's id by coincidence is not mistaken for
/// a carrier). The hit's object id may appear only as the redacted object
/// itself and its slot in its owner's library; its name may appear only in
/// the unindexed public carriers. Returns whether the name reached
/// `derived.stack_revealed_cards`.
fn assert_hit_unlocatable(
    board: &BlastBoard,
    shifted: &BlastBoard,
    viewer: Option<engine::types::player::PlayerId>,
    label: &str,
) -> bool {
    let payload = transport_payload(board.runner.state(), &board.steps, viewer);
    let shifted_payload = transport_payload(shifted.runner.state(), &shifted.steps, viewer);
    let mut ids = Vec::new();
    id_paths(&payload, board.hit.0, "", &mut ids);
    let mut shifted_ids = Vec::new();
    id_paths(&shifted_payload, shifted.hit.0, "", &mut shifted_ids);
    let carriers: Vec<_> = ids
        .iter()
        .filter(|path| shifted_ids.contains(path))
        .collect();
    let leaks: Vec<_> = carriers
        .iter()
        .filter(|path| {
            !path.starts_with(".state.objects.<ID>")
                && !path.starts_with(".state.players[0].library[")
        })
        .collect();
    assert!(leaks.is_empty(), "{label}: the hit's id leaks at {leaks:?}");
    assert!(
        carriers
            .iter()
            .any(|path| path.starts_with(".state.players[0].library[")),
        "{label}: reach guard: the hit is in the projected library ({carriers:?})"
    );

    let mut names = Vec::new();
    name_paths(&payload, "Three Drop", "", &mut names);
    let unsafe_names: Vec<_> = names
        .iter()
        .filter(|path| {
            !path.starts_with(".derived.stack_revealed_cards")
                && !(path.starts_with(".events[") && path.contains(".card_names["))
                && !path.starts_with(".logs[")
        })
        .collect();
    assert!(
        unsafe_names.is_empty(),
        "{label}: the hit's name leaks at {unsafe_names:?}"
    );

    // The post-bottom library order alone must not place the hit: its projected
    // object is indistinguishable from a hidden miss but for the id itself.
    let strip = |id: ObjectId| {
        let mut object = payload["state"]["objects"][id.0.to_string()].clone();
        object
            .as_object_mut()
            .expect("projected object")
            .remove("id");
        object
    };
    assert_eq!(
        strip(board.hit),
        strip(board.miss),
        "{label}: the hit's projected object differs from a hidden miss"
    );

    names
        .iter()
        .any(|path| path.starts_with(".derived.stack_revealed_cards"))
}

struct BlastBoard {
    runner: GameRunner,
    steps: Vec<Step>,
    hit: ObjectId,
    miss: ObjectId,
}

/// Calibrated Blast over Forest, Forest, Three Drop (MV 3), Island: the reveal
/// bottoms the two Forests and the hit in a random order under the unrevealed
/// Island, and the reflexive (targeting P1) waits on the stack.
fn blast_board(id_shift: usize) -> BlastBoard {
    let Fixture {
        mut runner,
        spell,
        library,
        ..
    } = fixture_shifted(
        "Calibrated Blast",
        CALIBRATED_BLAST,
        false,
        &[
            LibCard::Land("Forest"),
            LibCard::Land("Forest"),
            LibCard::Sorcery("Three Drop", ManaCost::generic(3)),
            LibCard::Land("Island"),
        ],
        &[],
        id_shift,
    );
    let (miss, hit) = (library[0], library[2]);
    let mut steps = Vec::new();
    runner.cast(spell).commit();
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        act_logged(&mut runner, &mut steps, GameAction::PassPriority);
    }
    act_logged(
        &mut runner,
        &mut steps,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        },
    );
    let st = runner.state();
    assert_eq!(zone(st, hit), Zone::Library, "reach guard: bottomed");
    assert_eq!(
        st.players[0].library.front().copied(),
        Some(library[3]),
        "reach guard: the unrevealed Island is on top"
    );
    assert_eq!(
        reflexive_entries(st).len(),
        1,
        "reach guard: reflexive live"
    );
    assert!(
        st.holds_stack_bound_reveal(hit),
        "reach guard: the authoritative lease exists"
    );
    assert!(
        st.last_revealed_ids.contains(&hit),
        "reach guard: the authoritative reveal ledger names the hit"
    );
    BlastBoard {
        runner,
        steps,
        hit,
        miss,
    }
}

fn resolve_blast_reflexive(board: &mut BlastBoard) {
    act_logged(
        &mut board.runner,
        &mut board.steps,
        GameAction::PassPriority,
    );
    act_logged(
        &mut board.runner,
        &mut board.steps,
        GameAction::PassPriority,
    );
    assert!(board.runner.state().stack.is_empty());
    assert_eq!(
        board.runner.state().players[1].life,
        17,
        "reach guard: the reflexive resolved for 3"
    );
}

const VIEWERS: [(Option<engine::types::player::PlayerId>, &str); 3] =
    [(Some(P0), "P0"), (Some(P1), "P1"), (None, "unseated")];

/// CR 701.20a + CR 401.2: Calibrated Blast's hit goes to the library bottom
/// "in a random order" among the misses while its reflexive waits. Its identity
/// is public for the reflexive's lifetime, but no viewer (the owner included)
/// can tie that identity to an object id or library position, in any carrier of
/// the transport payload, before or after the reflexive leaves the stack.
#[test]
fn calibrated_blast_hit_is_public_but_unlocatable_in_every_viewer_payload() {
    let mut board = blast_board(0);
    let mut shifted = blast_board(7);
    assert_eq!(
        shifted.hit.0,
        board.hit.0 + 7,
        "reach guard: the shifted board's ids differ"
    );
    for (viewer, label) in VIEWERS {
        assert!(
            assert_hit_unlocatable(&board, &shifted, viewer, label),
            "{label}: the identity is public while the reflexive waits"
        );
        let payload = transport_payload(board.runner.state(), &board.steps, viewer);
        assert!(
            payload["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["type"] == "CardsRevealed"
                    && event["data"]["card_names"]
                        .as_array()
                        .is_some_and(|names| names.len() == 3)),
            "{label}: reach guard: the reveal event reached the viewer"
        );
    }

    resolve_blast_reflexive(&mut board);
    resolve_blast_reflexive(&mut shifted);
    for (viewer, label) in VIEWERS {
        assert!(
            !assert_hit_unlocatable(&board, &shifted, viewer, label),
            "{label}: the presentation ends with the lease"
        );
    }
}

/// Two Calibrated Blasts on a one-card library reveal the SAME occurrence (a
/// library → library bottom move only reorders), so two live reflexives hold
/// two leases on one occurrence. Releasing either keeps the card revealed
/// until the other leaves the stack.
fn two_blasts_on_one_card() -> (GameRunner, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    let Fixture {
        mut runner,
        spell,
        library,
        extras,
        ..
    } = fixture_with(
        "Calibrated Blast",
        CALIBRATED_BLAST,
        false,
        &[LibCard::Sorcery("Three Drop", ManaCost::generic(3))],
        &[
            (P0, "Calibrated Blast", CALIBRATED_BLAST),
            (P1, "Stifle", STIFLE),
        ],
    );
    let hit = library[0];
    runner.cast(spell).commit();
    assert!(pass_until_prompt_or_empty(&mut runner));
    choose(&mut runner, TargetRef::Player(P1));
    runner.cast(extras[0]).commit();
    assert!(
        pass_until_prompt_or_empty(&mut runner),
        "second reflexive prompt"
    );
    choose(&mut runner, TargetRef::Player(P1));
    let entries = reflexive_entries(runner.state());
    assert_eq!(entries.len(), 2, "reach guard: R1 and R2 both live");
    let rows: Vec<_> = entries
        .iter()
        .map(|entry| runner.state().stack_bound_reveals[entry].clone())
        .collect();
    assert_eq!(
        rows[0], rows[1],
        "both leases hold the identical occurrence"
    );
    (runner, hit, entries, extras)
}

#[test]
fn overlapping_reveal_leases_release_independently_top_first() {
    let (mut runner, hit, entries, _) = two_blasts_on_one_card();
    assert_eq!(
        stack_groups_seen_by(runner.state(), P1),
        vec![entries.clone()],
        "the same revealed card and target: one ×2 group"
    );
    assert_eq!(
        stack_revealed_seen_by(runner.state(), P1),
        entries
            .iter()
            .map(|entry| (*entry, vec!["Three Drop".to_string()]))
            .collect(),
        "both reflexives present the revealed card"
    );
    pass_once_each(&mut runner);
    assert_eq!(
        reflexive_entries(runner.state()),
        vec![entries[0]],
        "R2 resolved"
    );
    assert_eq!(
        stack_revealed_seen_by(runner.state(), P1),
        [(entries[0], vec!["Three Drop".to_string()])].into(),
        "R1 still holds it"
    );
    assert_eq!(
        name_seen_by(runner.state(), P1, hit),
        HIDDEN,
        "CR 401.2: the library object itself stays unlocatable"
    );
    pass_once_each(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert!(stack_revealed_seen_by(runner.state(), P1).is_empty());
    assert_eq!(name_seen_by(runner.state(), P1, hit), HIDDEN);
    assert_eq!(
        runner.state().players[1].life,
        14,
        "reach guard: both dealt 3"
    );
}

#[test]
fn overlapping_reveal_leases_release_independently_bottom_first() {
    let (mut runner, hit, entries, extras) = two_blasts_on_one_card();
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(extras[1]).target_object(entries[0]).commit();
    pass_once_each(&mut runner);
    assert_eq!(
        reflexive_entries(runner.state()),
        vec![entries[1]],
        "R1 countered"
    );
    assert_eq!(
        stack_revealed_seen_by(runner.state(), P1),
        [(entries[1], vec!["Three Drop".to_string()])].into(),
        "R2 still holds it"
    );
    assert_eq!(name_seen_by(runner.state(), P1, hit), HIDDEN);
    pass_once_each(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert!(stack_revealed_seen_by(runner.state(), P1).is_empty());
    assert_eq!(name_seen_by(runner.state(), P1, hit), HIDDEN);
}

/// CR 701.20a: two Calibrated Blast reflexives with the same target but
/// different revealed cards are not the same thing twice. The stack display
/// keeps them apart, so each entry's public reveal (and its different damage)
/// stays visible.
#[test]
fn reflexives_revealing_different_cards_do_not_coalesce_on_the_stack() {
    let Fixture {
        mut runner,
        spell,
        library,
        extras,
        ..
    } = fixture_with(
        "Calibrated Blast",
        CALIBRATED_BLAST,
        false,
        &[
            LibCard::Sorcery("Two Drop", ManaCost::generic(2)),
            LibCard::Sorcery("Three Drop", ManaCost::generic(3)),
        ],
        &[(P0, "Calibrated Blast", CALIBRATED_BLAST)],
    );
    runner.cast(spell).commit();
    assert!(pass_until_prompt_or_empty(&mut runner));
    choose(&mut runner, TargetRef::Player(P1));
    runner.cast(extras[0]).commit();
    assert!(
        pass_until_prompt_or_empty(&mut runner),
        "second reflexive prompt"
    );
    choose(&mut runner, TargetRef::Player(P1));
    let entries = reflexive_entries(runner.state());
    assert_eq!(entries.len(), 2, "reach guard: R1 and R2 both live");
    assert_eq!(
        runner.state().stack_bound_reveals[&entries[0]][0].object_id,
        library[0],
        "reach guard: R1 revealed Two Drop"
    );
    assert_eq!(
        runner.state().stack_bound_reveals[&entries[1]][0].object_id,
        library[1],
        "reach guard: R2 revealed Three Drop"
    );

    for viewer in [P0, P1] {
        assert_eq!(
            stack_revealed_seen_by(runner.state(), viewer),
            [
                (entries[0], vec!["Two Drop".to_string()]),
                (entries[1], vec!["Three Drop".to_string()]),
            ]
            .into(),
            "both reveals are published"
        );
        assert_eq!(
            stack_groups_seen_by(runner.state(), viewer),
            vec![vec![entries[0]], vec![entries[1]]],
            "different reveals do not coalesce"
        );
    }

    pass_until_prompt_or_empty(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().players[1].life,
        15,
        "reach guard: 3 + 2 damage"
    );
}

#[test]
fn every_opponent_sees_the_hit_in_a_three_player_game() {
    let mut scenario = GameScenario::new_n_player(3, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario.add_creature(P0, "Cub", 1, 1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Yuna's Whistle", true, YUNAS_WHISTLE)
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Library Beast", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    assert!(pass_until_prompt_or_empty(&mut runner));
    choose(&mut runner, TargetRef::Object(bear));
    let p2 = engine::types::player::PlayerId(2);
    for viewer in [P1, p2] {
        assert_eq!(name_seen_by(runner.state(), viewer, hit), "Library Beast");
    }
    pass_until_prompt_or_empty(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(p1p1(runner.state(), bear), 2, "reach guard");
    for viewer in [P1, p2] {
        assert_eq!(name_seen_by(runner.state(), viewer, hit), HIDDEN);
    }
}

#[test]
fn a_debug_phase_jump_ends_every_lease() {
    let (mut runner, hit, _, _) = whistle_with_reflexive_on_stack(&[]);
    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(
            engine::types::actions::DebugAction::SetPhase {
                phase: Phase::PreCombatMain,
                active_player: P0,
            },
        ))
        .expect("debug phase jump");
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: the stack was cleared"
    );
    assert!(runner.state().stack_bound_reveals.is_empty());
    assert_eq!(name_seen_by(runner.state(), P1, hit), HIDDEN);
}
