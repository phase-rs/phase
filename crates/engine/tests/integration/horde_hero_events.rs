//! Turtle Team-Up hero-choice Events through the production wave pipeline.
//!
//! The `game::boss` unit tests pin each Event's offer/apply semantics through
//! the driver directly; these tests enter every prompt through its producer
//! (the Boss Action wave reveal), answer through `runner.act`, and assert the
//! downstream effect plus the completion boundary (Priority + empty stack +
//! spent budget). Synthetic event-deck tokens (name-keyed, like the real
//! loader emits) keep the rig deterministic without the card DB.
//!
//! Seat 0 = Horde, seats 1-2 = heroes.

use engine::game::combat::AttackTarget;
use engine::game::deck_loading::{load_deck_into_state, DeckPayload};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::format::{ChallengeDeck, FormatConfig};
use engine::types::game_state::{GameState, ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};
use engine::types::TargetRef;

const HORDE: PlayerId = PlayerId(0);
const HERO1: PlayerId = PlayerId(1);
const HERO2: PlayerId = PlayerId(2);

/// A Turtle Team-Up game with stage 1 deployed (via the public deck loader),
/// parked at the Horde's upkeep on turn 4. The caller rigs the Horde library
/// top-first, stages hero boards, then opens the wave with
/// [`begin_wave_with_budget`].
fn hero_event_game() -> GameState {
    let mut state = GameState::new(
        FormatConfig::horde(ChallengeDeck::TurtleTeamUpHorde.default_ruleset()),
        3,
        42,
    );
    load_deck_into_state(&mut state, &DeckPayload::default());
    state.turn_number = 4;
    state.active_player = HORDE;
    state.phase = Phase::Upkeep;
    state.priority_player = HORDE;
    state.waiting_for = WaitingFor::Priority { player: HORDE };
    assert_eq!(
        state.format_config.archenemy_player(),
        Some(HORDE),
        "seat 0 is the Horde"
    );
    state
}

/// A synthetic event-deck token on the Horde library. Creation order is
/// top-first (`add_to_zone` pushes to the back; the wave reads the front).
/// Name-keyed dispatch reads only the name + Event typing.
fn add_library_event(state: &mut GameState, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, HORDE, name.to_string(), Zone::Library);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.is_token = true;
    obj.is_horde_deck_token = true;
    obj.card_types.core_types.push(CoreType::Event);
    obj.base_card_types = obj.card_types.clone();
    id
}

fn add_hero_creature(state: &mut GameState, owner: PlayerId, name: &str, power: i32) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.controller = owner;
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(power);
    obj.toughness = Some(2);
    obj.base_power = Some(power);
    obj.base_toughness = Some(2);
    id
}

fn add_hand_card(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    create_object(state, card_id, owner, name.to_string(), Zone::Hand)
}

/// Face-up Boss cards in the command zone (stage deployment comes from the
/// deck loader; this scan avoids the `pub(crate)` boss helper).
fn deployed_bosses(state: &GameState) -> Vec<ObjectId> {
    state
        .objects
        .iter()
        .filter(|(_, o)| {
            o.zone == Zone::Command
                && !o.face_down
                && o.card_types.core_types.contains(&CoreType::Boss)
        })
        .map(|(id, _)| *id)
        .collect()
}

fn seat_life(state: &GameState, seat: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == seat)
        .expect("seat")
        .life
}

fn in_pile(state: &GameState, seat: PlayerId, id: ObjectId, pile: Zone) -> bool {
    let player = state.players.iter().find(|p| p.id == seat).expect("seat");
    match pile {
        Zone::Graveyard => player.graveyard.contains(&id),
        Zone::Hand => player.hand.contains(&id),
        Zone::Library => player.library.contains(&id),
        Zone::Exile => state.exile.contains(&id),
        _ => panic!("pile assert only covers gy/hand/library/exile"),
    }
}

/// Enter the Horde's precombat main (firing `begin_wave`) and set an exact
/// wave budget, so each test reveals exactly what it rigged.
fn begin_wave_with_budget(runner: &mut GameRunner, budget: u32) {
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.state_mut().horde_wave_remaining = budget;
}

/// Drive the wave to completion (bounded): pass priority through, answering
/// each HeroEventChoice prompt from `answers` in order. The closure runs on
/// every prompt BEFORE its answer is submitted, so each test asserts the full
/// prompt shape (actor, kind, offer, count) unconditionally; any other prompt
/// panics, and leftover answers fail — a prompt that never fires is a failure,
/// not a vacuous pass.
fn drive_wave_with_answers(
    runner: &mut GameRunner,
    answers: &mut Vec<Vec<ObjectId>>,
    mut on_prompt: impl FnMut(&mut GameRunner),
) {
    for _ in 0..80 {
        let done = {
            let st = runner.state();
            st.horde_wave_remaining == 0
                && st.stack.is_empty()
                && matches!(st.waiting_for, WaitingFor::Priority { .. })
        };
        if done {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::HeroEventChoice { .. } => {
                assert!(
                    !answers.is_empty(),
                    "unexpected HeroEventChoice prompt: {:?}",
                    runner.state().waiting_for
                );
                on_prompt(runner);
                let answer = answers.remove(0);
                runner
                    .act(GameAction::SelectCards { cards: answer })
                    .expect("the planned answer must be legal");
            }
            other => panic!("unexpected prompt during the wave: {other:?}"),
        }
    }
    assert!(
        answers.is_empty(),
        "every planned prompt must fire during the wave"
    );
}

/// The completion boundary every wave test ends at: Priority, an empty stack,
/// and a spent budget.
fn assert_wave_settled(runner: &GameRunner) {
    let st = runner.state();
    assert!(
        matches!(st.waiting_for, WaitingFor::Priority { .. }),
        "the wave settles back to Priority, got {:?}",
        st.waiting_for
    );
    assert!(st.stack.is_empty(), "the stack is empty after the wave");
    assert_eq!(st.horde_wave_remaining, 0, "the wave budget is spent");
}

#[test]
fn wave_prompts_hero_event_then_continues() {
    let mut state = hero_event_game();
    let bear = add_hero_creature(&mut state, HERO1, "Hero Bear", 2);
    let griffin = add_hero_creature(&mut state, HERO1, "Hero Griffin", 2);
    for id in [bear, griffin] {
        state.objects.get_mut(&id).unwrap().tapped = true;
    }
    add_library_event(&mut state, "Event: Chrome Dome's Overload");
    add_library_event(&mut state, "Event: Bebop's Rampage");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 2);
    let mut answers = vec![vec![bear]];
    drive_wave_with_answers(&mut runner, &mut answers, |runner| {
        let WaitingFor::HeroEventChoice {
            player,
            count,
            up_to,
            cards,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Overload must prompt, got {:?}", runner.state().waiting_for);
        };
        assert_eq!(*player, HERO1);
        assert_eq!(*count, 1);
        assert!(!*up_to);
        assert_eq!(cards.len(), 2, "both tapped creatures are offered");
    });
    assert_wave_settled(&runner);
    let st = runner.state();
    assert!(
        in_pile(st, HERO1, bear, Zone::Graveyard),
        "the chosen creature is sacrificed"
    );
    assert_eq!(
        st.objects.get(&griffin).expect("griffin").damage_marked,
        2,
        "the wave continued into Rampage, dealing 2 to the survivor"
    );
}

/// R5-#7 — CR 702.26b: phased-out permanents are treated as though they
/// don't exist, so hero-event offers never include them. HERO1's only tapped
/// creature phases out through the production phasing authority; the Chrome
/// Dome's Overload wave finds nothing eligible, prompts nobody, and the
/// phased-out creature survives (it must not be auto-sacrificed).
///
/// Revert guard: without the phased-out filter the lone creature is
/// auto-sacrificed with no prompt — the zone assertion flips.
#[test]
fn chrome_overload_skips_phased_out_hero_creature() {
    use engine::game::game_object::PhaseOutCause;
    use engine::game::phasing::phase_out_object;

    let mut state = hero_event_game();
    let bear = add_hero_creature(&mut state, HERO1, "Hero Bear", 2);
    state.objects.get_mut(&bear).unwrap().tapped = true;
    add_library_event(&mut state, "Event: Chrome Dome's Overload");

    // Real phasing through the production authority (the Teferi's Protection
    // class), not a status flip: cascade, status, and events all run.
    let mut phase_events = Vec::new();
    let phased = phase_out_object(&mut state, bear, PhaseOutCause::Directly, &mut phase_events);
    assert!(
        phased.contains(&bear),
        "reach-guard: the bear really phased out"
    );

    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    // No prompts expected: the phased-out bear is invisible to the offer, so
    // both heroes Skip — any prompt panics via the driver's guard.
    let mut answers = vec![];
    drive_wave_with_answers(&mut runner, &mut answers, |_| {
        panic!("no hero-event prompt may fire for a phased-out creature");
    });
    assert_wave_settled(&runner);

    let obj = runner.state().objects.get(&bear).expect("bear survives");
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "the phased-out bear must not be sacrificed"
    );
    assert!(obj.is_phased_out(), "the bear is still phased out");
}

/// R6-#2 — the Smackdown half of the phased-out exclusion: a phased-out
/// 5-power creature must not set the greatest-power selection or the bosses'
/// life loss. The two phased-in 2/2s tie, so the captain picks from those two
/// only; the bosses lose exactly 2 and the phased-out creature survives.
///
/// Revert guard: without the filter the lone phased-out 5 is the max and
/// auto-picks with no prompt — the planned answer never fires.
#[test]
fn smackdown_ignores_phased_out_greatest_power() {
    use engine::game::game_object::PhaseOutCause;
    use engine::game::phasing::phase_out_object;

    let mut state = hero_event_game();
    let big = add_hero_creature(&mut state, HERO1, "Phased Brute", 5);
    let cub1 = add_hero_creature(&mut state, HERO1, "Hero Cub One", 2);
    let cub2 = add_hero_creature(&mut state, HERO2, "Hero Cub Two", 2);
    add_library_event(&mut state, "Event: Leatherhead's Smackdown");

    let mut phase_events = Vec::new();
    let phased = phase_out_object(&mut state, big, PhaseOutCause::Directly, &mut phase_events);
    assert!(phased.contains(&big), "reach-guard: the brute phased out");

    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    // Snapshot after the advance: the deployed Krang's upkeep trigger ("the
    // bosses lose 2 life") fires during it, before the prompt is answered.
    let boss_life = seat_life(runner.state(), HORDE);
    let mut answers = vec![vec![cub1]];
    drive_wave_with_answers(&mut runner, &mut answers, |runner| {
        let WaitingFor::HeroEventChoice { cards, count, .. } = &runner.state().waiting_for else {
            panic!(
                "Smackdown must prompt, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*count, 1);
        assert_eq!(
            *cards,
            vec![cub1, cub2],
            "only the tied phased-in 2/2s are offered, not the phased-out 5"
        );
    });
    assert_wave_settled(&runner);

    let st = runner.state();
    assert!(
        in_pile(st, HERO1, cub1, Zone::Graveyard),
        "the picked 2/2 is destroyed"
    );
    assert_eq!(
        seat_life(st, HORDE),
        boss_life - 2,
        "the bosses lose the picked creature's power, not the phased-out 5"
    );
    let obj = st.objects.get(&big).expect("brute survives");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.is_phased_out(), "the brute is still phased out");
}

#[test]
fn krang_team_discard_rejects_overcount_then_completes() {
    let mut state = hero_event_game();
    let a1 = add_hand_card(&mut state, HERO1, "Alpha One");
    let a2 = add_hand_card(&mut state, HERO1, "Alpha Two");
    let b1 = add_hand_card(&mut state, HERO2, "Bravo One");
    add_library_event(&mut state, "Event: Krang's Stratagem");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    let mut answers = vec![vec![a1]];
    drive_wave_with_answers(&mut runner, &mut answers, |runner| {
        let WaitingFor::HeroEventChoice {
            player,
            count,
            up_to,
            min,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "Stratagem must prompt, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*player, *count, *up_to), (HERO1, 2, true));
        // R5-#6: HERO2 can still discard 1, so HERO1 owes at least
        // 2 - 1 = 1 — the team total cannot fall short.
        assert_eq!(
            *min, 1,
            "the first hero's minimum covers what later heroes cannot"
        );
        // An over-count answer is rejected and must not consume the prompt.
        let bad = runner.act(GameAction::SelectCards {
            cards: vec![a1, a2, b1],
        });
        assert!(
            bad.is_err(),
            "three cards against an up-to-2 prompt is illegal"
        );
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::HeroEventChoice { .. }
            ),
            "a rejected answer leaves the prompt pending"
        );
        // An under-minimum answer is likewise refused: choosing none here
        // would strand the team at one discard after HERO2 auto-covers.
        let empty = runner.act(GameAction::SelectCards { cards: vec![] });
        assert!(
            empty.is_err(),
            "an empty answer against a minimum-1 prompt is illegal"
        );
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::HeroEventChoice { .. }
            ),
            "a refused answer leaves the prompt pending"
        );
    });
    assert_wave_settled(&runner);
    let st = runner.state();
    assert!(in_pile(st, HERO1, a1, Zone::Graveyard));
    assert!(
        in_pile(st, HERO1, a2, Zone::Hand),
        "the declined second card stays"
    );
    assert!(
        in_pile(st, HERO2, b1, Zone::Graveyard),
        "the last loaded hero auto-covers the remainder"
    );
}

#[test]
fn baxter_banks_then_fires_on_the_next_wave_card() {
    let mut state = hero_event_game();
    let bear = add_hero_creature(&mut state, HERO1, "Hero Bear", 2);
    let baxter = add_library_event(&mut state, "Event: Baxter's Experiments");
    add_library_event(&mut state, "Event: Bebop's Rampage");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 2);
    // No prompts: Baxter banks silently and Rampage is synchronous.
    let mut answers = vec![];
    drive_wave_with_answers(&mut runner, &mut answers, |runner| {
        panic!("no prompt expected, got {:?}", runner.state().waiting_for)
    });
    assert_wave_settled(&runner);
    let st = runner.state();
    assert_eq!(
        st.objects.get(&bear).expect("bear").damage_marked,
        4,
        "Rampage resolved twice (2 + 2) under the fired Baxter"
    );
    assert!(
        in_pile(st, HORDE, baxter, Zone::Graveyard),
        "the fired Baxter is discarded after the resolution"
    );
    assert!(
        st.baxter_set_aside.is_empty(),
        "the bank drained when it fired"
    );
}

#[test]
fn rat_king_stuns_once_per_deployed_boss() {
    let mut state = hero_event_game();
    let bosses = deployed_bosses(&state).len();
    assert!(bosses >= 1, "stage 1 deploys at load");
    let bear = add_hero_creature(&mut state, HERO1, "Hero Bear", 2);
    add_hero_creature(&mut state, HERO1, "Hero Griffin", 2);
    add_library_event(&mut state, "Event: Rat King's Revolution");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    let mut answers = vec![vec![bear]];
    drive_wave_with_answers(&mut runner, &mut answers, |runner| {
        let WaitingFor::HeroEventChoice { player, count, .. } = &runner.state().waiting_for else {
            panic!("Rat King must prompt, got {:?}", runner.state().waiting_for);
        };
        assert_eq!((*player, *count), (HERO1, 1));
    });
    assert_wave_settled(&runner);
    let obj = runner.state().objects.get(&bear).expect("bear");
    assert!(obj.tapped, "the pick taps");
    assert_eq!(
        obj.counters.get(&CounterType::Stun).copied().unwrap_or(0),
        bosses as u32,
        "one stun counter per deployed boss"
    );
}

#[test]
fn mayhem_team_spare_destroys_the_rest() {
    let mut state = hero_event_game();
    let bear = add_hero_creature(&mut state, HERO1, "Hero Bear", 2);
    let griffin = add_hero_creature(&mut state, HERO1, "Hero Griffin", 2);
    let turtle = add_hero_creature(&mut state, HERO1, "Hero Turtle", 2);
    let h2a = add_hero_creature(&mut state, HERO2, "Second Hero A", 2);
    let h2b = add_hero_creature(&mut state, HERO2, "Second Hero B", 2);
    add_library_event(&mut state, "Event: Villain Mayhem!");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    let mut answers = vec![vec![bear, griffin, h2a]];
    drive_wave_with_answers(&mut runner, &mut answers, |runner| {
        let WaitingFor::HeroEventChoice {
            player,
            count,
            cards,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Mayhem must prompt, got {:?}", runner.state().waiting_for);
        };
        assert_eq!(*player, HERO1, "the first hero calls for the team");
        assert_eq!(*count, 3);
        assert_eq!(cards.len(), 5, "the pool spans both heroes");
    });
    assert_wave_settled(&runner);
    let st = runner.state();
    for survivor in [bear, griffin, h2a] {
        assert!(
            st.battlefield.contains(&survivor),
            "the spared three survive"
        );
    }
    assert!(
        in_pile(st, HERO1, turtle, Zone::Graveyard),
        "every other creature is destroyed"
    );
    assert!(in_pile(st, HERO2, h2b, Zone::Graveyard));
}

#[test]
fn smackdown_destroys_strongest_and_drains_power() {
    let mut state = hero_event_game();
    add_hero_creature(&mut state, HERO1, "Hero Bear", 2);
    let griffin = add_hero_creature(&mut state, HERO1, "Hero Griffin", 5);
    add_hero_creature(&mut state, HERO1, "Hero Turtle", 5);
    add_library_event(&mut state, "Event: Leatherhead's Smackdown");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    // Snapshot after the advance: a deployed Krang's turn-start trigger
    // ("the bosses lose 2 life") resolves on the way through upkeep.
    let boss_life = seat_life(runner.state(), HORDE);
    let mut answers = vec![vec![griffin]];
    drive_wave_with_answers(&mut runner, &mut answers, |runner| {
        let WaitingFor::HeroEventChoice { count, cards, .. } = &runner.state().waiting_for else {
            panic!(
                "Smackdown must prompt, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*count, 1);
        assert_eq!(cards.len(), 2, "only the tied 5s are offered");
    });
    assert_wave_settled(&runner);
    let st = runner.state();
    assert!(
        in_pile(st, HERO1, griffin, Zone::Graveyard),
        "the pick is destroyed"
    );
    assert_eq!(
        seat_life(st, HORDE),
        boss_life - 5,
        "the Bosses lose life equal to the destroyed power"
    );
}

#[test]
fn savanti_curse_bounces_picks_to_hand() {
    let mut state = hero_event_game();
    let bosses = deployed_bosses(&state).len();
    assert!(bosses >= 1, "stage 1 deploys at load");
    // Force the prompt: more creatures than bosses (battlefield order is
    // creation order, so the first `bosses` picks are deterministic).
    let mut made: Vec<ObjectId> = vec![
        add_hero_creature(&mut state, HERO1, "Hero Bear", 2),
        add_hero_creature(&mut state, HERO1, "Hero Griffin", 2),
        add_hero_creature(&mut state, HERO1, "Hero Turtle", 2),
    ];
    let mut extra = 0;
    while made.len() <= bosses {
        made.push(add_hero_creature(
            &mut state,
            HERO1,
            &format!("Extra {extra}"),
            2,
        ));
        extra += 1;
    }
    let picked: Vec<ObjectId> = made.into_iter().take(bosses).collect();
    add_library_event(&mut state, "Event: Savanti Romero's Curse");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    let mut answers = vec![picked.clone()];
    drive_wave_with_answers(&mut runner, &mut answers, |runner| {
        let WaitingFor::HeroEventChoice { count, .. } = &runner.state().waiting_for else {
            panic!("Savanti must prompt, got {:?}", runner.state().waiting_for);
        };
        assert_eq!(*count, bosses, "X is the number of bosses");
    });
    assert_wave_settled(&runner);
    let st = runner.state();
    for id in &picked {
        assert!(
            in_pile(st, HERO1, *id, Zone::Hand),
            "each pick returns to its owner's hand"
        );
    }
    assert!(
        !st.battlefield.is_empty(),
        "unpicked creatures stay on the battlefield"
    );
}

#[test]
fn infiltration_destroys_strongest_without_draining() {
    let mut state = hero_event_game();
    add_hero_creature(&mut state, HERO1, "Hero Bear", 2);
    add_hero_creature(&mut state, HERO1, "Hero Griffin", 5);
    let turtle = add_hero_creature(&mut state, HERO1, "Hero Turtle", 5);
    add_library_event(&mut state, "Event: Villain Infiltration");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    // Snapshot after the advance (see the Smackdown test: Krang's turn-start
    // trigger resolves on the way through upkeep).
    let boss_life = seat_life(runner.state(), HORDE);
    let mut answers = vec![vec![turtle]];
    drive_wave_with_answers(&mut runner, &mut answers, |runner| {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::HeroEventChoice { .. }
            ),
            "Infiltration must prompt, got {:?}",
            runner.state().waiting_for
        );
    });
    assert_wave_settled(&runner);
    let st = runner.state();
    assert!(
        in_pile(st, HERO1, turtle, Zone::Graveyard),
        "the pick is destroyed"
    );
    assert_eq!(
        seat_life(st, HORDE),
        boss_life,
        "Infiltration drains no life, unlike Smackdown"
    );
}

/// Give `id` a regeneration shield (the `regenerate.rs` recipe, pushed live +
/// base like `install_esix` — integration tests cannot reach the
/// `pub(crate)` resolution installer) plus one shield counter. Two distinct
/// automatic destroy preventions, so the destroy raises a CR 616.1 ordering
/// prompt; either answer prevents the destruction.
fn add_regen_shield_and_counter(state: &mut GameState, id: ObjectId) {
    use std::sync::Arc;
    let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
        .valid_card(TargetFilter::SelfRef)
        .description("Regenerate".to_string())
        .regeneration_shield();
    let obj = state.objects.get_mut(&id).unwrap();
    obj.replacement_definitions = vec![shield.clone()].into();
    obj.base_replacement_definitions = Arc::new(vec![shield]);
    obj.counters.insert(CounterType::Shield, 1);
}

/// R2-#6: a prevented Mayhem destroy resumes the remaining steps. The first
/// destroy pauses on the shield-ordering prompt; answering prevents that one
/// destruction (CR 608.2c modifies only that action), and the three destroys
/// still owed run instead of staying parked.
///
/// Revert guard: without the Prevented-arm hero drain, the remaining steps
/// never resume — the wave never settles (budget unspent, `pending_hero_event`
/// still parked) and the assertions flip.
#[test]
fn mayhem_prevented_destroy_resumes_remaining_steps() {
    let mut state = hero_event_game();
    // Created first: first in HERO1's battlefield order, so its destroy is
    // the first step and parks the other three behind the prompt.
    let shielded = add_hero_creature(&mut state, HERO1, "Shielded Bear", 2);
    add_regen_shield_and_counter(&mut state, shielded);
    let spared_a = add_hero_creature(&mut state, HERO1, "Spared A", 2);
    let spared_b = add_hero_creature(&mut state, HERO1, "Spared B", 2);
    let spared_c = add_hero_creature(&mut state, HERO2, "Spared C", 2);
    let doomed_d = add_hero_creature(&mut state, HERO2, "Doomed D", 2);
    let doomed_e = add_hero_creature(&mut state, HERO2, "Doomed E", 2);
    let doomed_f = add_hero_creature(&mut state, HERO2, "Doomed F", 2);
    add_library_event(&mut state, "Event: Villain Mayhem!");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);

    // The team spares three; the shielded bear is NOT among them.
    let mut answers = vec![vec![spared_a, spared_b, spared_c]];
    let mut replacement_prompts = 0;
    for _ in 0..80 {
        let done = {
            let st = runner.state();
            st.horde_wave_remaining == 0
                && st.stack.is_empty()
                && matches!(st.waiting_for, WaitingFor::Priority { .. })
        };
        if done {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::HeroEventChoice { player, count, .. } => {
                assert_eq!((*player, *count), (HERO1, 3));
                let answer = answers.remove(0);
                runner
                    .act(GameAction::SelectCards { cards: answer })
                    .expect("the spare set must be legal");
            }
            WaitingFor::ReplacementChoice {
                player,
                kind,
                candidates,
                ..
            } => {
                replacement_prompts += 1;
                assert_eq!(
                    *player, HERO1,
                    "the shielded bear's controller orders the preventions"
                );
                assert_eq!(
                    *kind,
                    ReplacementChoiceKind::Order,
                    "two automatic preventions raise an ordering prompt, got {candidates:?}"
                );
                assert_eq!(
                    candidates.len(),
                    2,
                    "the shield and the counter are the two candidates, got {candidates:?}"
                );
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the ordering pick must be accepted");
            }
            other => panic!("unexpected prompt during the wave: {other:?}"),
        }
    }
    assert!(answers.is_empty(), "the spare choice must fire");
    assert_eq!(
        replacement_prompts, 1,
        "exactly one ordering prompt (the shielded destroy) must fire"
    );
    assert_wave_settled(&runner);
    assert!(
        runner.state().pending_hero_event.is_none(),
        "the parked hero event must drain on resume"
    );

    let st = runner.state();
    assert!(
        st.battlefield.contains(&shielded),
        "the prevented destroy leaves the shielded bear alive"
    );
    for survivor in [spared_a, spared_b, spared_c] {
        assert!(
            st.battlefield.contains(&survivor),
            "the spared three survive"
        );
    }
    for doomed in [doomed_d, doomed_e, doomed_f] {
        assert!(
            in_pile(st, HERO2, doomed, Zone::Graveyard),
            "the remaining destroys still run after the prevented one"
        );
    }
}

/// Blood Artist with its real trigger, parsed from verbatim Oracle text
/// through the production parser — a real printed card:
/// "Whenever this creature or another creature dies, target player loses 1
/// life and you gain 1 life."
/// The parsed definition is cloned onto the synthetic horde object so the rig
/// stays DB-free; the definition is parser output, never hand-built. The
/// reach-guard assert proves the parser path ran (a vacuous trigger set would
/// pass every downstream assertion for the wrong reason).
fn add_blood_artist(state: &mut GameState, owner: PlayerId) -> ObjectId {
    let artist = add_hero_creature(state, owner, "Blood Artist", 1);
    let mut oracle = GameScenario::new();
    let parsed = oracle
        .add_creature_from_oracle(
            owner,
            "Blood Artist",
            1,
            1,
            "Whenever this creature or another creature dies, target player loses 1 life and you gain 1 life.",
        )
        .id();
    let face = oracle.build();
    let source = &face.state().objects[&parsed];
    assert!(
        !source.trigger_definitions.is_empty(),
        "Blood Artist must parse to a dies trigger"
    );
    let obj = state.objects.get_mut(&artist).expect("artist");
    obj.trigger_definitions = source.trigger_definitions.clone();
    obj.base_trigger_definitions = source.base_trigger_definitions.clone();
    obj.power = Some(0);
    obj.base_power = Some(0);
    obj.toughness = Some(1);
    obj.base_toughness = Some(1);
    engine::game::trigger_index::reindex_object_triggers(state, artist);
    artist
}

/// A one-shot Optional `Moved` redirect for `card`'s graveyard move
/// (accept = 0 exiles instead, decline = 1 lets the death stand), mirroring
/// the boss unit-test rig. Declining parks the hero batch mid-list so the
/// resume drains the remainder — the cohort then needs the ledger stamp for
/// cross-pause look-back.
fn add_decline_rigging(state: &mut GameState, owner: PlayerId, card: ObjectId) {
    use std::sync::Arc;
    let source = create_object(
        state,
        CardId(state.next_object_id),
        owner,
        "Redirector".to_string(),
        Zone::Battlefield,
    );
    let def = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .destination_zone(Zone::Graveyard)
        .valid_card(TargetFilter::SpecificObject { id: card })
        .mode(ReplacementMode::Optional { decline: None });
    let obj = state.objects.get_mut(&source).expect("source");
    obj.replacement_definitions = vec![def.clone()].into();
    obj.base_replacement_definitions = Arc::new(vec![def]);
}

/// Drive a wave whose Event parks once on the decline rigging and whose Blood
/// Artist triggers ask for targets. Returns (replacement_prompts,
/// target_prompts). Any other prompt — or a planned hero answer that never
/// fires, or a trigger that never resolves — panics or fails the counts
/// instead of passing silently.
fn drive_wave_with_artist(runner: &mut GameRunner, answers: &mut Vec<Vec<ObjectId>>) -> (u32, u32) {
    use engine::game::triggers::drain_order_triggers_with_identity;
    let mut replacement_prompts = 0;
    let mut target_prompts = 0;
    for _ in 0..120 {
        let done = {
            let st = runner.state();
            st.horde_wave_remaining == 0
                && st.stack.is_empty()
                && matches!(st.waiting_for, WaitingFor::Priority { .. })
        };
        if done {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::HeroEventChoice { .. } => {
                assert!(
                    !answers.is_empty(),
                    "unexpected HeroEventChoice prompt: {:?}",
                    runner.state().waiting_for
                );
                let answer = answers.remove(0);
                runner
                    .act(GameAction::SelectCards { cards: answer })
                    .expect("the planned answer must be legal");
            }
            WaitingFor::ReplacementChoice {
                player,
                kind,
                candidates,
                ..
            } => {
                replacement_prompts += 1;
                assert!(
                    matches!(*player, HERO1 | HERO2),
                    "the rigged replacement belongs to a hero, got {player:?}"
                );
                assert_eq!(
                    *kind,
                    ReplacementChoiceKind::OptionalBranch,
                    "the single Optional rigging is an accept/decline branch, got {candidates:?}"
                );
                assert_eq!(
                    candidates.len(),
                    2,
                    "accept + decline are the two branches, got {candidates:?}"
                );
                runner
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .expect("decline must be accepted");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                target_prompts += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(HORDE)),
                    })
                    .expect("the Horde must be a legal drain target");
            }
            other => panic!("unexpected prompt during the wave: {other:?}"),
        }
    }
    assert!(answers.is_empty(), "every planned hero answer must fire");
    (replacement_prompts, target_prompts)
}

/// CR 101.4 + CR 603.10a: Chrome Dome's Overload sacrificing a tapped Blood
/// Artist for the first hero, then pausing on the decline rigging for the
/// second hero's sacrifice, still triggers the Artist twice — the pause must
/// not split the instruction's departure batch.
///
/// Revert guard: without the carried cohort stamp, the second death's
/// look-back finds no co-departed Artist, so only one target prompt fires and
/// the Horde loses 1 instead of 2.
#[test]
fn chrome_dome_overload_sacrifice_pause_keeps_artist_cohort() {
    let mut state = hero_event_game();
    // One tapped creature per hero: both offers auto-resolve, so the steps
    // are fixed and the only prompt mid-batch is the rigged replacement.
    let artist = add_blood_artist(&mut state, HERO1);
    state.objects.get_mut(&artist).expect("artist").tapped = true;
    let doomed = add_hero_creature(&mut state, HERO2, "Doomed Ox", 2);
    state.objects.get_mut(&doomed).expect("doomed").tapped = true;
    add_decline_rigging(&mut state, HERO2, doomed);
    add_library_event(&mut state, "Event: Chrome Dome's Overload");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    // Baselines sit after the wave starts: the phase advance wraps through
    // the Boss turn, whose opening trigger costs the Horde 2 life before the
    // Event resolves. Measuring here isolates the Artist's own drains.
    let horde_life_before = seat_life(runner.state(), HORDE);
    let hero_life_before = seat_life(runner.state(), HERO1);

    let mut answers = Vec::new();
    let (replacements, targets) = drive_wave_with_artist(&mut runner, &mut answers);
    assert_eq!(
        replacements, 1,
        "the doomed sacrifice parks once on the decline rigging"
    );
    assert_eq!(
        targets, 2,
        "the Artist triggers for its own death and the doomed death"
    );
    assert_wave_settled(&runner);
    assert!(
        runner.state().pending_hero_event.is_none(),
        "the parked event drains on resume"
    );
    let st = runner.state();
    assert!(
        in_pile(st, HERO1, artist, Zone::Graveyard),
        "the Artist is sacrificed"
    );
    assert!(
        in_pile(st, HERO2, doomed, Zone::Graveyard),
        "the declined redirect still sacrifices"
    );
    assert_eq!(
        seat_life(st, HORDE),
        horde_life_before - 2,
        "two Artist triggers drain the Horde"
    );
    assert_eq!(
        seat_life(st, HERO1),
        hero_life_before + 2,
        "the Artist's controller gains twice"
    );
}

/// CR 101.4 + CR 603.10a, re-pause: Chrome Dome's Overload pausing TWICE (a
/// decline rigging on each hero's sacrifice) still triggers the Artist
/// twice — the second park must not lose the first parking step's ledger
/// record while its id stays in the cohort.
///
/// Revert guard: without the carried-ids re-scan at park time, the Artist's
/// own-death record (emitted in the middle segment, after `steps` dropped it)
/// never reaches the terminal stamp, so the look-back misses one
/// co-departure and only one target prompt fires.
#[test]
fn chrome_dome_overload_double_pause_keeps_artist_cohort() {
    let mut state = hero_event_game();
    let artist = add_blood_artist(&mut state, HERO1);
    state.objects.get_mut(&artist).expect("artist").tapped = true;
    let doomed = add_hero_creature(&mut state, HERO2, "Doomed Ox", 2);
    state.objects.get_mut(&doomed).expect("doomed").tapped = true;
    add_decline_rigging(&mut state, HERO1, artist);
    add_decline_rigging(&mut state, HERO2, doomed);
    add_library_event(&mut state, "Event: Chrome Dome's Overload");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    // Baselines sit after the wave starts; see the Overload test above.
    let horde_life_before = seat_life(runner.state(), HORDE);
    let hero_life_before = seat_life(runner.state(), HERO1);

    let mut answers = Vec::new();
    let (replacements, targets) = drive_wave_with_artist(&mut runner, &mut answers);
    assert_eq!(
        replacements, 2,
        "each rigged sacrifice parks once on its decline rigging"
    );
    assert_eq!(
        targets, 2,
        "the Artist triggers for its own death and the doomed death"
    );
    assert_wave_settled(&runner);
    assert!(
        runner.state().pending_hero_event.is_none(),
        "both parked segments drain on resume"
    );
    let st = runner.state();
    assert!(
        in_pile(st, HERO1, artist, Zone::Graveyard),
        "the Artist is sacrificed"
    );
    assert!(
        in_pile(st, HERO2, doomed, Zone::Graveyard),
        "the declined redirect still sacrifices"
    );
    assert_eq!(
        seat_life(st, HORDE),
        horde_life_before - 2,
        "two Artist triggers drain the Horde"
    );
    assert_eq!(
        seat_life(st, HERO1),
        hero_life_before + 2,
        "the Artist's controller gains twice"
    );
}

/// Grumgully on the Horde battlefield: parsed replacement cloned onto the
/// synthetic object (the Artist recipe), so the ETB counter replacement is
/// parser output, never hand-built. The reach-guard assert proves the parser
/// path ran.
fn add_grumgully(state: &mut GameState, owner: PlayerId) -> ObjectId {
    let grumgully = add_hero_creature(state, owner, "Grumgully, the Generous", 3);
    let obj = state.objects.get_mut(&grumgully).expect("grumgully");
    obj.toughness = Some(3);
    obj.base_toughness = Some(3);
    let mut oracle = GameScenario::new();
    let parsed = oracle
        .add_creature_from_oracle(
            owner,
            "Grumgully, the Generous",
            3,
            3,
            "Each other non-Human creature you control enters with an additional +1/+1 counter on it.",
        )
        .id();
    let face = oracle.build();
    let source = &face.state().objects[&parsed];
    assert!(
        !source.replacement_definitions.is_empty(),
        "Grumgully must parse to an ETB-counter replacement"
    );
    let obj = state.objects.get_mut(&grumgully).expect("grumgully");
    obj.replacement_definitions = source.replacement_definitions.clone();
    obj.base_replacement_definitions = source.base_replacement_definitions.clone();
    grumgully
}

/// A counter-modifying enchantment on the battlefield: parsed replacement
/// cloned onto the synthetic object (the Artist recipe for noncreatures).
/// The reach-guard assert proves the parser path ran.
fn add_counter_enchantment(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    oracle_text: &str,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.controller = owner;
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.base_card_types = obj.card_types.clone();
    let mut oracle = GameScenario::new();
    let parsed = oracle
        .add_enchantment_from_oracle(owner, name, oracle_text)
        .id();
    let face = oracle.build();
    let source = &face.state().objects[&parsed];
    assert!(
        !source.replacement_definitions.is_empty(),
        "{name} must parse to a counter replacement"
    );
    let obj = state.objects.get_mut(&id).expect("enchantment");
    obj.replacement_definitions = source.replacement_definitions.clone();
    obj.base_replacement_definitions = source.base_replacement_definitions.clone();
    id
}

/// A creature token on the Horde library (the `horde_tokens` recipe).
/// Creation order is top-first, like the event helper above.
fn add_library_token(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    p: i32,
    t: i32,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Library);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.is_token = true;
    obj.is_horde_deck_token = true;
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(p);
    obj.toughness = Some(t);
    obj.base_power = Some(p);
    obj.base_toughness = Some(t);
    id
}

/// Drive a token-counter wave: the only prompt can be the ETB counter
/// ordering prompt. Applies Hardened Scales first when both doublers are
/// offered; a lone remainder applies trivially. Returns the ordering-prompt
/// count. Any other prompt panics instead of passing silently.
///
/// `second` is the library card beneath the first reveal: at the FIRST
/// prompt the test asserts the wave parked (payload set, budget spent on the
/// parked reveal, second card untouched) — the wave cannot advance past an
/// unanswered prompt.
fn drive_token_counter_wave(runner: &mut GameRunner, second: ObjectId) -> u32 {
    let mut order_prompts = 0;
    for _ in 0..200 {
        let done = {
            let st = runner.state();
            st.horde_wave_remaining == 0
                && st.stack.is_empty()
                && matches!(st.waiting_for, WaitingFor::Priority { .. })
        };
        if done {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::ReplacementChoice {
                player,
                kind,
                candidates,
                ..
            } => {
                if order_prompts == 0 {
                    assert!(
                        runner.state().pending_horde_reveal.is_some(),
                        "the first reveal parks while its ordering prompt is pending"
                    );
                    assert!(
                        in_pile(runner.state(), HORDE, second, Zone::Library),
                        "the second card waits in the library while the first prompt is pending"
                    );
                }
                order_prompts += 1;
                assert_eq!(
                    *player, HORDE,
                    "the Horde orders its own counter replacements"
                );
                assert_eq!(
                    *kind,
                    ReplacementChoiceKind::Order,
                    "Scales vs Evolution is an ordering prompt, got {candidates:?}"
                );
                assert_eq!(
                    candidates.len(),
                    2,
                    "Grumgully applies alone first; only the two doublers order, got {candidates:?}"
                );
                let index = candidates
                    .iter()
                    .position(|c| c.source_name == "Hardened Scales")
                    .expect("Hardened Scales is one of the two candidates");
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("the ordering pick must be accepted");
            }
            WaitingFor::DeclareAttackers {
                valid_attacker_ids, ..
            } => {
                // The wave may ride into combat on an all-pass while a parked
                // token reveal waits for its seam beat (the seam only fires
                // when priority is granted to the Horde). Declare everything
                // and keep driving — the phase-agnostic seam drains the parked
                // reveal on a later beat.
                let ids = valid_attacker_ids.clone();
                let attackers: Vec<(ObjectId, AttackTarget)> = ids
                    .iter()
                    .map(|id| (*id, AttackTarget::Player(HERO1)))
                    .collect();
                runner
                    .declare_attackers(&attackers)
                    .expect("the Horde's attackers must be declarable");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("the heroes have no blockers");
            }
            other => panic!("unexpected prompt during the token wave: {other:?}"),
        }
    }
    order_prompts
}

/// CR 614.16 + CR 616.1: revealed tokens whose ETB counter replacements
/// order-prompt (Grumgully alone first, then Scales vs Evolution) pause the
/// wave instead of dropping the reveal tail — each token still enters, with
/// all three replacements applied in the answered order, and the wave
/// continues to the next card only after the prompt is answered.
///
/// Revert guard: without the delivery-pause park, the delivery raises the
/// ordering prompt and the wave tail clobbers it — no ordering prompt is
/// ever observed (0 vs 2) and no token enters.
#[test]
fn revealed_token_counter_ordering_pause_resumes_wave() {
    let mut state = hero_event_game();
    add_grumgully(&mut state, HORDE);
    add_counter_enchantment(
        &mut state,
        HORDE,
        "Hardened Scales",
        "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.",
    );
    add_counter_enchantment(
        &mut state,
        HORDE,
        "Branching Evolution",
        "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.",
    );
    let before_battlefield: Vec<ObjectId> = state.battlefield.iter().copied().collect();
    // Creation order is top-first: the Zombie reveals first. (The turn-entry
    // wave may reveal it during the advance below; the budget overwrite only
    // resets the counter — a parked payload survives it.)
    add_library_token(&mut state, HORDE, "Zombie Token", 2, 2);
    let second = add_library_token(&mut state, HORDE, "Skeleton Token", 1, 1);
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 2);

    let order_prompts = drive_token_counter_wave(&mut runner, second);
    assert_eq!(
        order_prompts, 2,
        "Scales vs Evolution orders once per revealed token"
    );
    assert_wave_settled(&runner);
    assert!(
        runner.state().pending_horde_reveal.is_none(),
        "each parked token reveal drains on resume"
    );
    let st = runner.state();
    let entered: Vec<ObjectId> = st
        .battlefield
        .iter()
        .copied()
        .filter(|id| !before_battlefield.contains(id))
        .collect();
    assert_eq!(
        entered.len(),
        2,
        "both revealed tokens enter, got {entered:?}"
    );
    for token in entered {
        assert_eq!(
            st.objects
                .get(&token)
                .and_then(|o| o.counters.get(&CounterType::Plus1Plus1))
                .copied(),
            Some(4),
            "Grumgully 1, then Scales +1, then Evolution x2 in the answered order"
        );
    }
}

/// CR 101.4 + CR 603.10a: Villain Mayhem! destroying the Artist before the
/// pause, then pausing on the decline rigging for a later destroy, still
/// triggers the Artist for deaths on both sides of the pause.
///
/// Revert guard: without the carried cohort stamp, the post-pause death's
/// look-back misses the Artist, so only one target prompt fires.
#[test]
fn villain_mayhem_destroy_pause_keeps_artist_cohort() {
    let mut state = hero_event_game();
    let artist = add_blood_artist(&mut state, HERO1);
    let spared_a = add_hero_creature(&mut state, HERO1, "Spared A", 2);
    let spared_b = add_hero_creature(&mut state, HERO1, "Spared B", 2);
    let spared_c = add_hero_creature(&mut state, HERO2, "Spared C", 2);
    let doomed = add_hero_creature(&mut state, HERO2, "Doomed Ox", 2);
    add_decline_rigging(&mut state, HERO2, doomed);
    add_library_event(&mut state, "Event: Villain Mayhem!");
    let mut runner = GameRunner::from_state(state);
    begin_wave_with_budget(&mut runner, 1);
    // Baselines sit after the wave starts; see the Overload test above.
    let horde_life_before = seat_life(runner.state(), HORDE);
    let hero_life_before = seat_life(runner.state(), HERO1);

    // The team spares three others; the Artist and the rigged Ox die.
    let mut answers = vec![vec![spared_a, spared_b, spared_c]];
    let (replacements, targets) = drive_wave_with_artist(&mut runner, &mut answers);
    assert_eq!(
        replacements, 1,
        "the rigged destroy parks once on the decline rigging"
    );
    assert_eq!(
        targets, 2,
        "the Artist triggers for its own death and the Ox death"
    );
    assert_wave_settled(&runner);
    assert!(
        runner.state().pending_hero_event.is_none(),
        "the parked event drains on resume"
    );
    let st = runner.state();
    assert!(
        in_pile(st, HERO1, artist, Zone::Graveyard),
        "the Artist is destroyed"
    );
    assert!(
        in_pile(st, HERO2, doomed, Zone::Graveyard),
        "the declined redirect still destroys"
    );
    for survivor in [spared_a, spared_b, spared_c] {
        assert!(
            st.battlefield.contains(&survivor),
            "the spared three survive"
        );
    }
    assert_eq!(
        seat_life(st, HORDE),
        horde_life_before - 2,
        "two Artist triggers drain the Horde"
    );
    assert_eq!(
        seat_life(st, HERO1),
        hero_life_before + 2,
        "the Artist's controller gains twice"
    );
}
