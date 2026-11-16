//! "If C, do A and repeat this process" reached through A's gate.
//!
//! CR 608.2c: an instruction that says to repeat the process is followed again
//! from its start, here only when the clause it trails was reached (its gate
//! held). Cards (verbatim Oracle text): Grist, the Hunger Tide (+1), Cultivator
//! Colossus (#9109), Countryside Crusher, The Tale of Tamiyo.
//!
//! Library contents are forced by card type, never by seed: `K::Insect` is a
//! creature card with the Insect subtype, `K::Other` a sorcery, `K::Land` a land,
//! `K::Creature` a creature without a subtype. Rows drive `GameRunner::activate`
//! / `act` or the trigger-body route (`build_resolved_from_def` +
//! `resolve_ability_chain`) with the real parser output.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{create_object, move_to_zone};
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoMayChoice, GameState, MayTriggerAutoChoiceKey, MayTriggerAutoChoiceScope, MayTriggerOrigin,
    PersistedGameState, PersistedRestoreFinalization, RepeatProcessRequest, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::resolution::ResolutionFrame;
use engine::types::zones::Zone;

use self::K::{Creature, Insect, Land, Other};
use crate::support::shared_card_db;

const GRIST: &str = "As long as Grist isn't on the battlefield, it's a 1/1 Insect creature in addition to its other types.\n+1: Create a 1/1 black and green Insect creature token, then mill a card. If an Insect card was milled this way, put a loyalty counter on Grist and repeat this process.\n\u{2212}2: You may sacrifice a creature. When you do, destroy target creature or planeswalker.\n\u{2212}5: Each opponent loses life equal to the number of creature cards in your graveyard.";
const COLOSSUS: &str = "Trample\nCultivator Colossus's power and toughness are each equal to the number of lands you control.\nWhen this creature enters, you may put a land card from your hand onto the battlefield tapped. If you do, draw a card and repeat this process.";
const CRUSHER: &str = "At the beginning of your upkeep, reveal the top card of your library. If it's a land card, put it into your graveyard and repeat this process.\nWhenever a land card is put into your graveyard from anywhere, put a +1/+1 counter on this creature.";
const TAMIYO: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after IV.)\nI, II, III \u{2014} Mill two cards. If two cards that share a card type were milled this way, draw a card and repeat this process.\nIV \u{2014} Exile any number of target instant, sorcery, and/or Tamiyo planeswalker cards from your graveyard. Copy them. You may cast any number of the copies.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

#[derive(Clone, Copy, Debug)]
enum K {
    Insect,
    Other,
    Land,
    Creature,
}

fn set_kind(state: &mut GameState, id: ObjectId, k: K) {
    let o = state.objects.get_mut(&id).unwrap();
    match k {
        K::Insect => {
            o.card_types.core_types.push(CoreType::Creature);
            o.card_types.subtypes.push("Insect".to_string());
        }
        K::Other => o.card_types.core_types.push(CoreType::Sorcery),
        K::Land => o.card_types.core_types.push(CoreType::Land),
        K::Creature => o.card_types.core_types.push(CoreType::Creature),
    }
    o.base_card_types = o.card_types.clone();
}

/// Retype the top `kinds.len()` cards of P0's library, top first.
fn lib_kinds(runner: &mut GameRunner, kinds: &[K]) {
    for (i, k) in kinds.iter().enumerate() {
        let id = runner.state().players[0].library[i];
        set_kind(runner.state_mut(), id, *k);
    }
}

fn tokens_on_battlefield(st: &GameState) -> usize {
    st.objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield)
        .count()
}

fn in_zone(st: &GameState, zone: Zone) -> usize {
    st.objects.values().filter(|o| o.zone == zone).count()
}

struct GristRun {
    loyalty: u32,
    tokens: usize,
    graveyard: usize,
    exile: usize,
}

/// Grist's +1 through `GameRunner::activate`, over a library forced to `lib` (top first).
fn grist_plus_one(lib: &[K], rest_in_peace: bool) -> GristRun {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let grist = sc
        .add_planeswalker_from_oracle(P0, "Grist, the Hunger Tide", "Grist", 3, GRIST)
        .id();
    if rest_in_peace {
        sc.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    }
    for i in (0..lib.len()).rev() {
        sc.add_card_to_library_top(P0, &format!("Card{i}"));
    }
    let mut runner = sc.build();
    lib_kinds(&mut runner, lib);
    let out = runner.activate(grist, 0).resolve();
    let st = out.state();
    GristRun {
        loyalty: out.counters(grist, CounterType::Loyalty),
        tokens: tokens_on_battlefield(st),
        graveyard: st.players[0].graveyard.len(),
        exile: in_zone(st, Zone::Exile),
    }
}

// G1 + G3: the +1 repeats while the milled card is an Insect card; the Insect
// token it creates is not a milled card (so `[Other]` is one pass).
#[test]
fn grist_plus_one_repeats_while_insect_cards_are_milled() {
    // Loyalty includes the +1 cost: 3 + 1 + one counter per Insect card milled.
    let r = grist_plus_one(&[Insect, Insect, Other], false);
    assert_eq!(
        (r.loyalty, r.tokens, r.graveyard),
        (6, 3, 3),
        "[Insect, Insect, Other]: three passes"
    );
    let r = grist_plus_one(&[Other], false);
    assert_eq!(
        (r.loyalty, r.tokens, r.graveyard),
        (4, 1, 1),
        "[Other]: the Insect token already created is not a milled card; one pass"
    );
    let r = grist_plus_one(&[Insect, Other], false);
    assert_eq!(
        (r.loyalty, r.tokens, r.graveyard),
        (5, 2, 2),
        "[Insect, Other]"
    );
    let r = grist_plus_one(&[Insect, Insect, Insect], false);
    assert_eq!(
        (r.loyalty, r.tokens, r.graveyard),
        (7, 4, 3),
        "[Insect x3]: the fourth pass mills nothing and stops"
    );
}

// G2: CR 701.17c - Rest in Peace exiles the milled cards; they were still milled.
#[test]
fn grist_plus_one_counts_cards_exiled_instead_of_milled() {
    let r = grist_plus_one(&[Insect, Insect, Other], true);
    assert_eq!(
        (r.loyalty, r.tokens, r.graveyard, r.exile),
        (6, 3, 0, 3),
        "Rest in Peace: three passes, all three milled cards exiled"
    );
    let control = grist_plus_one(&[Insect, Insect, Other], false);
    assert_eq!(
        (control.loyalty, control.graveyard, control.exile),
        (6, 3, 0),
        "paired positive: without Rest in Peace the same cards reach the graveyard"
    );
}

// G4: the process keeps going after Grist has left the battlefield (production
// route: the stamped incarnation keeps the loyalty counter off the graveyard object).
#[test]
fn grist_plus_one_continues_after_grist_leaves_the_battlefield() {
    let lib = [Insect, Insect, Other];
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let grist = sc
        .add_planeswalker_from_oracle(P0, "Grist, the Hunger Tide", "Grist", 3, GRIST)
        .id();
    for i in (0..lib.len()).rev() {
        sc.add_card_to_library_top(P0, &format!("Card{i}"));
    }
    let mut runner = sc.build();
    lib_kinds(&mut runner, &lib);
    runner
        .act(GameAction::ActivateAbility {
            source_id: grist,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .unwrap();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the +1 is on the stack"
    );
    move_to_zone(runner.state_mut(), grist, Zone::Graveyard, &mut vec![]);
    runner.resolve_top();
    let st = runner.state();
    assert_eq!(
        tokens_on_battlefield(st),
        3,
        "three passes made three tokens"
    );
    assert_eq!(
        st.players[0].graveyard.len(),
        4,
        "three milled cards plus Grist herself"
    );
    assert_eq!(
        st.objects[&grist].counters.get(&CounterType::Loyalty),
        None,
        "no loyalty counter is put on the graveyard object (CR 400.7)"
    );
}

// G5: needs phase 1 - a milled Grist is an Insect card (CR 113.6c).
#[test]
fn a_milled_grist_is_an_insect_card_for_the_repeat() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let grist = sc
        .add_planeswalker_from_oracle(P0, "Grist, the Hunger Tide", "Grist", 3, GRIST)
        .id();
    sc.add_card_to_library_top(P0, "Other");
    let grist2 = sc
        .add_spell_to_library_top(P0, "Grist, the Hunger Tide", false)
        .from_oracle_text(GRIST)
        .as_planeswalker_with_loyalty("Grist", 3)
        .id();
    let mut runner = sc.build();
    let other = runner.state().players[0].library[1];
    set_kind(runner.state_mut(), other, Other);
    let out = runner.activate(grist, 0).resolve();
    let st = out.state();
    assert_eq!(
        out.counters(grist, CounterType::Loyalty),
        5,
        "the milled Grist counted as an Insect card: two passes"
    );
    assert_eq!(tokens_on_battlefield(st), 2);
    assert_eq!(st.objects[&grist2].zone, Zone::Graveyard);
}

/// A trigger/ability body from verbatim Oracle text, resolved through the chain
/// driver with a library forced to `lib` (top first).
struct BodyRun {
    graveyard: usize,
    hand: usize,
    library: usize,
    idle: bool,
    resolved: bool,
}

fn body_run(text: &str, name: &str, types: &[&str], subs: &[&str], lib: &[K]) -> BodyRun {
    body_run_with(text, name, types, subs, lib, |_| {})
}

fn body_run_with(
    text: &str,
    name: &str,
    types: &[&str],
    subs: &[&str],
    lib: &[K],
    adjust: impl FnOnce(&mut engine::types::ability::AbilityDefinition),
) -> BodyRun {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let mut runner = sc.build();
    let src = {
        let st = runner.state_mut();
        create_object(
            st,
            CardId(st.next_object_id),
            P0,
            name.into(),
            Zone::Battlefield,
        )
    };
    for (i, k) in lib.iter().enumerate() {
        let st = runner.state_mut();
        let id = create_object(
            st,
            CardId(st.next_object_id),
            P0,
            format!("Lib{i}"),
            Zone::Library,
        );
        set_kind(st, id, *k);
    }
    let t: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let s: Vec<String> = subs.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(text, name, &[], &t, &s);
    let mut def = match parsed.triggers.iter().find_map(|t| t.execute.clone()) {
        Some(d) => *d,
        None => parsed.abilities.first().cloned().expect("an ability body"),
    };
    adjust(&mut def);
    let ab = build_resolved_from_def(&def, src, P0);
    let mut events = vec![];
    let res = resolve_ability_chain(runner.state_mut(), &ab, &mut events, 0);
    let st = runner.state();
    BodyRun {
        graveyard: st.players[0].graveyard.len(),
        hand: st.players[0].hand.len(),
        library: st.players[0].library.len(),
        idle: st.resolution_stack.is_empty()
            && matches!(st.waiting_for, WaitingFor::Priority { .. }),
        resolved: res.is_ok(),
    }
}

// G6: Countryside Crusher - the revealed land is put into the graveyard and the
// process repeats; a nonland stops it and stays on top.
#[test]
fn countryside_crusher_repeats_while_the_revealed_card_is_a_land() {
    let crusher = |lib: &[K]| {
        body_run(
            CRUSHER,
            "Countryside Crusher",
            &["Creature"],
            &["Giant", "Warrior"],
            lib,
        )
    };
    let r = crusher(&[Land, Land, Creature]);
    assert!(r.resolved && r.idle);
    assert_eq!(
        (r.graveyard, r.library),
        (2, 1),
        "[Land, Land, Creature]: the creature stays"
    );
    let r = crusher(&[Creature]);
    assert!(r.resolved && r.idle);
    assert_eq!(
        (r.graveyard, r.library),
        (0, 1),
        "paired negative: a nonland first card repeats nothing"
    );
    let r = crusher(&[Land, Land]);
    assert!(
        r.resolved && r.idle,
        "an emptied library terminates the loop"
    );
    assert_eq!(r.graveyard, 2);
    let r = crusher(&[]);
    assert!(r.resolved && r.idle, "an empty library terminates the loop");
}

/// Marks the producer whose direct child is the repeat directive as forwarding
/// its result; true when one was found.
fn forward_into_repeat_directive(def: &mut engine::types::ability::AbilityDefinition) -> bool {
    if def
        .sub_ability
        .as_ref()
        .is_some_and(|child| child.effect.is_repeat_directive())
    {
        def.forward_result = true;
        return true;
    }
    def.sub_ability
        .as_deref_mut()
        .is_some_and(forward_into_repeat_directive)
        || def
            .else_ability
            .as_deref_mut()
            .is_some_and(forward_into_repeat_directive)
}

// CR 608.2c: the repeat directive belongs to the process, so a producer that
// forwards its result to the directive (rebinding a child's source to the moved
// object) must not detach the request from the process it repeats. Countryside
// Crusher's put-into-graveyard is marked `forward_result` (a building-block
// staging: no printed card forwards into its directive today).
#[test]
fn a_forwarding_producer_does_not_detach_the_repeat_request() {
    let mut marked = false;
    let r = body_run_with(
        CRUSHER,
        "Countryside Crusher",
        &["Creature"],
        &["Giant", "Warrior"],
        &[Land, Land, Creature],
        |def| marked = forward_into_repeat_directive(def),
    );
    assert!(
        marked,
        "reach guard: the directive's producer forwards its result"
    );
    assert!(r.resolved && r.idle);
    assert_eq!(
        (r.graveyard, r.library),
        (2, 1),
        "both revealed lands are put into the graveyard; the creature stays"
    );
}

// G7: The Tale of Tamiyo - the draw and the repeat are gated on the two milled cards
// sharing a card type (the base drew unconditionally).
#[test]
fn tale_of_tamiyo_repeats_while_the_milled_cards_share_a_type() {
    let tamiyo = |lib: &[K]| {
        body_run(
            TAMIYO,
            "The Tale of Tamiyo",
            &["Enchantment"],
            &["Saga"],
            lib,
        )
    };
    let r = tamiyo(&[Creature, Creature, Land, Land, Other, Creature]);
    assert!(r.resolved && r.idle);
    assert_eq!(
        (r.graveyard, r.hand, r.library),
        (4, 1, 1),
        "repeats while the milled pair shares a type; stops on Land + Other"
    );
    let r = tamiyo(&[Creature, Land, Creature]);
    assert_eq!(
        (r.graveyard, r.hand),
        (2, 0),
        "Creature + Land share no type: no draw, no repeat"
    );
    let r = tamiyo(&[Land, Land, Other, Land]);
    assert_eq!(
        (r.graveyard, r.hand),
        (3, 1),
        "a one-card final mill does not satisfy \"two cards\""
    );
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Gate {
    Prompted,
    StoredAccept,
    StoredDecline,
}

struct ColossusRun {
    trace: String,
    lands: usize,
    hand: usize,
    library: usize,
    idle: bool,
}

/// Cultivator Colossus's enters trigger body. Trace: `O` = the optional prompt,
/// `Z` = the zone choice among several hand lands. `answers` is consumed one letter
/// per optional prompt (`A` accept, `D` decline; the last letter repeats).
fn colossus_run(
    hand_lands: usize,
    lib: &[K],
    answers: &str,
    gate: Gate,
    stale_slot: bool,
) -> ColossusRun {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let mut runner = sc.build();
    let src = {
        let st = runner.state_mut();
        create_object(
            st,
            CardId(st.next_object_id),
            P0,
            "Cultivator Colossus".into(),
            Zone::Battlefield,
        )
    };
    for i in 0..hand_lands {
        let st = runner.state_mut();
        let id = create_object(
            st,
            CardId(st.next_object_id),
            P0,
            format!("HandLand{i}"),
            Zone::Hand,
        );
        set_kind(st, id, K::Land);
    }
    for (i, k) in lib.iter().enumerate() {
        let st = runner.state_mut();
        let id = create_object(
            st,
            CardId(st.next_object_id),
            P0,
            format!("Lib{i}"),
            Zone::Library,
        );
        set_kind(st, id, *k);
    }
    let parsed = parse_oracle_text(
        COLOSSUS,
        "Cultivator Colossus",
        &["Trample".to_string()],
        &["Creature".to_string()],
        &["Plant".to_string(), "Wall".to_string()],
    );
    let def = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.clone())
        .expect("the enters trigger body");
    let mut ab = build_resolved_from_def(&def, src, P0);
    if gate != Gate::Prompted {
        let origin = MayTriggerOrigin::Printed { trigger_index: 0 };
        runner.state_mut().set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: P0,
                source_id: src,
                origin: origin.clone(),
            },
            if gate == Gate::StoredAccept {
                AutoMayChoice::Accept
            } else {
                AutoMayChoice::Decline
            },
        );
        ab.set_may_trigger_origin_recursive(origin);
    }
    if stale_slot {
        // The stale request names this very process, so only the pass-head
        // clear (never the source scoping) can keep it from being honoured.
        runner.state_mut().repeat_process_requested = Some(RepeatProcessRequest { source_id: src });
    }
    let mut events = vec![];
    resolve_ability_chain(runner.state_mut(), &ab, &mut events, 0).expect("the chain resolves");
    let answers: Vec<char> = answers.chars().collect();
    let mut asked = 0usize;
    let mut trace = String::new();
    for _ in 0..80 {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                let a = answers[asked.min(answers.len() - 1)];
                asked += 1;
                trace.push('O');
                GameAction::DecideOptionalEffect { accept: a == 'A' }
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                trace.push('Z');
                GameAction::SelectCards {
                    cards: vec![cards[0]],
                }
            }
            _ => break,
        };
        runner.act(action).expect("the answer is accepted");
    }
    let st = runner.state();
    let lands = st
        .battlefield
        .iter()
        .filter(|id| {
            st.objects[id]
                .card_types
                .core_types
                .contains(&CoreType::Land)
        })
        .count();
    ColossusRun {
        trace,
        lands,
        hand: st.players[0].hand.len(),
        library: st.players[0].library.len(),
        idle: st.resolution_stack.is_empty()
            && matches!(st.waiting_for, WaitingFor::Priority { .. }),
    }
}

// G8: Cultivator Colossus (#9109) - the "you may" is re-asked every pass (phase 2),
// and the repeat is reached only when a land was put and the card drawn.
#[test]
fn cultivator_colossus_repeats_and_asks_each_pass() {
    // Two lands in hand: each pass also pauses on the zone choice (drain with a choice).
    let r = colossus_run(
        2,
        &[Land, Land, Creature, Creature],
        "AD",
        Gate::Prompted,
        false,
    );
    assert_eq!(
        r.trace, "OZO",
        "accept, choose the land, then asked again and declined"
    );
    assert_eq!((r.lands, r.library, r.hand), (1, 3, 2));
    let r = colossus_run(
        2,
        &[Land, Land, Creature, Creature],
        "A",
        Gate::Prompted,
        false,
    );
    assert!(
        r.trace.starts_with("OZOZOZO"),
        "every pass asks again: {}",
        r.trace
    );
    assert_eq!((r.lands, r.hand, r.library), (4, 2, 0));
    // One land in hand: no zone choice, a prompt per pass.
    let r = colossus_run(1, &[Land, Land, Creature], "AAD", Gate::Prompted, false);
    assert_eq!(r.trace, "OOO", "decline on the third prompt ends the loop");
    assert_eq!((r.lands, r.library), (2, 1));
    // Stored decline: no prompt, no land, nothing drawn.
    let r = colossus_run(1, &[Land, Land, Creature], "A", Gate::StoredDecline, false);
    assert_eq!((r.trace.as_str(), r.lands, r.library), ("", 0, 3));
    // Stored accept: consecutive non-pausing passes run to the end without a prompt.
    let r = colossus_run(1, &[Land, Land, Creature], "A", Gate::StoredAccept, false);
    assert_eq!(r.trace, "", "a stored accept never prompts");
    assert_eq!((r.lands, r.hand, r.library), (3, 1, 0));
    assert!(r.idle);
    // Nothing to put: accepting performs nothing, the "if you do" draw does not run.
    let r = colossus_run(0, &[Land, Land], "A", Gate::Prompted, false);
    assert_eq!(
        (r.lands, r.library),
        (0, 2),
        "no repeat without a performed put"
    );
}

// G9: the request slot is cleared at the head of every pass, so a stale `true`
// (a request left behind by an earlier resolution) cannot re-ask a declined pass.
#[test]
fn a_stale_repeat_request_does_not_survive_a_declined_pass() {
    let r = colossus_run(1, &[Land, Land, Creature], "D", Gate::Prompted, true);
    assert_eq!(
        r.trace, "O",
        "declined at the first prompt: no second prompt"
    );
    assert_eq!(r.lands, 0);
    let control = colossus_run(
        2,
        &[Land, Land, Creature, Creature],
        "AD",
        Gate::Prompted,
        false,
    );
    assert_eq!(
        control.trace, "OZO",
        "paired positive: the slot is set and read in a real pass"
    );
}

/// G13: a milled Grist counts at -5 through the real mill path, and the +1 keeps
/// repeating through her (phase 1 + phase 3).
fn grist_plus_one_then_minus_five(
    second_grist: bool,
    strip_second_static: bool,
) -> (u32, usize, i32, u32, Zone) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let grist = sc
        .add_planeswalker_from_oracle(P0, "Grist, the Hunger Tide", "Grist", 3, GRIST)
        .id();
    sc.add_card_to_library_top(P0, "Llanowar Elves");
    let mut grist2 = None;
    if second_grist {
        grist2 = Some(
            sc.add_spell_to_library_top(P0, "Grist, the Hunger Tide", false)
                .from_oracle_text(GRIST)
                .as_planeswalker_with_loyalty("Grist", 3)
                .id(),
        );
    }
    sc.add_card_to_library_top(P0, "Haywire Mite");
    let mut runner = sc.build();
    let lib = runner.state().players[0].library.clone();
    set_kind(runner.state_mut(), lib[0], Insect);
    set_kind(runner.state_mut(), *lib.last().unwrap(), Creature);
    if strip_second_static {
        if let Some(g) = grist2 {
            runner
                .state_mut()
                .objects
                .get_mut(&g)
                .unwrap()
                .static_definitions = Default::default();
        }
    }
    let out = runner.activate(grist, 0).resolve();
    let after_plus_loyalty = out.counters(grist, CounterType::Loyalty);
    let after_plus_graveyard = out.state().players[0].graveyard.len();
    drop(out);
    // The second activation of the turn is only legal with the per-object count reset.
    runner
        .state_mut()
        .objects
        .get_mut(&grist)
        .unwrap()
        .loyalty_activations_this_turn = 0;
    let out = runner.activate(grist, 2).resolve();
    (
        after_plus_loyalty,
        after_plus_graveyard,
        out.state().players[1].life,
        out.counters(grist, CounterType::Loyalty),
        out.state().objects[&grist].zone,
    )
}

#[test]
fn a_milled_grist_repeats_the_plus_one_and_counts_at_minus_five() {
    // Library [Haywire Mite (Insect creature), Grist, Llanowar Elves (creature)].
    let (loyalty_plus, gy_plus, life, loyalty, zone) = grist_plus_one_then_minus_five(true, false);
    assert_eq!(
        (loyalty_plus, gy_plus),
        (6, 3),
        "the +1 repeats through the milled Grist (phase 3 over phase 1)"
    );
    assert_eq!(
        (life, loyalty, zone),
        (17, 1, Zone::Battlefield),
        "-5 counts Mite, the milled Grist and Elves (phase 1): 3 life lost, Grist survives on 1"
    );
    // Phase 1 control: the milled Grist's static removed, so it is not a creature card.
    let (loyalty_plus, gy_plus, life, loyalty, _) = grist_plus_one_then_minus_five(true, true);
    assert_eq!(
        (loyalty_plus, gy_plus),
        (5, 2),
        "without the static the milled Grist is no Insect card: the +1 stops there"
    );
    assert_eq!((life, loyalty), (18, 0), "and she does not count at -5");
    // Paired positive: no second Grist.
    let (loyalty_plus, _, life, loyalty, _) = grist_plus_one_then_minus_five(false, false);
    assert_eq!(loyalty_plus, 5);
    assert_eq!(
        (life, loyalty),
        (17, 0),
        "Mite, Elves and the dying Grist herself"
    );
}

// Production CardDB consumers. The direct-body rows above are supplementary
// building-block tests. Regenerate integration_cards.json.gz from the fresh
// production export with scripts/gen-test-fixture.py (including canonical gzip).
fn colossus_scenario(
    hand_lands: usize,
    library: &[&str],
) -> (GameScenario, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    let db = shared_card_db().expect("production repeat fixtures must load");
    assert_eq!(
        db.get_face_by_name("Cultivator Colossus")
            .unwrap()
            .oracle_text
            .as_deref(),
        Some(COLOSSUS)
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let source = scenario.add_real_card(P0, "Cultivator Colossus", Zone::Hand, db);
    let hand = (0..hand_lands)
        .map(|_| scenario.add_real_card(P0, "Forest", Zone::Hand, db))
        .collect();
    let library = library
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Library, db))
        .collect();
    scenario.with_mana_pool(
        P0,
        (0..40)
            .flat_map(|_| [ManaType::Green, ManaType::Blue])
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    (scenario, source, hand, library)
}

fn cast_colossus_etb(runner: &mut GameRunner, source: ObjectId) {
    runner.cast(source).commit();
    assert_eq!(runner.state().stack.len(), 1);
    runner.act(GameAction::PassPriority).expect("caster passes");
    runner
        .act(GameAction::PassPriority)
        .expect("opponent passes to resolve the permanent");
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the actual ETB trigger was collected"
    );
    assert_eq!(runner.state().stack.last().unwrap().source_id, source);
    runner.resolve_top();
}

fn colossus_prompt_key(runner: &GameRunner, source: ObjectId) -> MayTriggerAutoChoiceKey {
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice {
            player,
            source_id,
            may_trigger_key: Some(key),
            ..
        } => {
            assert_eq!((*player, *source_id), (P0, source));
            assert_eq!((key.player, key.source_id), (P0, source));
            assert!(matches!(key.origin, MayTriggerOrigin::Definition { .. }));
            key.clone()
        }
        other => panic!("expected the live Colossus optional prompt, got {other:?}"),
    }
}

fn assert_repeat_idle(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        matches!(&state.waiting_for, WaitingFor::Priority { player: P0 })
            && state.stack.is_empty()
            && state.resolution_stack.is_empty(),
        "waiting_for={:?}, spell_stack_len={}, resolution_stack_len={}",
        state.waiting_for,
        state.stack.len(),
        state.resolution_stack.len()
    );
}

#[test]
fn colossus_real_etb_remembered_accept_26_puts() {
    let mut names = vec!["Forest"; 25];
    names.extend(["Shock", "Lightning Bolt"]);
    let (scenario, source, hand, library) = colossus_scenario(1, &names);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), shared_card_db().unwrap());
    cast_colossus_etb(&mut runner, source);
    colossus_prompt_key(&runner, source);
    assert_eq!(
        runner.state().players[0]
            .hand
            .iter()
            .filter(|id| runner.state().objects[id]
                .card_types
                .core_types
                .contains(&CoreType::Land))
            .count(),
        1,
        "the synchronous segment has exactly one eligible land"
    );
    let draws_before = runner.state().players[0].cards_drawn_this_turn;
    runner
        .act(GameAction::DecideOptionalEffectAndRemember {
            choice: AutoMayChoice::Accept,
            scope: MayTriggerAutoChoiceScope::ExactInstance,
        })
        .expect("26 synchronous passes finish");
    // CR 608.2c + CR 608.2d: the initial land and 25 drawn lands each
    // produce one draw; a nonland draw leaves the next put infeasible.
    let put: Vec<_> = hand.iter().chain(library.iter().take(25)).collect();
    assert_eq!(put.len(), 26);
    for id in put {
        assert_eq!(runner.state().objects[id].zone, Zone::Battlefield);
        assert!(runner.state().objects[id].tapped);
    }
    assert_eq!(
        runner.state().players[0].cards_drawn_this_turn - draws_before,
        26
    );
    assert_eq!(runner.state().objects[&library[25]].zone, Zone::Hand);
    assert_eq!(
        runner.state().players[0]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![library[26]]
    );
    assert_repeat_idle(&runner);
}

#[test]
fn colossus_real_etb_prompted_inner_pause_and_decline() {
    for (lands, answers, expected_puts) in [(2, "AD", 1), (1, "AAD", 2), (1, "D", 0), (0, "D", 0)] {
        let (scenario, source, hand, library) =
            colossus_scenario(lands, &["Forest", "Forest", "Shock"]);
        let mut runner = scenario.build();
        engine::game::rehydrate_game_from_card_db(runner.state_mut(), shared_card_db().unwrap());
        cast_colossus_etb(&mut runner, source);
        if lands == 0 {
            assert!(
                runner.state().players[0].hand.is_empty(),
                "zero-hand/no-eligible-land reach guard"
            );
        }
        for answer in answers.chars() {
            colossus_prompt_key(&runner, source);
            runner
                .act(GameAction::DecideOptionalEffect {
                    accept: answer == 'A',
                })
                .unwrap();
            if lands == 2 && answer == 'A' {
                let cards = match &runner.state().waiting_for {
                    WaitingFor::EffectZoneChoice { player, cards, .. } => {
                        assert_eq!(*player, P0);
                        assert_eq!(cards.len(), 2);
                        assert!(hand.iter().all(|id| cards.contains(id)));
                        cards.clone()
                    }
                    other => panic!("accepting with two eligible lands must pause: {other:?}"),
                };
                assert!(
                    runner.state().resolution_stack.len() >= 2,
                    "the loop owner remains below the choice tail"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .unwrap();
            }
        }
        let additional_lands = runner
            .state()
            .battlefield
            .iter()
            .filter(|id| {
                runner.state().objects[id]
                    .card_types
                    .core_types
                    .contains(&CoreType::Land)
            })
            .count()
            - 1;
        assert_eq!(additional_lands, expected_puts);
        assert_eq!(
            runner.state().players[0].cards_drawn_this_turn,
            expected_puts as u32
        );
        assert_eq!(
            runner.state().players[0].library.len(),
            library.len() - expected_puts
        );
        eprintln!(
            "Colossus completion case: lands={lands}, answers={answers}, expected_puts={expected_puts}"
        );
        assert_repeat_idle(&runner);
    }
}

#[test]
fn colossus_real_etb_exact_instance_decline_reprompts_after_reentry() {
    let (mut scenario, source, hand, library) = colossus_scenario(1, &["Forest", "Shock"]);
    let db = shared_card_db().unwrap();
    let capsize = scenario.add_real_card(P0, "Capsize", Zone::Hand, db);
    let sibling = scenario.add_real_card(P0, "Cultivator Colossus", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    cast_colossus_etb(&mut runner, source);
    let saved = colossus_prompt_key(&runner, source);
    runner
        .act(GameAction::DecideOptionalEffectAndRemember {
            choice: AutoMayChoice::Decline,
            scope: MayTriggerAutoChoiceScope::ExactInstance,
        })
        .unwrap();
    assert_repeat_idle(&runner);
    runner.cast(capsize).target_object(source).resolve();
    assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
    cast_colossus_etb(&mut runner, source);
    let reentered = colossus_prompt_key(&runner, source);
    // CR 400.7 + CR 603.5: reentry produces a new trigger-source occurrence.
    assert_ne!(reentered.origin, saved.origin);
    assert_eq!(reentered.source_id, saved.source_id);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert_repeat_idle(&runner);
    cast_colossus_etb(&mut runner, sibling);
    let sibling_key = colossus_prompt_key(&runner, sibling);
    assert_ne!(sibling_key.source_id, saved.source_id);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert_eq!(runner.state().objects[&hand[0]].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].cards_drawn_this_turn, 0);
    assert_eq!(runner.state().players[0].library.len(), library.len());
    assert_repeat_idle(&runner);
}

#[test]
fn crusher_real_upkeep_repeats_and_publishes_counters() {
    let db = shared_card_db().expect("production repeat fixtures must load");
    assert_eq!(
        db.get_face_by_name("Countryside Crusher")
            .unwrap()
            .oracle_text
            .as_deref(),
        Some(CRUSHER)
    );
    for names in [vec!["Forest", "Island", "Shock"], vec!["Shock"], vec![]] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        let crusher = scenario.add_real_card(P0, "Countryside Crusher", Zone::Battlefield, db);
        let library: Vec<_> = names
            .iter()
            .map(|name| scenario.add_real_card(P0, name, Zone::Library, db))
            .collect();
        let mut runner = scenario.build();
        engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
        runner.advance_to_upkeep();
        // CR 603.2b: phase advancement collects the real upkeep trigger.
        assert_eq!(runner.state().phase, Phase::Upkeep);
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack.last().unwrap().source_id, crusher);
        runner.act(GameAction::PassPriority).unwrap();
        runner.act(GameAction::PassPriority).unwrap();
        let milled = if names.len() == 3 { 2 } else { 0 };
        assert_eq!(runner.state().players[0].graveyard.len(), milled);
        if milled > 0 {
            assert_eq!(
                runner.state().stack.len(),
                2,
                "each land created its separate counter trigger"
            );
            assert_eq!(runner.state().players[0].library[0], library[2]);
        }
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&crusher]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            milled as u32
        );
        assert_repeat_idle(&runner);
    }
}

#[test]
fn crusher_real_upkeep_sources_do_not_borrow_prior_revealed_lands() {
    let db = shared_card_db().expect("production repeat fixtures must load");
    assert_eq!(
        db.get_face_by_name("Countryside Crusher")
            .unwrap()
            .oracle_text
            .as_deref(),
        Some(CRUSHER)
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let crushers = [
        scenario.add_real_card(P0, "Countryside Crusher", Zone::Battlefield, db),
        scenario.add_real_card(P0, "Countryside Crusher", Zone::Battlefield, db),
    ];
    let library: Vec<_> = ["Forest", "Island", "Shock", "Lightning Bolt"]
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Library, db))
        .collect();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.advance_to_upkeep();
    // CR 603.2b + CR 603.3b: both real upkeep triggers reach the stack;
    // answer an ordering prompt if their order is not automatically settled.
    assert_eq!(runner.state().phase, Phase::Upkeep);
    if let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for {
        assert_eq!(*player, P0);
        assert_eq!(triggers.len(), 2);
        for source in crushers {
            assert_eq!(triggers.iter().filter(|t| t.source_id == source).count(), 1);
        }
        let order = (0..triggers.len()).rev().collect();
        runner.act(GameAction::OrderTriggers { order }).unwrap();
    }
    assert_eq!(runner.state().stack.len(), 2);
    let first = runner.state().stack[1].source_id;
    let second = runner.state().stack[0].source_id;
    assert_ne!(first, second);
    assert!(crushers.contains(&first) && crushers.contains(&second));
    runner.act(GameAction::PassPriority).unwrap();
    let first_result = runner.act(GameAction::PassPriority).unwrap();
    // CR 608.2c: this source's process reveals and moves only its two lands,
    // then stops on the nonland before the other source's upkeep resolves.
    let revealed: Vec<_> = first_result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        revealed,
        vec![vec![library[0]], vec![library[1]], vec![library[2]]]
    );
    assert_eq!(runner.state().players[0].graveyard.len(), 2);
    assert_eq!(runner.state().objects[&library[0]].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&library[1]].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].library[0], library[2]);

    // CR 603.2c + CR 603.3b: each source observes both graveyard events.
    if let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for {
        assert_eq!(*player, P0);
        assert_eq!(triggers.len(), 4);
        for source in crushers {
            assert_eq!(triggers.iter().filter(|t| t.source_id == source).count(), 2);
        }
        let order = (0..triggers.len()).rev().collect();
        runner.act(GameAction::OrderTriggers { order }).unwrap();
    }
    assert_eq!(runner.state().stack.len(), 5);
    assert_eq!(runner.state().stack[0].source_id, second);
    for source in crushers {
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .skip(1)
                .filter(|entry| entry.source_id == source)
                .count(),
            2
        );
    }
    for remaining in (1..=4).rev() {
        assert_eq!(runner.state().stack.len(), remaining + 1);
        runner.act(GameAction::PassPriority).unwrap();
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, second);
    runner.act(GameAction::PassPriority).unwrap();
    let second_result = runner.act(GameAction::PassPriority).unwrap();
    // CR 608.2c + CR 701.20b: the second source really reveals the nonland;
    // the first source's earlier lands cannot satisfy this source's gate.
    let revealed: Vec<_> = second_result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(revealed, vec![vec![library[2]]]);
    assert_eq!(runner.state().players[0].graveyard.len(), 2);
    assert_eq!(
        runner.state().players[0]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        library[2..]
    );
    for source in crushers {
        assert_eq!(
            runner.state().objects[&source]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(2),
            "both counter observers retain their own source"
        );
    }
    assert_repeat_idle(&runner);
}

#[test]
fn tamiyo_real_chapter_repeats_only_for_shared_pair() {
    let db = shared_card_db().expect("production repeat fixtures must load");
    assert_eq!(
        db.get_face_by_name("The Tale of Tamiyo")
            .unwrap()
            .oracle_text
            .as_deref(),
        Some(TAMIYO)
    );
    for (names, milled, chapter_draws) in [
        (
            vec![
                "Llanowar Elves",
                "Llanowar Elves",
                "Forest",
                "Island",
                "Shock",
                "Llanowar Elves",
            ],
            4,
            1,
        ),
        (vec!["Llanowar Elves", "Forest", "Shock"], 2, 0),
        (vec!["Forest", "Island", "Shock", "Forest"], 3, 1),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        let saga = scenario.add_real_card(P0, "The Tale of Tamiyo", Zone::Battlefield, db);
        let normal_draw = scenario.add_real_card(P0, "Lightning Bolt", Zone::Library, db);
        let library: Vec<_> = names
            .iter()
            .map(|name| scenario.add_real_card(P0, name, Zone::Library, db))
            .collect();
        let mut runner = scenario.build();
        engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
        let lore_before = runner.state().objects[&saga]
            .counters
            .get(&CounterType::Lore)
            .copied()
            .unwrap_or(0);
        runner.advance_to_phase(Phase::PreCombatMain);
        // CR 714.3c: the precombat-main turn action adds lore and dispatches
        // the next real chapter. Normal draw is separate from chapter output.
        assert_eq!(runner.state().phase, Phase::PreCombatMain);
        assert_eq!(
            runner.state().objects[&saga]
                .counters
                .get(&CounterType::Lore)
                .copied()
                .unwrap_or(0),
            lore_before + 1
        );
        assert_eq!(runner.state().objects[&normal_draw].zone, Zone::Hand);
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack.last().unwrap().source_id, saga);
        runner.resolve_top();
        assert_eq!(runner.state().players[0].graveyard.len(), milled);
        assert_eq!(
            runner.state().players[0].cards_drawn_this_turn,
            chapter_draws + 1
        );
        assert_eq!(
            runner.state().players[0].library.len(),
            library.len() - milled - chapter_draws as usize
        );
        if names.len() == 6 {
            assert_eq!(runner.state().players[0].library[0], library[5]);
        }
        assert_repeat_idle(&runner);
    }
}

fn saved_repeat_state(runner: &GameRunner, trusted: bool) -> serde_json::Value {
    let persisted = if trusted {
        PersistedGameState::capture(runner.state().clone())
    } else {
        PersistedGameState::Raw(Box::new(runner.state().clone()))
    };
    serde_json::to_value(persisted).expect("live continuation serializes")
}

fn restore_repeat_state(value: serde_json::Value) -> GameRunner {
    let db = shared_card_db().expect("restore requires the candidate production fixture");
    let state = serde_json::from_value::<PersistedGameState>(value)
        .expect("historical continuation decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("historical continuation prepares")
        .finalize_after_rehydration(|state| {
            engine::game::rehydrate_game_from_card_db(state, db);
            Ok(())
        })
        .expect("historical continuation finalizes after rehydration");
    GameRunner::from_state(state)
}

#[test]
fn legacy_optional_repeat_carriers_restore_and_resume() {
    let (scenario, source, hand, library) = colossus_scenario(2, &["Shock", "Lightning Bolt"]);
    let mut original = scenario.build();
    engine::game::rehydrate_game_from_card_db(original.state_mut(), shared_card_db().unwrap());
    cast_colossus_etb(&mut original, source);
    let origin = colossus_prompt_key(&original, source);
    assert!(matches!(
        original.state().resolution_stack.last(),
        Some(ResolutionFrame::OptionalEffect(_))
    ));
    for trusted in [false, true] {
        for accept in [false, true] {
            let mut value = saved_repeat_state(&original, trusted);
            let game = if trusted {
                &mut value["state"]
            } else {
                &mut value
            };
            let frames = game["resolution_frames"]["frames"].as_array_mut().unwrap();
            let owner_index = frames
                .iter()
                .position(|frame| frame["type"] == "RepeatUntil")
                .expect("live loop owner");
            let template = frames[owner_index]["data"]["ability"].clone();
            let carrier = &mut frames
                .iter_mut()
                .find(|frame| frame["type"] == "OptionalEffect")
                .unwrap()["data"]["ability"];
            // The historical optional frame carried the full loop above its
            // parked owner, which alone drives progression (CR 608.2c);
            // retain the live engine-issued source origin and object targets.
            carrier["repeat_until"] = template["repeat_until"].clone();
            let mut restored = restore_repeat_state(value);
            assert_eq!(colossus_prompt_key(&restored, source), origin);
            restored
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap();
            if accept {
                match &restored.state().waiting_for {
                    WaitingFor::EffectZoneChoice { player, cards, .. } => {
                        assert_eq!(*player, P0);
                        assert!(hand.iter().all(|id| cards.contains(id)));
                    }
                    other => {
                        panic!("restored accept must reach the real inner choice: {other:?}")
                    }
                }
                restored
                    .act(GameAction::SelectCards {
                        cards: vec![hand[0]],
                    })
                    .unwrap();
                assert_eq!(colossus_prompt_key(&restored, source), origin);
                restored
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .unwrap();
            }
            assert_eq!(
                restored.state().players[0].cards_drawn_this_turn,
                u32::from(accept)
            );
            assert_eq!(
                restored.state().players[0].library.len(),
                library.len() - usize::from(accept)
            );
            assert_eq!(
                restored.state().objects[&hand[0]].zone,
                if accept {
                    Zone::Battlefield
                } else {
                    Zone::Hand
                }
            );
            assert_repeat_idle(&restored);
        }
    }
}
