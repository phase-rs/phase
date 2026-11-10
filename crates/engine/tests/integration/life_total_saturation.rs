//! A life total stops at the `i32` limit a gain or loss moves it toward. That
//! is an engine storage limit (`arithmetic::saturating_life_total`), not a
//! rule, as is the declared starting-life ceiling `MAX_STARTING_LIFE`, which
//! `types/format.rs` calls "an engine invariant, not a Comprehensive Rules
//! limit". A life total may be negative (CR 107.1b). The `LifeChanged` event
//! and the per-turn gained/lost counters report the rules amount (CR 119.3),
//! even when the stored total moved less; the event's `i32` field clamps a
//! gain past `i32::MAX` to `i32::MAX` and a loss past it to `i32::MIN`. Energy
//! and player counters keep refusing a total past `u32::MAX`. A life amount
//! past `i32::MAX` moves the total by the whole amount, up to the `i32` limit,
//! but an effect amount is an `i32` quantity (`quantity::resolve_quantity`), so
//! a "that much" or "that many" reader of it (such as
//! `targeting::extract_amount_from_event`,
//! `effects::previous_effect_amount_from_events` or a substitute's
//! `last_effect_count` stamp) sees at most `i32::MAX`. Exquisite Blood
//! ("Whenever an opponent loses life, you gain that much life.") reads a loss
//! past it as `i32::MAX`, and so does Tainted Remedy ("If an opponent would
//! gain life, that player loses that much life instead.") for a gain past it,
//! which `engine_debug.rs`'s
//! `a_loss_past_i32_reports_i32_min_and_that_much_reads_its_magnitude` and
//! `a_gain_past_i32_substituted_by_tainted_remedy_reads_the_saturated_stamp`
//! pin. Set/exchange/redistribute land exactly across the whole range, and a
//! change to a shared life total, a Two-Headed Giant team's (CR 810.4) or the
//! Archenemy Commander heroes' (CR 904.13b), caps that total (CR 810.9).
//! A life replacement quantity past
//! `u32` lands on `u32::MAX`. A mandatory life-gain loop at the cap is drawn
//! (CR 104.4b + CR 732.4), as are a mandatory loop that drains a seat held at
//! `i32::MIN` and one whose life changes leave a team at the cap where it stood;
//! an optional one whose only growth is the life the cap holds
//! (Heliod, Sun-Crowned with Spike Feeder) is never drawn or offered a
//! shortcut, and a loop of mandatory triggers that another player can change
//! only with an out (Lightning Bolt) is offered a draw without end, at the cap
//! as below it (CR 732.5). A shortcut's batched life lands at the take, and a
//! shortcut whose cycles' life a lump would not land as performing lands it
//! (a change an `i32` limit holds back, a loss, a change stored on a teammate,
//! or life beside batched tokens or counters) is taken by performing its
//! cycles, so its counters and total end as performing them leaves them, even
//! after a later life change (CR 732.2c), and its record keeps the first and
//! last cycle (CR 732.1b).

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::{CountedTake, OfferRoad, ShortcutResponse};
use engine::analysis::resource::{CounterClass, ObjectClass, ResourceAxis};
use engine::game::derived_views::derive_views;
use engine::game::players::team_life_sum;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    Effect, EffectKind, QuantityModification, ReplacementDefinition, TargetRef,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopDetectionMode, PayableResource, PersistentAxisMaterialization,
    WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::replacements::ReplacementEvent;
use engine::types::resolved_commands::{
    ResolvedPlayerEdit, ResolvedPlayerEditReplayInvariantError, ResolvedRulesCommand,
};
use engine::types::zones::Zone;

use crate::loop_period_remade_token_source::{markers, repetitions};

/// Luminescent Rain's verbatim Oracle text (not a member of the test fixture).
const LUMINESCENT_RAIN: &str =
    "Choose a creature type. You gain 2 life for each permanent you control of that type.";

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(9_999), false, Vec::new())
}

/// Every `LifeChanged` as `(player, amount, new total)`, in order.
pub(crate) fn life_changes(events: &[GameEvent]) -> Vec<(PlayerId, i32, Option<i32>)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::LifeChanged {
                player_id,
                amount,
                new_total,
            } => Some((*player_id, *amount, new_total.0)),
            _ => None,
        })
        .collect()
}

fn is_priority(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. })
}

/// The second team's first seat in a four-player Two-Headed Giant game.
const P2: PlayerId = PlayerId(2);

/// Reverse the Sands' verbatim Oracle text (not a member of the test fixture).
const REVERSE_THE_SANDS: &str = "Redistribute any number of players' life totals. (Each of those \
                                 players gets one life total back.)";

/// Beacon of Immortality's verbatim Oracle text (not a member of the test
/// fixture).
const BEACON_OF_IMMORTALITY: &str = "Double target player's life total. Shuffle Beacon of \
                                     Immortality into its owner's library.";

fn mana_pool(color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count).map(|_| mana(color)).collect()
}

/// A four-player Two-Headed Giant game (P0 and P1 share a life total, as do
/// P2 and P3) in P0's precombat main phase.
fn two_headed_giant() -> GameScenario {
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// Passes priority while the stack holds something and priority is open,
/// returning every event; stops at the first prompt.
fn pass_until_prompt(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..8 {
        if runner.state().stack.is_empty() || !is_priority(runner.state()) {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority is accepted")
                .events,
        );
    }
    events
}

/// The `(P0, P1)` stored life fields.
fn team_a_fields(state: &GameState) -> (i32, i32) {
    (state.players[0].life, state.players[1].life)
}

/// The game is over with `winner` winning.
fn won_by(state: &GameState, winner: PlayerId) -> bool {
    matches!(state.waiting_for, WaitingFor::GameOver { winner: Some(w) } if w == winner)
}

/// The issue's board: Ouroboroid ("At the beginning of combat on your turn,
/// put X +1/+1 counters on each creature you control, where X is this
/// creature's power.") doubles its own power each combat, which is how a long
/// game reaches the power cap. Dazzling Reflection ("You gain life equal to
/// target creature's power. The next time that creature would deal damage this
/// turn, prevent that damage.") on it then gains `i32::MAX` life: the total
/// lands on `i32::MAX`, the gain reports the rules amount, and the rest of the
/// spell resolves. The control at power 100 is unchanged.
#[test]
fn ouroboroid_at_the_power_cap_resolves_dazzling_reflection_at_the_life_cap() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (+1/+1 counters, power, life after, gained after)
    let rows: [(u32, i32, i32, u32); 2] = [
        (u32::MAX, i32::MAX, i32::MAX, i32::MAX.unsigned_abs()),
        (99, 100, 120, 100),
    ];
    for (counters, power, life_after, gained_after) in rows {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ouroboroid = scenario.add_real_card(P0, "Ouroboroid", Zone::Battlefield, db);
        let spell = scenario.add_real_card(P0, "Dazzling Reflection", Zone::Hand, db);
        scenario.with_mana_pool(P0, vec![mana(ManaType::White), mana(ManaType::White)]);
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&ouroboroid)
            .expect("Ouroboroid is on the battlefield")
            .counters
            .insert(CounterType::Plus1Plus1, counters);
        engine::game::layers::evaluate_layers(runner.state_mut());
        assert_eq!(
            runner.state().objects[&ouroboroid].power,
            Some(power),
            "reach guard: Ouroboroid's power with {counters} counters"
        );

        // The fixture parse gives the spell a second target slot for the
        // prevention's source, so the creature is named for both.
        let outcome = runner
            .cast(spell)
            .target_object(ouroboroid)
            .target_object(ouroboroid)
            .try_resolve()
            .expect("Dazzling Reflection resolves");
        let events = outcome.events().to_vec();

        assert_eq!(runner.life(P0), life_after, "power {power}");
        assert_eq!(
            runner.state().players[0].life_gained_this_turn,
            gained_after,
            "power {power}: the gain counts the rules amount"
        );
        assert_eq!(
            life_changes(&events),
            vec![(P0, power, Some(life_after))],
            "power {power}: one LifeChanged reports the rules amount"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::PreventDamage,
                    ..
                }
            )),
            "power {power}: the prevention clause resolves"
        );
        assert_eq!(
            runner.state().objects[&spell].zone,
            Zone::Graveyard,
            "power {power}: the spell finishes resolving"
        );
        assert!(runner.state().stack.is_empty(), "power {power}");
        assert!(is_priority(runner.state()), "power {power}");
    }
}

/// Luminescent Rain one below the cap: the named type is applied after the
/// choice is answered, the gain of 2 lands on `i32::MAX` and counts 2. Naming
/// a type P0 does not control gains 0 life, which is no life-gain event
/// (CR 119.9). The control at 20 life gains 2 in full.
#[test]
fn luminescent_rain_one_below_the_cap_applies_the_named_type_and_saturates() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (start life, named type, life after, gained after, LifeChanged events)
    type Row = (
        i32,
        &'static str,
        i32,
        u32,
        Vec<(PlayerId, i32, Option<i32>)>,
    );
    let rows: [Row; 3] = [
        (
            i32::MAX - 1,
            "Merfolk",
            i32::MAX,
            2,
            vec![(P0, 2, Some(i32::MAX))],
        ),
        (i32::MAX - 1, "Elf", i32::MAX - 1, 0, vec![]),
        (20, "Merfolk", 22, 2, vec![(P0, 2, Some(22))]),
    ];
    for (start_life, named, life_after, gained_after, expected_changes) in rows {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, start_life);
        scenario.add_real_card(P0, "Merfolk of the Pearl Trident", Zone::Battlefield, db);
        let rain = scenario
            .add_spell_to_hand_from_oracle(P0, "Luminescent Rain", true, LUMINESCENT_RAIN)
            .id();
        let mut runner = scenario.build();
        let _ = runner.cast(rain).commit();

        let mut events = Vec::new();
        for _ in 0..8 {
            if runner.state().stack.is_empty() || !is_priority(runner.state()) {
                break;
            }
            events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority resolves the spell")
                    .events,
            );
        }
        let WaitingFor::NamedChoice {
            player,
            choice_type: engine::types::ability::ChoiceType::CreatureType { .. },
            options,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "reach guard: the spell asks for a creature type, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert!(
            options.iter().any(|option| option == "Merfolk"),
            "reach guard: Merfolk is offered: {options:?}"
        );
        events.extend(
            runner
                .act(GameAction::ChooseOption {
                    choice: named.to_string(),
                })
                .expect("the named type is accepted")
                .events,
        );

        assert_eq!(runner.life(P0), life_after, "{start_life} / {named}");
        assert_eq!(
            runner.state().players[0].life_gained_this_turn,
            gained_after,
            "{start_life} / {named}"
        );
        assert_eq!(
            life_changes(&events),
            expected_changes,
            "{start_life} / {named}"
        );
        assert_eq!(
            runner.state().objects[&rain].zone,
            Zone::Graveyard,
            "{start_life} / {named}: the spell finishes resolving"
        );
        assert!(runner.state().stack.is_empty(), "{start_life} / {named}");
        assert!(is_priority(runner.state()), "{start_life} / {named}");
    }
}

/// A loss past `i32::MIN` lands on the limit (CR 107.1b: a life total may be
/// negative) and counts the rules amount. Platinum Angel ("You can't lose the
/// game and your opponents can't win the game.") keeps P1 in the game at
/// `i32::MIN`; once it leaves, the state-based action ends the game (CR 104.3b
/// + CR 704.5a).
#[test]
fn a_loss_past_i32_min_lands_on_the_limit_and_the_player_still_loses() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, i32::MIN + 1);
    let angel = scenario.add_real_card(P1, "Platinum Angel", Zone::Battlefield, db);
    let sign_in_blood = scenario.add_real_card(P0, "Sign in Blood", Zone::Hand, db);
    let unsummon = scenario.add_real_card(P0, "Unsummon", Zone::Hand, db);
    scenario.with_library_top(P1, &["Filler One", "Filler Two", "Filler Three"]);
    scenario.with_mana_pool(
        P0,
        vec![
            mana(ManaType::Black),
            mana(ManaType::Black),
            mana(ManaType::Blue),
        ],
    );
    let mut runner = scenario.build();

    // Sign in Blood: "Target player draws two cards and loses 2 life."
    let outcome = runner
        .cast(sign_in_blood)
        .target_player(P1)
        .try_resolve()
        .expect("Sign in Blood resolves");
    assert_eq!(runner.life(P1), i32::MIN);
    assert_eq!(runner.state().players[1].life_lost_this_turn, 2);
    assert_eq!(
        life_changes(outcome.events()),
        vec![(P1, -2, Some(i32::MIN))]
    );
    assert!(
        !runner.state().players[1].is_eliminated,
        "Platinum Angel keeps P1 in the game"
    );
    assert!(is_priority(runner.state()));

    // Unsummon: "Return target creature to its owner's hand."
    runner
        .cast(unsummon)
        .target_object(angel)
        .try_resolve()
        .expect("Unsummon resolves");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(winner) } if winner == P0
        ),
        "P1 at i32::MIN loses once Platinum Angel leaves; got {:?}",
        runner.state().waiting_for
    );
}

/// "Whenever you gain life" triggers fire for a player already at `i32::MAX`:
/// Lochmere Serpent's "{B}, Sacrifice a Swamp: You gain 1 life and draw a
/// card." gains 1 that moves the total 0, yet the gain is a life-gain event
/// of the rules amount (CR 119.9), so Ajani's Pridemate ("Whenever you gain
/// life, put a +1/+1 counter on this creature.") and Sanguine Bond ("Whenever
/// you gain life, target opponent loses that much life.") both resolve. The
/// control at 20 life is identical apart from P0's total.
#[test]
fn life_gain_triggers_fire_for_a_player_at_the_cap() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    for (start_life, life_after) in [(i32::MAX, i32::MAX), (20, 21)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, start_life);
        let serpent = scenario.add_real_card(P0, "Lochmere Serpent", Zone::Battlefield, db);
        let pridemate = scenario.add_real_card(P0, "Ajani's Pridemate", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Sanguine Bond", Zone::Battlefield, db);
        let swamp = scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
        scenario.with_mana_pool(P0, vec![mana(ManaType::Black)]);
        scenario.with_library_top(P0, &["Filler One", "Filler Two"]);
        let mut runner = scenario.build();
        let hand_before = runner.state().players[0].hand.len();

        let outcome = runner
            .activate(serpent, 1)
            .pay_with(&[swamp])
            .try_resolve()
            .expect("the Serpent's gain resolves with its triggers");

        assert_eq!(runner.life(P0), life_after, "start {start_life}");
        assert_eq!(
            runner.state().players[0].life_gained_this_turn,
            1,
            "start {start_life}"
        );
        assert_eq!(
            runner.state().objects[&pridemate]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
            "start {start_life}: the Pridemate trigger resolved"
        );
        assert_eq!(
            runner.life(P1),
            19,
            "start {start_life}: Sanguine Bond drains 1"
        );
        assert_eq!(
            runner.state().players[1].life_lost_this_turn,
            1,
            "start {start_life}"
        );
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + 1,
            "start {start_life}: the draw runs"
        );
        assert_eq!(
            life_changes(outcome.events()),
            vec![(P0, 1, Some(life_after)), (P1, -1, Some(19))],
            "start {start_life}"
        );
        assert!(runner.state().stack.is_empty(), "start {start_life}");
        assert!(is_priority(runner.state()), "start {start_life}");
    }
}

/// The edit authority (`GameState::apply_resolved_player_edit`, the one place
/// every journaled player-resource edit lands) saturates a life total at the
/// `i32` limits and the per-turn counters at `u32::MAX`, and re-applying the
/// recorded command on the same prefix reproduces the result. Energy and
/// player counters keep the checked edit (an engine numeric limit).
#[test]
fn the_edit_authority_saturates_life_and_keeps_energy_and_player_counters_checked() {
    let player = |state: &GameState| state.players[0].clone();

    // Gain 2 at i32::MAX - 1, and its journal replay on the pre-state.
    let mut state = GameState::new_two_player(7);
    state.players[0].life = i32::MAX - 1;
    let pre_state = state.clone();
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta: 2 })
        .expect("the gain saturates");
    assert_eq!(state.players[0].life, i32::MAX);
    assert_eq!(state.players[0].life_gained_this_turn, 2);
    let command = state
        .resolved_rules_journal
        .entries()
        .iter()
        .find_map(|entry| match &entry.command {
            Some(ResolvedRulesCommand::PlayerEdit(command)) => Some(command.clone()),
            _ => None,
        })
        .expect("the saturating edit is journaled");
    let mut replay = pre_state;
    replay
        .apply_resolved_player_edit(&command)
        .expect("the recorded command replays");
    assert_eq!(player(&replay), player(&state), "replay is deterministic");

    // Lose 2 at i32::MIN + 1.
    let mut state = GameState::new_two_player(7);
    state.players[0].life = i32::MIN + 1;
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta: -2 })
        .expect("the loss saturates");
    assert_eq!(state.players[0].life, i32::MIN);
    assert_eq!(state.players[0].life_lost_this_turn, 2);

    // At exactly the limit the total does not move, yet the counter counts.
    let mut state = GameState::new_two_player(7);
    state.players[0].life = i32::MAX;
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta: 1 })
        .expect("a gain at the cap is accepted");
    assert_eq!(state.players[0].life, i32::MAX);
    assert_eq!(state.players[0].life_gained_this_turn, 1);
    let mut state = GameState::new_two_player(7);
    state.players[0].life = i32::MIN;
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta: -1 })
        .expect("a loss at the floor is accepted");
    assert_eq!(state.players[0].life, i32::MIN);
    assert_eq!(state.players[0].life_lost_this_turn, 1);

    // The per-turn counters saturate at u32::MAX.
    let mut state = GameState::new_two_player(7);
    state.players[0].life_gained_this_turn = u32::MAX - 1;
    state.players[0].life_lost_this_turn = u32::MAX - 1;
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta: 2 })
        .expect("the gained counter saturates");
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta: -3 })
        .expect("the lost counter saturates");
    assert_eq!(state.players[0].life, 19);
    assert_eq!(state.players[0].life_gained_this_turn, u32::MAX);
    assert_eq!(state.players[0].life_lost_this_turn, u32::MAX);

    // Energy and Experience keep the checked edit: one below the limit lands
    // on it (reach guard), at the limit the edit is refused unchanged.
    let experience = |delta| ResolvedPlayerEdit::Counter {
        kind: PlayerCounterKind::Experience,
        delta,
    };
    let mut state = GameState::new_two_player(7);
    state.players[0].energy = u32::MAX - 1;
    state
        .players
        .first_mut()
        .expect("P0 is seated")
        .player_counters
        .insert(PlayerCounterKind::Experience, u32::MAX - 1);
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Energy { delta: 1 })
        .expect("energy lands on u32::MAX");
    state
        .resolve_and_apply_player_edit(P0, experience(1))
        .expect("experience lands on u32::MAX");
    assert_eq!(state.players[0].energy, u32::MAX);
    assert_eq!(
        state.players[0].player_counter(&PlayerCounterKind::Experience),
        u32::MAX
    );
    let at_limit = player(&state);
    assert_eq!(
        state.resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Energy { delta: 1 }),
        Err(ResolvedPlayerEditReplayInvariantError::ResourceOverflow)
    );
    assert_eq!(
        state.resolve_and_apply_player_edit(P0, experience(1)),
        Err(ResolvedPlayerEditReplayInvariantError::ResourceOverflow)
    );
    assert_eq!(player(&state), at_limit, "a refused edit changes nothing");
}

/// Soul Conduit ("{6}, {T}: Two target players exchange life totals.") between
/// `i32::MIN` and `i32::MAX`: each player gains or loses `u32::MAX`, the exact
/// difference (CR 701.12c), and lands on the other's previous total. P1 at
/// `i32::MIN` then loses the game (CR 704.5a). The control (5, 20) swaps by 15.
#[test]
fn exchanging_life_totals_across_the_whole_range_lands_exactly() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    for (p0_start, p1_start) in [(i32::MIN, i32::MAX), (5, 20)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, p0_start);
        scenario.with_life(P1, p1_start);
        scenario.add_real_card(P0, "Platinum Angel", Zone::Battlefield, db);
        let conduit = scenario.add_real_card(P0, "Soul Conduit", Zone::Battlefield, db);
        scenario.with_mana_pool(P0, mana_pool(ManaType::Colorless, 6));
        let mut runner = scenario.build();

        let outcome = runner
            .activate(conduit, 0)
            .target_players(&[P0, P1])
            .try_resolve()
            .expect("Soul Conduit's exchange resolves");
        assert!(
            runner.state().objects[&conduit].tapped,
            "reach guard: Soul Conduit paid its {{T}} cost"
        );

        let gained = p1_start.abs_diff(p0_start);
        let loss = i32::try_from(gained).map_or(i32::MIN, |gained| -gained);
        let gain = i32::try_from(gained).unwrap_or(i32::MAX);
        assert_eq!(
            life_changes(outcome.events()),
            vec![(P0, gain, Some(p1_start)), (P1, loss, Some(p0_start))],
            "({p0_start}, {p1_start})"
        );
        assert_eq!(runner.life(P0), p1_start);
        assert_eq!(runner.life(P1), p0_start);
        assert_eq!(runner.state().players[0].life_gained_this_turn, gained);
        assert_eq!(runner.state().players[1].life_lost_this_turn, gained);
        if p0_start == i32::MIN {
            assert!(
                won_by(runner.state(), P0),
                "P1 at i32::MIN loses; got {:?}",
                runner.state().waiting_for
            );
        } else {
            assert!(runner.state().stack.is_empty());
            assert!(is_priority(runner.state()));
        }
    }
}

/// Angel of Grace ("{4}{W}{W}, Exile this card from your graveyard: Your life
/// total becomes 10.") from `i32::MIN`: P0 gains 2,147,483,658, the exact
/// difference (CR 119.5), and lands on 10; the event reports `i32::MAX`. The
/// control from 20 loses 10.
#[test]
fn setting_life_from_i32_min_lands_exactly_on_the_target() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (start, LifeChanged amount, gained, lost)
    for (start, amount, gained, lost) in [(i32::MIN, i32::MAX, 2_147_483_658, 0), (20, -10, 0, 10)]
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, start);
        scenario.add_real_card(P0, "Platinum Angel", Zone::Battlefield, db);
        let angel = scenario.add_real_card(P0, "Angel of Grace", Zone::Graveyard, db);
        scenario.with_mana_pool(P0, mana_pool(ManaType::White, 6));
        let mut runner = scenario.build();

        let outcome = runner
            .activate(angel, 0)
            .try_resolve()
            .expect("Angel of Grace's ability resolves");

        assert_eq!(
            life_changes(outcome.events()),
            vec![(P0, amount, Some(10))],
            "start {start}"
        );
        assert_eq!(runner.life(P0), 10, "start {start}");
        assert_eq!(runner.state().players[0].life_gained_this_turn, gained);
        assert_eq!(runner.state().players[0].life_lost_this_turn, lost);
        assert_eq!(
            runner.state().objects[&angel].zone,
            Zone::Exile,
            "reach guard: the exile cost was paid"
        );
        assert!(runner.state().stack.is_empty(), "start {start}");
        assert!(is_priority(runner.state()), "start {start}");
    }
}

/// Tree of Perdition ("{T}: Exchange target opponent's life total with this
/// creature's toughness.", 0/13) against an opponent at `i32::MIN`: P1 gains
/// 2,147,483,661 (CR 701.12g) and lands on 13; the Tree's toughness becomes
/// `i32::MIN` (CR 613.4b), so it is put into its owner's graveyard (CR 704.5f).
/// Platinum Angel keeps P1 in the game at `i32::MIN` before the exchange.
#[test]
fn exchanging_life_with_toughness_from_i32_min_lands_exactly() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, i32::MIN);
    scenario.add_real_card(P1, "Platinum Angel", Zone::Battlefield, db);
    let tree = scenario.add_real_card(P0, "Tree of Perdition", Zone::Battlefield, db);
    let mut runner = scenario.build();

    let outcome = runner
        .activate(tree, 0)
        .target_player(P1)
        .try_resolve()
        .expect("Tree of Perdition's exchange resolves");

    assert_eq!(
        life_changes(outcome.events()),
        vec![(P1, i32::MAX, Some(13))]
    );
    assert_eq!(runner.life(P1), 13);
    assert_eq!(
        runner.state().players[1].life_gained_this_turn,
        2_147_483_661
    );
    assert!(
        !runner.state().players[1].is_eliminated,
        "reach guard: P1 stays in the game"
    );
    assert_eq!(runner.state().objects[&tree].zone, Zone::Graveyard);
    assert!(runner.state().players[0].graveyard.contains(&tree));
    assert!(runner.state().stack.is_empty());
    assert!(is_priority(runner.state()));
}

/// Reverse the Sands ("Redistribute any number of players' life totals.")
/// cast with P0 at `i32::MIN` and P1 at `i32::MAX`: the prompt offers the
/// identity and the swap, and the swap gives each player the other's total by
/// gaining or losing `u32::MAX`. P1 then loses the game (CR 704.5a).
#[test]
fn redistributing_life_totals_across_the_whole_range_lands_exactly() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, i32::MIN);
    scenario.with_life(P1, i32::MAX);
    scenario.add_real_card(P0, "Platinum Angel", Zone::Battlefield, db);
    let sands = scenario
        .add_spell_to_hand_from_oracle(P0, "Reverse the Sands", false, REVERSE_THE_SANDS)
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(sands).commit();
    pass_until_prompt(&mut runner);

    let WaitingFor::RedistributeLifeTotals { player, options } = &runner.state().waiting_for else {
        panic!(
            "reach guard: the spell asks for a redistribution, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let assignments: Vec<Vec<(PlayerId, i32)>> = options
        .iter()
        .map(|option| option.assignment.clone())
        .collect();
    assert_eq!(
        assignments,
        vec![
            vec![(P0, i32::MIN), (P1, i32::MAX)],
            vec![(P0, i32::MAX), (P1, i32::MIN)],
        ]
    );

    let result = runner
        .act(GameAction::SubmitLifeRedistribution { option_index: 1 })
        .expect("the swap is accepted");
    assert_eq!(
        life_changes(&result.events),
        vec![
            (P0, i32::MAX, Some(i32::MAX)),
            (P1, i32::MIN, Some(i32::MIN)),
        ]
    );
    assert_eq!(runner.life(P0), i32::MAX);
    assert_eq!(runner.life(P1), i32::MIN);
    assert_eq!(runner.state().players[0].life_gained_this_turn, u32::MAX);
    assert_eq!(runner.state().players[1].life_lost_this_turn, u32::MAX);
    assert!(
        won_by(runner.state(), P0),
        "P1 at i32::MIN loses; got {:?}",
        runner.state().waiting_for
    );
}

/// Two-Headed Giant: a life change adjusts the team's shared total (CR 810.9),
/// which stops at the `i32` limit the change moves toward; the acting
/// player's field takes the change first and only what it cannot hold moves
/// to the teammate. The event and the gained/lost counter report the rules
/// amount. A team whose total reaches 0 or less loses (CR 810.8c).
#[test]
fn two_headed_giant_life_changes_cap_at_the_team_total() {
    // (P0, P1, gain?, amount, fields after, event new_total, team lost?)
    type Row = (i32, i32, bool, u32, (i32, i32), i32, bool);
    let rows: [Row; 5] = [
        (
            i32::MAX - 10,
            5,
            true,
            100,
            (i32::MAX - 5, 5),
            i32::MAX - 5,
            false,
        ),
        (
            i32::MAX - 1,
            -100,
            true,
            50,
            (i32::MAX, -51),
            i32::MAX,
            false,
        ),
        (
            i32::MIN + 10,
            -5,
            false,
            100,
            (i32::MIN + 5, -5),
            i32::MIN + 5,
            true,
        ),
        (i32::MIN + 1, 100, false, 50, (i32::MIN, 51), i32::MIN, true),
        (15, 15, true, 5, (20, 15), 20, false),
    ];
    for (p0, p1, gain, amount, fields_after, new_total, team_lost) in rows {
        let label = format!("({p0}, {p1}) {} {amount}", if gain { "+" } else { "-" });
        let mut scenario = two_headed_giant();
        scenario.with_life(P0, p0);
        scenario.with_life(P1, p1);
        let mut runner = scenario.build();
        runner.state_mut().debug_mode = true;
        let action = if gain {
            DebugAction::GainLife {
                player_id: P0,
                amount,
            }
        } else {
            DebugAction::LoseLife {
                player_id: P0,
                amount,
            }
        };
        let result = runner
            .act(GameAction::Debug(action))
            .expect("the debug life change applies");

        let signed = if gain {
            i32::try_from(amount).unwrap()
        } else {
            -i32::try_from(amount).unwrap()
        };
        assert_eq!(
            life_changes(&result.events),
            vec![(P0, signed, Some(new_total))],
            "{label}"
        );
        assert_eq!(team_a_fields(runner.state()), fields_after, "{label}");
        let (gained, lost) = if gain { (amount, 0) } else { (0, amount) };
        assert_eq!(
            (
                runner.state().players[0].life_gained_this_turn,
                runner.state().players[0].life_lost_this_turn,
            ),
            (gained, lost),
            "{label}: the acting player counts the rules amount"
        );
        assert_eq!(
            (
                runner.state().players[1].life_gained_this_turn,
                runner.state().players[1].life_lost_this_turn,
            ),
            (0, 0),
            "{label}: the teammate's counters do not move"
        );
        if team_lost {
            assert!(
                won_by(runner.state(), P2),
                "{label}: got {:?}",
                runner.state().waiting_for
            );
        } else {
            assert!(is_priority(runner.state()), "{label}");
        }
    }

    // The cap row's team total is exactly i32::MAX.
    let mut scenario = two_headed_giant();
    scenario.with_life(P0, i32::MAX - 10);
    scenario.with_life(P1, 5);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::GainLife {
            player_id: P0,
            amount: 100,
        }))
        .expect("the debug gain applies");
    assert_eq!(team_life_sum(runner.state(), P0), i64::from(i32::MAX));
}

/// The edit authority caps a Two-Headed Giant team's shared total and stores
/// the change acting field first (CR 810.9), and replaying the journaled
/// command on the same prefix reproduces both fields, also where the change
/// spills onto the teammate's field. A total outside `i32` (two members at
/// `i32::MAX`) is never moved past the limit by a gain and moves exactly by a
/// loss. A player who has left the game adjusts only its own total, also beside
/// a departed teammate whose field has room.
#[test]
fn the_edit_authority_caps_a_shared_life_total_and_replays_its_spread() {
    let fields = |state: &GameState| -> Vec<i32> {
        state.players.iter().map(|player| player.life).collect()
    };
    // Team A at `(p0, p1)`, P0 changes by `delta`: the edited state, and the
    // journaled command replayed on the same prefix.
    let edit_and_replay = |p0: i32, p1: i32, delta: i64| -> (GameState, GameState) {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 7);
        state.players[0].life = p0;
        state.players[1].life = p1;
        let pre_state = state.clone();
        state
            .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta })
            .expect("the life change is accepted");
        let command = state
            .resolved_rules_journal
            .entries()
            .iter()
            .find_map(|entry| match &entry.command {
                Some(ResolvedRulesCommand::PlayerEdit(command)) => Some(command.clone()),
                _ => None,
            })
            .expect("the edit is journaled");
        assert_eq!(command.edit, ResolvedPlayerEdit::Life { delta });
        let mut replay = pre_state;
        replay
            .apply_resolved_player_edit(&command)
            .expect("the recorded command replays");
        (state, replay)
    };

    // Cap: (MAX - 10, 5) + 100 -> (MAX - 5, 5), and its journal replay.
    let (state, replay) = edit_and_replay(i32::MAX - 10, 5, 100);
    assert_eq!(team_a_fields(&state), (i32::MAX - 5, 5));
    assert_eq!(team_life_sum(&state, P0), i64::from(i32::MAX));
    assert_eq!(state.players[0].life_gained_this_turn, 100);
    assert_eq!(fields(&replay), fields(&state), "the capped change replays");
    assert_eq!(replay.players[0], state.players[0]);

    // Spill: (MAX - 1, -100) + 50 -> P0's field takes 1, the spread moves 49
    // onto P1's (CR 810.9 + CR 810.9a), and its journal replay.
    let (state, replay) = edit_and_replay(i32::MAX - 1, -100, 50);
    assert_eq!(team_a_fields(&state), (i32::MAX, -51));
    assert_eq!(team_life_sum(&state, P0), i64::from(i32::MAX) - 51);
    assert_eq!(state.players[0].life_gained_this_turn, 50);
    assert_eq!(fields(&replay), fields(&state), "the spread replays");
    assert_eq!(
        (&replay.players[0], &replay.players[1]),
        (&state.players[0], &state.players[1])
    );

    // A team total outside `i32`: a gain does not move it, a loss moves it
    // by exactly the amount, on the acting field.
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 7);
    state.players[0].life = i32::MAX;
    state.players[1].life = i32::MAX;
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta: 1 })
        .expect("a gain past the cap is accepted");
    assert_eq!(team_a_fields(&state), (i32::MAX, i32::MAX));
    assert_eq!(state.players[0].life_gained_this_turn, 1);
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta: -1 })
        .expect("a loss is accepted");
    assert_eq!(team_a_fields(&state), (i32::MAX - 1, i32::MAX));
    assert_eq!(state.players[0].life_lost_this_turn, 1);

    // A player who has left the game adjusts only its own total, also beside a
    // departed teammate whose field has room: team A has left the game, both
    // members together (CR 810.8a), with P1's field at the cap.
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 7);
    state.players[0].life = -100;
    state.players[1].life = i32::MAX;
    state.players[0].is_eliminated = true;
    state.players[1].is_eliminated = true;
    state
        .resolve_and_apply_player_edit(P1, ResolvedPlayerEdit::Life { delta: 5 })
        .expect("the departed player's gain is accepted");
    assert_eq!(state.players[1].life, i32::MAX);
    assert_eq!(state.players[1].life_gained_this_turn, 5);
    assert_eq!(
        state.players[0].life, -100,
        "the departed teammate's field takes no part of the gain"
    );
}

/// Archenemy Commander: the heroes share one life total (CR 904.13b), which a life change
/// adjusts as in Two-Headed Giant (CR 810.9). With the heroes' fields at (`i32::MAX`,
/// `i32::MAX - 2`, `i32::MIN`), a shared total of `i32::MAX - 3`, P1 casts Whitesun's Passage
/// ("You gain 5 life."). The total stops at `i32::MAX` (an engine storage limit,
/// `arithmetic::saturating_life_total`), so it moves by 3, stored on P1's field first and then
/// on each teammate's in turn (`players::spread_life_change`): P1's field is full, P2's takes 2
/// and P3's takes the last 1. The `LifeChanged` event and P1's gained counter report the rules
/// amount, 5 (CR 119.3), and the archenemy's own total does not move.
#[test]
fn an_archenemy_commander_gain_spills_past_the_first_teammate_at_the_cap() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let p3 = PlayerId(3);
    let heroes = [P1, P2, p3];
    let mut scenario = GameScenario::new_with_format(FormatConfig::archenemy_commander(), 4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for (hero, life) in heroes.into_iter().zip([i32::MAX, i32::MAX - 2, i32::MIN]) {
        scenario.with_life(hero, life);
    }
    let passage = scenario.add_real_card(P1, "Whitesun's Passage", Zone::Hand, db);
    scenario.with_mana_pool(P1, mana_pool(ManaType::White, 2));
    let mut runner = scenario.build();
    let archenemy_life = runner.life(P0);
    assert_eq!(
        team_life_sum(runner.state(), P1),
        i64::from(i32::MAX) - 3,
        "REACH-GUARD: the heroes' shared total is 3 below the cap"
    );

    // CR 117.3d: the archenemy, the active player, passes and P1 receives priority.
    runner
        .act(GameAction::PassPriority)
        .expect("the archenemy passes priority");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 holds priority, got {:?}",
        runner.state().waiting_for
    );
    let outcome = runner.cast(passage).resolve();

    assert_eq!(
        life_changes(outcome.events()),
        vec![(P1, 5, Some(i32::MAX))]
    );
    assert_eq!(
        heroes.map(|hero| runner.life(hero)),
        [i32::MAX, i32::MAX, i32::MIN + 1]
    );
    assert_eq!(team_life_sum(runner.state(), P1), i64::from(i32::MAX));
    assert_eq!(
        heroes.map(|hero| runner.state().players[usize::from(hero.0)].life_gained_this_turn),
        [5, 0, 0],
        "the acting hero counts the rules amount; the teammates' counters do not move"
    );
    assert_eq!(
        runner.life(P0),
        archenemy_life,
        "the archenemy's total does not move"
    );
    assert!(
        runner
            .state()
            .players
            .iter()
            .all(|player| !player.is_eliminated),
        "the heroes' total is above 0 (CR 810.8c)"
    );
    assert_eq!(runner.state().objects[&passage].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(is_priority(runner.state()));
}

/// Angel of Grace ("Your life total becomes 10.") in Two-Headed Giant sets
/// the team's total to 10 (CR 810.9c): P0 gains or loses the difference from
/// the team's exact total (CR 810.9a), which a hand-built state may put outside
/// `i32`; the difference lands exactly while it is at most `u32::MAX`, as in
/// every row here, and the part of a change P0's field cannot hold is stored on
/// the teammate's.
#[test]
fn two_headed_giant_set_life_lands_the_team_on_its_target() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (P0, P1, fields after, LifeChanged, gained, lost)
    type Row = (i32, i32, (i32, i32), (PlayerId, i32, Option<i32>), u32, u32);
    let rows: [Row; 3] = [
        (
            i32::MAX,
            i32::MIN + 5,
            (i32::MAX, i32::MIN + 11),
            (P0, 6, Some(i32::MAX)),
            6,
            0,
        ),
        (
            i32::MAX,
            i32::MAX,
            (i32::MIN + 11, i32::MAX),
            (P0, i32::MIN, Some(i32::MIN + 11)),
            0,
            4_294_967_284,
        ),
        (15, 15, (-5, 15), (P0, -20, Some(-5)), 0, 20),
    ];
    for (p0, p1, fields_after, change, gained, lost) in rows {
        let mut scenario = two_headed_giant();
        scenario.with_life(P0, p0);
        scenario.with_life(P1, p1);
        let angel = scenario.add_real_card(P0, "Angel of Grace", Zone::Graveyard, db);
        scenario.with_mana_pool(P0, mana_pool(ManaType::White, 6));
        let mut runner = scenario.build();

        let outcome = runner
            .activate(angel, 0)
            .try_resolve()
            .expect("Angel of Grace's ability resolves");

        assert_eq!(life_changes(outcome.events()), vec![change], "({p0}, {p1})");
        assert_eq!(team_a_fields(runner.state()), fields_after, "({p0}, {p1})");
        assert_eq!(team_life_sum(runner.state(), P0), 10, "({p0}, {p1})");
        assert_eq!(
            (
                runner.state().players[0].life_gained_this_turn,
                runner.state().players[0].life_lost_this_turn,
            ),
            (gained, lost),
            "({p0}, {p1})"
        );
        assert_eq!(runner.state().objects[&angel].zone, Zone::Exile);
        assert!(runner.state().stack.is_empty(), "({p0}, {p1})");
        assert!(is_priority(runner.state()), "({p0}, {p1})");
    }
}

/// Minion of the Wastes ("As this creature enters, pay any amount of life.
/// ... power and toughness are each equal to the life paid as it entered.")
/// in Two-Headed Giant with both members at `i32::MAX`: the payment is
/// bounded by the team's exact total (CR 119.4a), a payment of 3,000,000,000
/// is deducted from the team's shared total (CR 810.9), and the life paid,
/// past `i32::MAX`, saturates the creature's power and toughness. The control
/// pays 7.
#[test]
fn two_headed_giant_pays_life_above_i32_max_as_minion_of_the_wastes_enters() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (paid, LifeChanged, P0 after, P/T)
    type Row = (u32, (PlayerId, i32, Option<i32>), i32, i32);
    let rows: [Row; 2] = [
        (
            3_000_000_000,
            (P0, i32::MIN, Some(-852_516_353)),
            -852_516_353,
            i32::MAX,
        ),
        (7, (P0, -7, Some(i32::MAX - 7)), i32::MAX - 7, 7),
    ];
    for (paid, change, p0_after, pt) in rows {
        let mut scenario = two_headed_giant();
        scenario.with_life(P0, i32::MAX);
        scenario.with_life(P1, i32::MAX);
        let minion = scenario.add_real_card(P0, "Minion of the Wastes", Zone::Hand, db);
        scenario.with_mana_pool(P0, mana_pool(ManaType::Black, 6));
        let mut runner = scenario.build();
        let _ = runner.cast(minion).commit();
        pass_until_prompt(&mut runner);

        let WaitingFor::PayAmountChoice {
            player,
            resource: PayableResource::Life,
            min,
            max,
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "reach guard: the entry payment is asked, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((player, min, max), (P0, 0, 4_294_967_294), "paid {paid}");

        let result = runner
            .act(GameAction::SubmitPayAmount { amount: paid })
            .expect("the payment is accepted");
        assert_eq!(life_changes(&result.events), vec![change], "paid {paid}");
        assert_eq!(
            team_a_fields(runner.state()),
            (p0_after, i32::MAX),
            "paid {paid}"
        );
        assert_eq!(
            team_life_sum(runner.state(), P0),
            2 * i64::from(i32::MAX) - i64::from(paid),
            "paid {paid}"
        );
        assert_eq!(runner.state().players[0].life_lost_this_turn, paid);
        let object = &runner.state().objects[&minion];
        assert_eq!(object.zone, Zone::Battlefield, "paid {paid}");
        assert_eq!((object.power, object.toughness), (Some(pt), Some(pt)));
        assert!(runner.state().stack.is_empty(), "paid {paid}");
        assert!(is_priority(runner.state()), "paid {paid}");
    }
}

/// Plunge into Darkness, second mode ("Pay any amount of life, then look at
/// that many cards from the top of your library. Put one of those cards into
/// your hand and exile the rest.") in Two-Headed Giant: a payment of
/// 3,000,000,000 is "that many", clamped to the `i32` quantity range, so P0
/// looks at the whole three-card library. A synthetic loss-halving
/// replacement (no card claim) modifies the loss to 1,500,000,000, but the
/// cost paid is still the declared amount (CR 118.11), so the count is the
/// same. No printed card halves a life loss, and the row needs a replacement
/// that lowers the loss below the declared payment: a printed doubler would
/// take both past `i32::MAX`, where the clamped count cannot tell them apart.
/// The nearest printed card is Bloodletter of Aclazotz ("If an opponent would
/// lose life during your turn, they lose twice that much life instead.").
#[test]
fn pay_any_amount_of_life_above_i32_max_reads_the_amount_paid() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (halved?, LifeChanged, P0 after, lost)
    type Row = (bool, (PlayerId, i32, Option<i32>), i32, u32);
    let rows: [Row; 2] = [
        (
            false,
            (P0, i32::MIN, Some(-852_516_353)),
            -852_516_353,
            3_000_000_000,
        ),
        (
            true,
            (P0, -1_500_000_000, Some(647_483_647)),
            647_483_647,
            1_500_000_000,
        ),
    ];
    for (halved, change, p0_after, lost) in rows {
        let mut scenario = two_headed_giant();
        scenario.with_life(P0, i32::MAX);
        scenario.with_life(P1, i32::MAX);
        let plunge = scenario.add_real_card(P0, "Plunge into Darkness", Zone::Hand, db);
        scenario.with_library_top(P0, &["Filler One", "Filler Two", "Filler Three"]);
        scenario.with_mana_pool(P0, mana_pool(ManaType::Black, 2));
        let mut runner = scenario.build();
        if halved {
            let shield = engine::game::zones::create_object(
                runner.state_mut(),
                CardId(990),
                P0,
                "Half Shield".to_string(),
                Zone::Battlefield,
            );
            runner
                .state_mut()
                .objects
                .get_mut(&shield)
                .expect("the synthetic source exists")
                .replacement_definitions
                .push(
                    ReplacementDefinition::new(ReplacementEvent::LoseLife)
                        .quantity_modification(QuantityModification::Half),
                );
        }
        let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
        assert_eq!(library.len(), 3, "reach guard: three library cards");

        let _ = runner.cast(plunge).modes(&[1]).commit();
        pass_until_prompt(&mut runner);
        let WaitingFor::PayAmountChoice {
            player,
            resource: PayableResource::Life,
            min,
            max,
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "reach guard: the life payment is asked, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((player, min, max), (P0, 0, 4_294_967_294));

        let result = runner
            .act(GameAction::SubmitPayAmount {
                amount: 3_000_000_000,
            })
            .expect("the payment is accepted");
        assert_eq!(
            life_changes(&result.events),
            vec![change],
            "halved {halved}"
        );
        assert_eq!(team_a_fields(runner.state()), (p0_after, i32::MAX));
        assert_eq!(runner.state().players[0].life_lost_this_turn, lost);
        assert_eq!(
            runner.state().last_effect_count,
            Some(i32::MAX),
            "halved {halved}: the declared payment, clamped"
        );
        let WaitingFor::DigChoice {
            player,
            cards,
            keep_count,
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "halved {halved}: P0 looks at that many cards, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, P0);
        let mut looked = cards.clone();
        looked.sort();
        let mut expected = library.clone();
        expected.sort();
        assert_eq!(looked, expected, "halved {halved}: the whole library");
        assert_eq!(keep_count, 1);

        let top = library[0];
        runner
            .act(GameAction::SelectCards { cards: vec![top] })
            .expect("the kept card is accepted");
        assert_eq!(runner.state().players[0].hand.len(), 1);
        assert!(runner.state().players[0].hand.contains(&top));
        for &rest in &library[1..] {
            assert_eq!(runner.state().objects[&rest].zone, Zone::Exile);
        }
        assert!(runner.state().stack.is_empty(), "halved {halved}");
        assert!(is_priority(runner.state()), "halved {halved}");
    }
}

/// Beacon of Immortality ("Double target player's life total.") on a player
/// at `i32::MIN`: doubling is a loss of 2,147,483,648 (CR 701.10d), which the
/// total cannot move past, and the event reports `i32::MIN`. Platinum Angel
/// keeps P0 in the game.
#[test]
fn doubling_a_life_total_at_i32_min_reports_the_saturated_loss() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, i32::MIN);
    scenario.add_real_card(P0, "Platinum Angel", Zone::Battlefield, db);
    let beacon = scenario
        .add_spell_to_hand_from_oracle(P0, "Beacon of Immortality", true, BEACON_OF_IMMORTALITY)
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(beacon)
        .target_player(P0)
        .try_resolve()
        .expect("Beacon of Immortality resolves");

    assert_eq!(
        life_changes(outcome.events()),
        vec![(P0, i32::MIN, Some(i32::MIN))]
    );
    assert_eq!(runner.life(P0), i32::MIN);
    assert_eq!(runner.state().players[0].life_lost_this_turn, 2_147_483_648);
    assert!(!runner.state().players[0].is_eliminated);
    assert!(runner.state().stack.is_empty());
    assert!(is_priority(runner.state()));
}

/// Blessed Wine's verbatim Oracle text (not a member of the test fixture): its
/// gain of 1 starts a life-gain loop. Its delayed draw comes only at the next
/// turn's upkeep, which no test here reaches.
pub(crate) const BLESSED_WINE: &str =
    "You gain 1 life.\nDraw a card at the beginning of the next turn's upkeep.";

/// Marauding Blight-Priest's verbatim Oracle text: each gain makes every
/// opponent lose 1 life.
const MARAUDING_BLIGHT_PRIEST: &str = "Whenever you gain life, each opponent loses 1 life.";

/// A two-player game in P0's precombat main phase.
fn two_player() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// Passes priority while priority is open, at most `beats` times, each pass
/// accepted; returns every event.
fn pass_beats(runner: &mut GameRunner, beats: usize) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..beats {
        if !is_priority(runner.state()) {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority is accepted")
                .events,
        );
    }
    events
}

/// Passes until the stack is empty at priority or a loop shortcut is offered;
/// returns every pass's events.
fn settle(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::LoopShortcut { .. } => return events,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return events,
            WaitingFor::Priority { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("passing priority is accepted")
                        .events,
                );
            }
            other => panic!("unexpected prompt while settling an activation: {other:?}"),
        }
    }
    panic!("the activation never settled");
}

/// Activates `source`'s ability `index`, then passes until the stack is empty
/// at priority or a loop shortcut is offered; returns every event.
fn activate_and_settle(runner: &mut GameRunner, source: ObjectId, index: usize) -> Vec<GameEvent> {
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the activation is accepted")
        .events;
    events.extend(settle(runner));
    events
}

/// The life state a shortcut or the performed cycles leave: P0's gained and lost
/// counters, P1's gained counter, team A's stored fields (`(P0, P1)`), P0's
/// shared total, and the last new total of P0's `LifeChanged` events. A
/// performed take's record keeps only its first and last cycle (CR 732.1b), so
/// the record's sum is not compared; its last new total is the last cycle's.
#[derive(Debug, PartialEq, Eq)]
struct LifeOutcome {
    gained: u32,
    lost: u32,
    teammate_gained: u32,
    fields: (i32, i32),
    team: i64,
    last_new_total: Option<i32>,
}

impl LifeOutcome {
    fn read(runner: &GameRunner, events: &[GameEvent]) -> Self {
        LifeOutcome {
            gained: runner.state().players[0].life_gained_this_turn,
            lost: runner.state().players[0].life_lost_this_turn,
            teammate_gained: runner.state().players[1].life_gained_this_turn,
            fields: team_a_fields(runner.state()),
            team: team_life_sum(runner.state(), P0),
            last_new_total: life_changes(events)
                .into_iter()
                .rev()
                .find(|&(player, _, _)| player == P0)
                .and_then(|(_, _, new_total)| new_total),
        }
    }
}

/// Passes priority until the current step ends and its boundary runs: returns
/// as soon as a non-priority prompt surfaces (the `LoopCollapse` amount
/// choice) or the phase advances (as
/// `combo_infinite_pile::drive_priority_to_next_boundary`).
fn pass_to_next_boundary(runner: &mut GameRunner) {
    let start_phase = runner.state().phase;
    for _ in 0..64 {
        if !is_priority(runner.state()) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority toward the next boundary is accepted");
        if !is_priority(runner.state()) || runner.state().phase != start_phase {
            return;
        }
    }
    panic!("no phase boundary reached within 64 passes");
}

/// CR 104.4b: P0's Worldgorger Dragon ("Flying, trample" / "When this creature enters, exile all
/// other permanents you control." / "When this creature leaves the battlefield, return the exiled
/// cards to the battlefield under their owners' control.") in the graveyard, and Animate Dead
/// ("Enchant creature card in a graveyard" / "When this Aura enters, if it's on the battlefield, it
/// loses "enchant creature card in a graveyard" and gains "enchant creature put onto the
/// battlefield with this Aura." Return enchanted creature card to the battlefield under your
/// control and attach this Aura to it. When this Aura leaves the battlefield, that creature's
/// controller sacrifices it." / "Enchanted creature gets -1/-0.") cast on it with {B}{B} floating
/// and left on the stack, beside what `add` puts on the board. From there the two repeat with no
/// player choice: each cycle the Dragon enters and dies.
fn dragon_loop(
    db: &'static engine::database::card_db::CardDatabase,
    mut scenario: GameScenario,
    detection: LoopDetectionMode,
    add: impl FnOnce(&mut GameScenario),
) -> GameRunner {
    let dragon = scenario.add_real_card(P0, "Worldgorger Dragon", Zone::Graveyard, db);
    let animate = scenario.add_real_card(P0, "Animate Dead", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana_pool(ManaType::Black, 2));
    add(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = detection;
    drop(runner.cast(animate).target_object(dragon).commit());
    runner
}

/// Asserts the standing shortcut offer: P0's, with exactly `unbounded` as its
/// unbounded axes, minted from P0's recorded period with a counted take that
/// performs (CR 732.2a), so every count the declaration names is admitted.
fn assert_recorded_period_offer(runner: &GameRunner, unbounded: &[ResourceAxis]) {
    let WaitingFor::LoopShortcut {
        proposer,
        certificate,
        road,
        counted_take,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "reach guard: one cycle offers the loop shortcut, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*proposer, P0);
    assert_eq!(certificate.unbounded, unbounded);
    assert_eq!(*road, OfferRoad::RecordedPeriod);
    assert_eq!(*counted_take, CountedTake::Performs);
}

/// Answers each `RespondToShortcut` with `Accept`, asserting that the
/// responders are exactly `expected`, in order; returns every answer's events.
fn accept_for_every_responder(runner: &mut GameRunner, expected: &[PlayerId]) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for &responder in expected {
        let WaitingFor::RespondToShortcut { player, .. } = &runner.state().waiting_for else {
            panic!(
                "expected {responder:?} to respond to the shortcut, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, responder);
        events.extend(
            runner
                .act(GameAction::RespondToShortcut {
                    response: ShortcutResponse::Accept,
                })
                .expect("accepting the shortcut is accepted")
                .events,
        );
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "no further responder: {:?}",
        runner.state().waiting_for
    );
    events
}

/// Passes to the step boundary and asserts that it asks no collapse amount.
fn pass_to_a_boundary_without_a_collapse(runner: &mut GameRunner) {
    pass_to_next_boundary(runner);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::PayAmountChoice { .. }
        ),
        "the boundary asks no collapse amount, got {:?}",
        runner.state().waiting_for
    );
}

/// P0's stashed persistent-growth items.
fn stash(runner: &GameRunner) -> Option<&Vec<PersistentAxisMaterialization>> {
    runner.state().pending_unbounded_materialization.get(&P0)
}

/// P0's `LifeChanged` amounts in `events`, in order.
fn p0_life_amounts(events: &[GameEvent]) -> Vec<i64> {
    life_changes(events)
        .into_iter()
        .filter(|&(player, _, _)| player == P0)
        .map(|(_, amount, _)| i64::from(amount))
        .collect()
}

/// Asserts a performed take's record (CR 732.1b): it keeps the first and last of
/// its `cycles` repetitions, each reporting `cycle` as P0's `LifeChanged` amounts,
/// and holds no other change of P0's.
fn assert_kept_cycles(name: &str, events: &[GameEvent], cycles: u32, cycle: &[i64]) {
    let kept: Vec<(PlayerId, u32)> = match cycles {
        0 => Vec::new(),
        1 => vec![(P0, 1)],
        _ => vec![(P0, 1), (P0, cycles)],
    };
    assert_eq!(
        markers(events),
        kept,
        "{name}: the record keeps the first and last cycle"
    );
    let kept_amounts: Vec<Vec<i64>> = repetitions(events)
        .into_iter()
        .map(p0_life_amounts)
        .collect();
    let record = p0_life_amounts(events);
    assert_eq!(
        kept_amounts,
        vec![cycle.to_vec(); kept.len()],
        "{name}: each kept cycle reports its changes, got {record:?}"
    );
    assert_eq!(
        record,
        kept_amounts.concat(),
        "{name}: the record holds only the kept cycles' changes"
    );
}

/// How a taken life shortcut lands at its accept (CR 732.2c).
#[derive(Clone, Copy)]
enum Landing {
    /// Performed: each kept repetition reports these `LifeChanged` amounts.
    Performed(&'static [i64]),
    /// One lump of this amount, with no repetition marker.
    Lump(i64),
}

/// Asserts that the accept's `events` land a take of `cycles` as `landing` says.
fn assert_landing(name: &str, events: &[GameEvent], cycles: u32, landing: Landing) {
    match landing {
        Landing::Performed(cycle) => assert_kept_cycles(name, events, cycles, cycle),
        Landing::Lump(lump) => {
            assert_eq!(
                markers(events),
                Vec::new(),
                "REACH-GUARD {name}: a lump performs no repetition"
            );
            assert_eq!(
                p0_life_amounts(events),
                vec![lump],
                "{name}: one lump at the accept"
            );
        }
    }
}

/// CR 616.1 + CR 616.1f + CR 119.3: Dazzling Reflection ("You gain life equal
/// to target creature's power. The next time that creature would deal damage
/// this turn, prevent that damage.") on a creature of power `i32::MAX`, doubled
/// by Boon Reflection ("If you would gain life, you gain twice that much life
/// instead.") alone and with Rhox Faithmender (the same doubler) in either
/// order: the gain past `u32` lands on `u32::MAX`, the total stops at
/// `i32::MAX`, and the rest of the spell resolves. The control at power 100
/// with both doublers gains 400.
#[test]
fn boon_reflection_doubles_a_gain_past_u32_and_the_total_saturates() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (doublers, power, ordering answer, life after, gained after, event amount)
    type Row = (&'static [&'static str], i32, Option<usize>, i32, u32, i32);
    let rows: [Row; 4] = [
        (
            &["Boon Reflection"],
            i32::MAX,
            None,
            i32::MAX,
            4_294_967_294,
            i32::MAX,
        ),
        (
            &["Rhox Faithmender", "Boon Reflection"],
            i32::MAX,
            Some(0),
            i32::MAX,
            u32::MAX,
            i32::MAX,
        ),
        (
            &["Rhox Faithmender", "Boon Reflection"],
            i32::MAX,
            Some(1),
            i32::MAX,
            u32::MAX,
            i32::MAX,
        ),
        (
            &["Rhox Faithmender", "Boon Reflection"],
            100,
            Some(0),
            420,
            400,
            400,
        ),
    ];
    for (doublers, power, answer, life_after, gained_after, amount) in rows {
        let label = format!("{doublers:?} power {power} answer {answer:?}");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for doubler in doublers {
            scenario.add_real_card(P0, doubler, Zone::Battlefield, db);
        }
        let creature = scenario.add_creature(P0, "Test Behemoth", power, 1).id();
        let spell = scenario.add_real_card(P0, "Dazzling Reflection", Zone::Hand, db);
        scenario.with_mana_pool(P0, vec![mana(ManaType::White), mana(ManaType::White)]);
        let mut runner = scenario.build();

        // Both of the spell's target slots name the creature.
        let outcome = runner
            .cast(spell)
            .target_object(creature)
            .target_object(creature)
            .try_resolve()
            .expect("Dazzling Reflection resolves to its gain");
        let mut events = outcome.events().to_vec();
        match answer {
            Some(index) => {
                let WaitingFor::ReplacementChoice {
                    player,
                    candidate_count,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!(
                        "{label}: two doublers ask the CR 616.1 ordering prompt, got {:?}",
                        runner.state().waiting_for
                    );
                };
                assert_eq!(*player, P0, "{label}");
                assert_eq!(*candidate_count, 2, "{label}");
                events.extend(
                    runner
                        .act(GameAction::ChooseReplacement { index })
                        .expect("the ordering answer is accepted")
                        .events,
                );
            }
            None => assert!(
                !matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ),
                "{label}: one doubler asks no prompt"
            ),
        }

        assert_eq!(runner.life(P0), life_after, "{label}");
        assert_eq!(
            runner.state().players[0].life_gained_this_turn,
            gained_after,
            "{label}: the gain counts the rules amount"
        );
        assert_eq!(
            life_changes(&events),
            vec![(P0, amount, Some(life_after))],
            "{label}"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::PreventDamage,
                    ..
                }
            )),
            "{label}: the prevention clause resolves"
        );
        assert_eq!(
            runner.state().objects[&spell].zone,
            Zone::Graveyard,
            "{label}: the spell finishes resolving"
        );
        assert!(runner.state().stack.is_empty(), "{label}");
        assert!(is_priority(runner.state()), "{label}");
    }
}

/// CR 104.4b: an optional loop is never drawn. Heliod, Sun-Crowned ("Whenever
/// you gain life, put a +1/+1 counter on target creature or enchantment you
/// control.") and Spike Feeder ("Remove a +1/+1 counter from this creature:
/// You gain 2 life.") make one: each activation gains 2 and Heliod puts the
/// counter back. Below the cap the loop is offered as a shortcut after one
/// cycle (CR 732.2a). At `i32::MAX` the life axis is held by the cap, so the
/// loop offers no growth: it is never offered or drawn, while every gain still
/// counts its rules amount and triggers Heliod (CR 119.3).
#[test]
fn an_optional_life_gain_loop_at_the_cap_is_never_drawn_or_offered() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (start life, cycles, offered after the first cycle)
    for (start_life, cycles, offered) in [(20, 1, true), (i32::MAX, 12, false)] {
        let mut scenario = two_player();
        scenario.with_life(P0, start_life);
        scenario.add_real_card(P0, "Heliod, Sun-Crowned", Zone::Battlefield, db);
        let feeder = scenario.add_real_card(P0, "Spike Feeder", Zone::Battlefield, db);
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        // Fixture setup: `add_real_card` does not run "This creature enters
        // with two +1/+1 counters on it.", so the counters are placed here.
        runner
            .state_mut()
            .objects
            .get_mut(&feeder)
            .expect("Spike Feeder is on the battlefield")
            .counters
            .insert(CounterType::Plus1Plus1, 2);
        let gain = runner.state().objects[&feeder]
            .abilities
            .iter()
            .position(|ability| matches!(&*ability.effect, Effect::GainLife { .. }))
            .expect("reach guard: Spike Feeder's life-gain ability parses");

        for cycle in 0..cycles {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: feeder,
                    ability_index: gain,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("the activation is accepted");
            pass_beats(&mut runner, 2);
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::TriggerTargetSelection { player, .. } if player == P0
                ),
                "{start_life}, cycle {cycle}: Heliod triggers on the gain, got {:?}",
                runner.state().waiting_for
            );
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(feeder)),
                })
                .expect("Spike Feeder is a legal target");
            pass_beats(&mut runner, 2);
            if offered {
                break;
            }
            assert!(
                is_priority(runner.state()) && runner.state().stack.is_empty(),
                "{start_life}, cycle {cycle}: never offered or drawn, got {:?}",
                runner.state().waiting_for
            );
        }

        if offered {
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::LoopShortcut { proposer, .. } if proposer == P0
                ),
                "below the cap the loop is offered, got {:?}",
                runner.state().waiting_for
            );
            continue;
        }
        assert_eq!(runner.state().unbounded_resources.get(&P0), None);
        assert_eq!(
            runner.state().objects[&feeder]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&2)
        );
        assert_eq!(runner.life(P0), i32::MAX);
        assert_eq!(runner.state().players[0].life_gained_this_turn, 24);
    }
}

/// CR 104.4b + CR 732.4: [`dragon_loop`]'s Worldgorger Dragon and Animate Dead repeat with no
/// player choice, and P1's Soul Warden ("Whenever another creature enters, you gain 1 life.") gains
/// 1 each time the Dragon enters, so the loop holds only mandatory actions and is a draw. Below the
/// cap each cycle is net progress with no loss axis. At `i32::MAX`, from one below the cap, and for
/// a Two-Headed Giant team whose shared total is at the cap (CR 810.4), the stored total stops
/// moving, so the position recurs with no resource change, which is a draw; every gain still
/// carries its rules amount (CR 119.3), so the gained counter counts it. A per-turn counter (gained
/// or lost) pinned at `u32::MAX` changes nothing here. The cards fix that the game is a draw and
/// that every gain counts; how many gains have resolved when the draw is called is where the
/// detection first sees the recurrence. In a duel P1's trigger goes on the stack after P0's and
/// resolves first (CR 603.3b); in Two-Headed Giant the team orders them (CR 805.7), and these rows
/// resolve the Warden's first. With loop detection off nothing is drawn.
#[test]
fn a_mandatory_life_gain_loop_at_the_cap_is_drawn() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let max = i32::MAX;
    let add_warden = |scenario: &mut GameScenario| {
        scenario.add_real_card(P1, WARDEN, Zone::Battlefield, db);
    };
    // Two players: ((P1's life, gained preset, lost preset), (life, gained, lost) when drawn).
    type DuelRow = ((i32, u32, u32), (i32, u32, u32));
    let rows: [DuelRow; 5] = [
        ((20, 0, 0), (21, 1, 0)),
        ((max, 0, 0), (max, 2, 0)),
        ((max - 1, 0, 0), (max, 2, 0)),
        // The gained counter pinned at `u32::MAX`.
        ((max, u32::MAX, 0), (max, u32::MAX, 0)),
        // The lost counter pinned at `u32::MAX` while the gained counter moves.
        ((max, 0, u32::MAX), (max, 2, u32::MAX)),
    ];
    for ((start_life, gained, lost), drawn) in rows {
        let row = format!("({start_life}, {gained}, {lost})");
        let mut scenario = two_player();
        scenario.with_life(P1, start_life);
        let mut runner = dragon_loop(db, scenario, LoopDetectionMode::Interactive, add_warden);
        runner.state_mut().players[1].life_gained_this_turn = gained;
        runner.state_mut().players[1].life_lost_this_turn = lost;
        let drive = drive_mandatory_loop(&mut runner, 60, None);
        assert!(
            life_changes(&drive.events).contains(&(P1, 1, Some(drawn.0))),
            "REACH-GUARD {row}: the Warden's trigger gained 1, got {:?}",
            life_changes(&drive.events)
        );
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::GameOver { winner: None }
            ),
            "{row}: drawn, got {:?}",
            runner.state().waiting_for
        );
        let player = &runner.state().players[1];
        assert_eq!(
            (
                runner.life(P1),
                player.life_gained_this_turn,
                player.life_lost_this_turn
            ),
            drawn,
            "{row}"
        );
    }

    // Two-Headed Giant: (P0 and P1 fields, fields when drawn, team total, P1's gained counter).
    type TeamRow = ((i32, i32), (i32, i32), i64, u32);
    let rows: [TeamRow; 2] = [
        ((5, max - 5), (5, max - 5), i64::from(max), 2),
        ((20, 20), (20, 21), 41, 1),
    ];
    for ((p0, p1), fields, team, gained) in rows {
        let mut scenario = two_headed_giant();
        for (player, life) in [(P0, p0), (P1, p1), (P2, 20), (PlayerId(3), 20)] {
            scenario.with_life(player, life);
        }
        let mut runner = dragon_loop(db, scenario, LoopDetectionMode::Interactive, add_warden);
        drive_mandatory_loop(&mut runner, 60, Some(WARDEN));
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::GameOver { winner: None }
            ),
            "team ({p0}, {p1}): drawn, got {:?}",
            runner.state().waiting_for
        );
        assert_eq!(team_a_fields(runner.state()), fields, "team ({p0}, {p1})");
        assert_eq!(team_life_sum(runner.state(), P0), team, "team ({p0}, {p1})");
        assert_eq!(
            runner.state().players[1].life_gained_this_turn,
            gained,
            "team ({p0}, {p1})"
        );
    }

    // Loop detection off: nothing is drawn.
    let mut scenario = two_player();
    scenario.with_life(P1, max);
    let mut runner = dragon_loop(db, scenario, LoopDetectionMode::Off, add_warden);
    drive_mandatory_loop(&mut runner, 100, None);
    assert!(
        is_priority(runner.state()),
        "detection off: not drawn, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().players[1].life_gained_this_turn > 2,
        "REACH-GUARD detection off: the loop keeps gaining life past the beat it is drawn at \
         with detection on"
    );
    assert_eq!(runner.life(P1), max);
}

/// CR 104.4b: P0's two mandatory triggers feed each other (Exquisite Blood,
/// "Whenever an opponent loses life, you gain that much life.", and Marauding
/// Blight-Priest, "Whenever you gain life, each opponent loses 1 life."), P1
/// can't lose and P0 can't win (Platinum Angel), so the loop has no way to
/// stop and is a draw. P1's life total is held at `i32::MIN`, so the cycle
/// stores no loss and no loss axis vetoes the draw.
#[test]
fn a_mandatory_drain_loop_against_a_seat_held_at_i32_min_is_drawn() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let mut scenario = two_player();
    scenario.with_life(P0, 20);
    scenario.with_life(P1, i32::MIN);
    scenario.add_real_card(P1, "Platinum Angel", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, db);
    scenario.add_creature_from_oracle(P0, "Marauding Blight-Priest", 3, 2, MARAUDING_BLIGHT_PRIEST);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Blessed Wine", true, BLESSED_WINE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;

    runner.cast(kickoff).resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: None }
        ),
        "drawn, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.life(P1), i32::MIN);
    assert!(
        !runner.state().players[1].is_eliminated,
        "reach guard: Platinum Angel keeps P1 in the game"
    );
    assert!(
        runner.state().players[1].life_lost_this_turn >= 1,
        "reach guard: the drain reached P1"
    );
    assert_eq!(runner.life(P0), 22);
    assert_eq!(runner.state().players[0].life_gained_this_turn, 2);
}

/// CR 119.3 + CR 732.2c: on P0's board of Phyrexian Altar ("Sacrifice a creature: Add one mana of
/// any color."), Gravecrawler ("This creature can't block." / "You may cast this card from your
/// graveyard as long as you control a Zombie.") and Walking Corpse (a Zombie with no rules text),
/// two Soul Wardens ("Whenever another creature enters, you gain 1 life.") gain 2 a cycle. The
/// cycle that offers the shortcut leaves P0 at `i32::MAX - 5`, and three more cycles' 6 is more
/// than the total can take under `i32::MAX`, so the take performs the cycles: nothing is stashed,
/// the step boundary asks no collapse amount, the total stops at `i32::MAX`, the gained counter
/// counts every cycle, and the record keeps the first and last cycle (CR 732.1b), each reporting
/// both gains, also the one the cap holds back.
#[test]
fn a_life_shortcut_gaining_past_i32_max_is_performed_at_the_take() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let max = i32::MAX;
    let mut board = AltarBoard::new(db, &[WARDEN, WARDEN], AltarSeats::Duel(max - 7), 0, |_| ());
    board.cycle_to_the_offer();
    assert_recorded_period_offer(&board.runner, &[ResourceAxis::Life(P0)]);
    let events = board.take(3);

    assert_eq!(stash(&board.runner), None, "nothing is stashed");
    assert_eq!(board.life_reading(), (max, 8, 0));
    assert_eq!(
        markers(&events),
        vec![(P0, 1), (P0, 3)],
        "the record keeps the first and last cycle"
    );
    assert_eq!(
        repetitions(&events)
            .into_iter()
            .map(life_changes)
            .collect::<Vec<_>>(),
        vec![
            vec![(P0, 1, Some(max - 4)), (P0, 1, Some(max - 3))],
            vec![(P0, 1, Some(max)), (P0, 1, Some(max))],
        ],
        "each kept cycle reports both gains"
    );
    assert_eq!(
        life_changes(&events).len(),
        4,
        "the record holds only the kept cycles' changes"
    );
    assert!(is_priority(board.runner.state()));
    pass_to_a_boundary_without_a_collapse(&mut board.runner);
    assert_eq!(board.runner.life(P0), max);
}

/// CR 104.4b + CR 732.2a + CR 732.2c: on [`AltarBoard::netting_white`], two Soul Wardens ("Whenever
/// another creature enters, you gain 1 life.") gain 2 a cycle. At `i32::MAX` those gains cannot
/// grow the stored total, so the shortcut offer carries no life axis, only `Mana(White)`. Each gain
/// is held at the cap, so accepting performs the three cycles at the take: nothing is stashed, the
/// total stays at `i32::MAX`, the gained counter counts every gain (CR 119.3), past `i32::MAX` with
/// the counter preset near it, and the record keeps the first and last cycle (CR 732.1b), each
/// reporting both gains. With the gained counter pinned at `u32::MAX` the take is performed too,
/// since the cycle's `LifeChanged` events report each held gain where the pinned counter cannot:
/// nothing is stashed and the boundary asks no collapse amount.
#[test]
fn a_life_shortcut_offer_at_the_cap_carries_no_life_axis() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let max = i32::MAX;
    let offered = |gained: u32| {
        let mut board =
            AltarBoard::netting_white(db, &[WARDEN, WARDEN], AltarSeats::Duel(max), |_| ());
        board.runner.state_mut().players[0].life_gained_this_turn = gained;
        board.cycle_to_the_offer();
        assert_recorded_period_offer(&board.runner, &[ResourceAxis::Mana(ManaType::White)]);
        board
    };

    // Row A: the take performs the cycles, and the counter passes `i32::MAX`.
    let mut board = offered(2_147_483_644);
    let events = board.take(3);

    assert_eq!(stash(&board.runner), None, "nothing is stashed");
    assert_eq!(board.life_reading(), (max, 2_147_483_652, 0));
    assert_eq!(
        markers(&events),
        vec![(P0, 1), (P0, 3)],
        "the record keeps the first and last cycle"
    );
    assert_eq!(
        repetitions(&events)
            .into_iter()
            .map(life_changes)
            .collect::<Vec<_>>(),
        vec![vec![(P0, 1, Some(max)); 2]; 2],
        "each kept cycle reports both gains at the cap"
    );
    assert_eq!(
        life_changes(&events),
        vec![(P0, 1, Some(max)); 4],
        "the record holds only the kept cycles' changes"
    );
    assert!(is_priority(board.runner.state()));
    pass_to_a_boundary_without_a_collapse(&mut board.runner);

    // Row B: the gained counter is pinned at `u32::MAX`.
    let mut board = offered(u32::MAX);
    let events = board.take(3);
    assert_eq!(stash(&board.runner), None, "pinned: nothing is stashed");
    assert_eq!(
        markers(&events),
        vec![(P0, 1), (P0, 3)],
        "pinned: the record keeps the first and last cycle"
    );
    pass_to_a_boundary_without_a_collapse(&mut board.runner);
    assert_eq!(board.life_reading(), (max, u32::MAX, 0));
}

/// CR 119.3 + CR 732.2c: on [`AltarBoard::netting_white`], two Soul Wardens ("Whenever another
/// creature enters, you gain 1 life.") gain 2 a cycle. From one below the cap the gains of 100
/// cycles cannot fit under `i32::MAX`, so the take performs the cycles: nothing is stashed, the
/// boundary asks no collapse amount, the total stops at the cap, the gained counter grows by 200,
/// and the record keeps the first and last cycle (CR 732.1b), each reporting both gains. In
/// Two-Headed Giant the cap is the team's shared total (CR 810.4 + CR 810.9a). With the gained
/// counter pinned at `u32::MAX` the counter stays pinned and each kept cycle still reports both
/// gains. Control: the same Wardens on P0's board of Phyrexian Altar, Gravecrawler and Walking
/// Corpse gain 2 a cycle from 20, the gain fits, so it lands at the take in one lump of 200, and
/// the boundary asks nothing.
#[test]
fn a_life_shortcut_period_crossing_the_cap_gains_the_rules_amount() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    struct Row {
        name: &'static str,
        seats: AltarSeats,
        gained: u32,
        p0_life_at_offer: i32,
        p0_life_after: i32,
        gained_after: u32,
    }
    let max = i32::MAX;
    let rows = [
        Row {
            name: "crossing the cap",
            seats: AltarSeats::Duel(max - 3),
            gained: 0,
            p0_life_at_offer: max - 1,
            p0_life_after: max,
            gained_after: 202,
        },
        Row {
            name: "Two-Headed Giant team crossing the cap",
            seats: AltarSeats::TwoHeadedGiant(max - 8, 5),
            gained: 0,
            p0_life_at_offer: max - 6,
            p0_life_after: max - 5,
            gained_after: 202,
        },
        Row {
            name: "gained counter pinned at u32::MAX",
            seats: AltarSeats::Duel(max - 3),
            gained: u32::MAX,
            p0_life_at_offer: max - 1,
            p0_life_after: max,
            gained_after: u32::MAX,
        },
    ];
    for row in rows {
        let name = row.name;
        let mut board = AltarBoard::netting_white(db, &[WARDEN, WARDEN], row.seats, |_| ());
        board.runner.state_mut().players[0].life_gained_this_turn = row.gained;
        board.cycle_to_the_offer();
        assert_recorded_period_offer(&board.runner, &[ResourceAxis::Mana(ManaType::White)]);
        assert_eq!(board.runner.life(P0), row.p0_life_at_offer, "{name}");
        let events = board.take(100);

        assert_eq!(stash(&board.runner), None, "{name}: nothing is stashed");
        assert_eq!(
            markers(&events),
            vec![(P0, 1), (P0, 100)],
            "{name}: the record keeps the first and last cycle"
        );
        let kept: Vec<Vec<(PlayerId, i32)>> = repetitions(&events)
            .into_iter()
            .map(|cycle| {
                life_changes(cycle)
                    .into_iter()
                    .map(|(player, amount, _)| (player, amount))
                    .collect()
            })
            .collect();
        assert_eq!(
            kept,
            vec![vec![(P0, 1); 2]; 2],
            "{name}: each kept cycle reports both gains"
        );
        assert!(is_priority(board.runner.state()), "{name}");
        pass_to_a_boundary_without_a_collapse(&mut board.runner);

        assert_eq!(board.runner.life(P0), row.p0_life_after, "{name}");
        assert_eq!(
            life_changes(&events)
                .last()
                .and_then(|&(_, _, new_total)| new_total),
            Some(row.p0_life_after),
            "{name}"
        );
        if let AltarSeats::TwoHeadedGiant(..) = row.seats {
            assert_eq!(team_a_fields(board.runner.state()), (max - 5, 5), "{name}");
            assert_eq!(
                team_life_sum(board.runner.state(), P0),
                i64::from(max),
                "{name}"
            );
        }
        assert_eq!(
            board.runner.state().players[0].life_gained_this_turn,
            row.gained_after,
            "{name}"
        );
    }

    let name = "control below the cap";
    let mut board = AltarBoard::new(db, &[WARDEN, WARDEN], AltarSeats::Duel(20), 0, |_| ());
    board.cycle_to_the_offer();
    assert_recorded_period_offer(&board.runner, &[ResourceAxis::Life(P0)]);
    assert_eq!(board.runner.life(P0), 22, "{name}");
    let events = board.take(100);
    assert_eq!(stash(&board.runner), None, "{name}: nothing is stashed");
    assert_eq!(
        life_changes(&events),
        vec![(P0, 200, Some(222))],
        "{name}: one lump at the accept"
    );
    assert!(is_priority(board.runner.state()), "{name}");
    pass_to_a_boundary_without_a_collapse(&mut board.runner);
    assert_eq!(board.life_reading(), (222, 202, 0), "{name}");
}

/// Soulmender's verbatim Oracle text (not a member of the test fixture).
const SOULMENDER: &str = "{T}: You gain 1 life.";

/// A printed card of P0's activated after a shortcut's accept, or after the performed cycles, and
/// before the step boundary.
#[derive(Clone, Copy)]
enum AfterTheTake {
    /// Ancient Tomb ("{T}: Add {C}{C}. This land deals 2 damage to you."): P0 loses 2 (CR 120.3a).
    AncientTomb,
    /// Soulmender ("{T}: You gain 1 life."): P0 gains 1.
    Soulmender,
}

/// Adds `cards` to P0's battlefield; returns them to activate, in order.
fn add_after_the_take(
    scenario: &mut GameScenario,
    db: &'static engine::database::card_db::CardDatabase,
    cards: &[AfterTheTake],
) -> Vec<ObjectId> {
    cards
        .iter()
        .map(|card| match card {
            AfterTheTake::AncientTomb => {
                scenario.add_real_card(P0, "Ancient Tomb", Zone::Battlefield, db)
            }
            AfterTheTake::Soulmender => scenario
                .add_creature_from_oracle(P0, "Soulmender", 1, 1, SOULMENDER)
                .id(),
        })
        .collect()
}

/// Accepts `Fixed(cycles)` on the offer standing on a fresh `board()`, asserts that it lands as
/// `landing` says, stashes nothing, writes no collapse bound and marks nothing, activates the
/// board's [`AfterTheTake`] cards and passes to a boundary that asks no collapse amount. Asserts
/// that declining and performing the cycles ends the same, and returns the outcome.
fn assert_take_matches_performing(
    name: &str,
    board: &dyn Fn() -> AltarBoard<Vec<ObjectId>>,
    cycles: u32,
    landing: Landing,
) -> LifeOutcome {
    let activate_after_the_take = |board: &mut AltarBoard<Vec<ObjectId>>| {
        let mut events = Vec::new();
        for id in board.reader.clone() {
            events.extend(activate_and_settle(&mut board.runner, id, 0));
        }
        events
    };

    let mut taken = board();
    let mut events = taken.take(cycles);
    assert_landing(name, &events, cycles, landing);
    assert_eq!(stash(&taken.runner), None, "{name}: nothing is stashed");
    assert_eq!(
        taken.runner.state().pending_materialization_count.get(&P0),
        None,
        "{name}: no collapse bound"
    );
    assert_eq!(
        taken.runner.state().unbounded_resources.get(&P0),
        None,
        "{name}: nothing is marked"
    );
    events.extend(activate_after_the_take(&mut taken));
    pass_to_a_boundary_without_a_collapse(&mut taken.runner);
    let shortcut = LifeOutcome::read(&taken.runner, &[]);
    // The record is the take's alone: `play_trace` drives the reference without its events.
    assert_eq!(
        LifeOutcome::read(&taken.runner, &events).last_new_total,
        Some(shortcut.fields.0),
        "{name}: the record's last new total"
    );

    let mut performed = board();
    performed.perform(cycles);
    activate_after_the_take(&mut performed);
    pass_to_a_boundary_without_a_collapse(&mut performed.runner);
    assert_eq!(
        shortcut,
        LifeOutcome::read(&performed.runner, &[]),
        "{name}"
    );
    shortcut
}

/// CR 732.2c + CR 119.3: a taken shortcut leaves the game as if its cycles were performed at the
/// take, so P0's gained and lost counters, both stored life fields, the shared total and the last
/// new total P0's `LifeChanged` events report end where declining the shortcut and performing the
/// cycles leaves them, also when Ancient Tomb ("{T}: Add {C}{C}. This land deals 2 damage to you.")
/// costs P0 2 life (CR 120.3a) after the accept and before the step boundary. On
/// [`AltarBoard::netting_white`], two Soul Wardens ("Whenever another creature enters, you gain 1
/// life.") gain 2 a cycle, and in these rows an `i32` limit holds a gain back: a total held at the
/// cap, a Two-Headed Giant team's shared total held at it (CR 810.9 + CR 810.9a), and a period
/// crossing it, with a readable or, in a duel, a pinned counter. The offer names no life axis
/// there, only `Mana(White)`. Each take performs its cycles and its record shows the first and last
/// (CR 732.1b); the boundary asks no collapse amount, and nothing is marked. Where the offer names
/// P0's life the rows are in [`a_soul_warden_shortcut_matches_performing_the_cycles`], and a cycle
/// that also loses life is performed wherever its total stands
/// ([`a_gain_and_loss_shortcut_is_performed_and_every_counter_matches_performing`]).
#[test]
fn a_life_shortcut_held_at_the_cap_matches_performing_the_cycles() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    struct Row {
        name: &'static str,
        seats: AltarSeats,
        /// P0's gained counter at the start.
        preset: u32,
        after_the_take: &'static [AfterTheTake],
        cycles: u32,
        /// The `(P0, P1)` fields after.
        fields: (i32, i32),
        gained: u32,
        lost: u32,
    }
    use AfterTheTake::AncientTomb;
    use AltarSeats::{Duel, TwoHeadedGiant};
    let max = i32::MAX;
    let rows = [
        Row {
            name: "held at the cap",
            seats: Duel(max),
            preset: 0,
            after_the_take: &[],
            cycles: 3,
            fields: (max, 20),
            gained: 8,
            lost: 0,
        },
        Row {
            name: "held at the cap, then Ancient Tomb",
            seats: Duel(max),
            preset: 0,
            after_the_take: &[AncientTomb],
            cycles: 3,
            fields: (max - 2, 20),
            gained: 8,
            lost: 2,
        },
        Row {
            name: "Two-Headed Giant team held at the cap",
            seats: TwoHeadedGiant(max - 5, 5),
            preset: 0,
            after_the_take: &[],
            cycles: 3,
            fields: (max - 5, 5),
            gained: 8,
            lost: 0,
        },
        Row {
            name: "Two-Headed Giant team held at the cap, then Ancient Tomb",
            seats: TwoHeadedGiant(max - 5, 5),
            preset: 0,
            after_the_take: &[AncientTomb],
            cycles: 3,
            fields: (max - 7, 5),
            gained: 8,
            lost: 2,
        },
        Row {
            name: "crossing the cap",
            seats: Duel(max - 3),
            preset: 0,
            after_the_take: &[],
            cycles: 3,
            fields: (max, 20),
            gained: 8,
            lost: 0,
        },
        Row {
            name: "crossing the cap, then Ancient Tomb",
            seats: Duel(max - 3),
            preset: 0,
            after_the_take: &[AncientTomb],
            cycles: 3,
            fields: (max - 2, 20),
            gained: 8,
            lost: 2,
        },
        Row {
            name: "held at the cap with the gained counter pinned",
            seats: Duel(max),
            preset: u32::MAX,
            after_the_take: &[],
            cycles: 3,
            fields: (max, 20),
            gained: u32::MAX,
            lost: 0,
        },
        Row {
            name: "crossing the cap in one cycle with the gained counter pinned",
            seats: Duel(max - 3),
            preset: u32::MAX,
            after_the_take: &[],
            cycles: 1,
            fields: (max, 20),
            gained: u32::MAX,
            lost: 0,
        },
    ];
    for row in rows {
        let name = row.name;
        let board = || {
            let mut board = AltarBoard::netting_white(
                db,
                &[WARDEN, WARDEN],
                row.seats,
                |scenario: &mut GameScenario| add_after_the_take(scenario, db, row.after_the_take),
            );
            board.runner.state_mut().players[0].life_gained_this_turn = row.preset;
            board.cycle_to_the_offer();
            assert_recorded_period_offer(&board.runner, &[ResourceAxis::Mana(ManaType::White)]);
            board
        };
        let shortcut = assert_take_matches_performing(name, &board, row.cycles, BOTH_GAINS);
        assert_eq!(shortcut.fields, row.fields, "{name}: the fields");
        assert_eq!(
            (shortcut.gained, shortcut.lost),
            (row.gained, row.lost),
            "{name}: P0's counters"
        );
    }
}

/// CR 732.2c + CR 119.3: two life shortcuts taken in one step leave the game as if their cycles
/// were performed at each take. On P0's board of Phyrexian Altar, Gravecrawler and Walking Corpse,
/// two Soul Wardens ("Whenever another creature enters, you gain 1 life.") gain 2 a cycle. P0
/// performs one cycle, accepts `Fixed(4)`, performs one more cycle, accepts `Fixed(4)` again, then
/// loses 2 to Ancient Tomb ("{T}: Add {C}{C}. This land deals 2 damage to you.", CR 120.3a), and
/// the step ends; the reference declines, performs the same ten cycles and taps the Tomb. Each take
/// that fits applies its lump at the take, so neither stashes anything or records a collapse bound,
/// and the boundary asks nothing. From 20 both lumps fit: P0 is at 40 after the second take and
/// ends at 38. From `i32::MAX - 15` the first lump fits and the second take's cycles pass the cap,
/// so it performs them, its record keeping the first and last (CR 732.1b): P0 is at `i32::MAX`
/// after the second take and ends at `i32::MAX - 2`. Both have gained 20.
#[test]
fn two_life_shortcuts_in_one_step_match_performing_the_cycles() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (name, P0's starting life, P0's life after the second take, the record's repetition
    // markers, the amounts of P0's `LifeChanged` events in the takes' records and the loss)
    type Row = (&'static str, i32, i32, &'static [u32], &'static [i64]);
    let rows: [Row; 2] = [
        (
            "past the cap",
            i32::MAX - 15,
            i32::MAX,
            &[1, 4],
            &[8, 1, 1, 1, 1, -2],
        ),
        ("below the cap", 20, 40, &[], &[8, 8, -2]),
    ];
    for (name, start, after_takes, kept, record) in rows {
        let board = || {
            AltarBoard::new(
                db,
                &[WARDEN, WARDEN],
                AltarSeats::Duel(start),
                0,
                |scenario: &mut GameScenario| {
                    scenario.add_real_card(P0, "Ancient Tomb", Zone::Battlefield, db)
                },
            )
        };

        // The shortcut: one cycle, accept 4; one more cycle, accept 4; tap the Tomb.
        let mut taken = board();
        taken.cycle_to_the_offer();
        assert_recorded_period_offer(&taken.runner, &[ResourceAxis::Life(P0)]);
        let mut events = taken.take(4);
        taken.cycle_to_the_offer();
        events.extend(taken.take(4));
        assert_eq!(stash(&taken.runner), None, "{name}: nothing is stashed");
        assert_eq!(
            taken.runner.state().pending_materialization_count.get(&P0),
            None,
            "{name}: no bound is recorded"
        );
        assert_eq!(
            taken.runner.life(P0),
            after_takes,
            "{name}: P0 after the takes"
        );
        let tomb = taken.reader;
        events.extend(activate_and_settle(&mut taken.runner, tomb, 0));
        pass_to_a_boundary_without_a_collapse(&mut taken.runner);
        assert_eq!(
            markers(&events),
            kept.iter()
                .map(|&repetition| (P0, repetition))
                .collect::<Vec<_>>(),
            "{name}: a performed take's record keeps its first and last cycle"
        );
        assert_eq!(p0_life_amounts(&events), record, "{name}: P0's record");
        assert_eq!(
            LifeOutcome::read(&taken.runner, &events).last_new_total,
            Some(after_takes - 2),
            "{name}: the record's last new total"
        );
        let shortcut = LifeOutcome::read(&taken.runner, &[]);

        // The reference: decline, perform nine more cycles, tap the Tomb.
        let mut performed = board();
        performed.cycle_to_the_offer();
        performed.perform(9);
        let tomb = performed.reader;
        activate_and_settle(&mut performed.runner, tomb, 0);
        pass_to_a_boundary_without_a_collapse(&mut performed.runner);

        assert_eq!(
            shortcut,
            LifeOutcome::read(&performed.runner, &[]),
            "{name}"
        );
        assert_eq!(shortcut.gained, 20, "{name}: the gained counter");
    }
}

/// Angelic Accord's verbatim Oracle text (Scryfall).
const ANGELIC_ACCORD: &str = "At the beginning of each end step, if you gained 4 or more life \
                              this turn, create a 4/4 white Angel creature token with flying.";

/// Passes P0's turn forward to the end step (declaring no attackers, ordering
/// triggers in the order they are listed, and collapsing a stashed loop at its
/// maximum where a boundary asks) until the end step holds priority with an empty
/// stack, so its beginning-of-step trigger has resolved.
fn settle_end_step(runner: &mut GameRunner) {
    for _ in 0..200 {
        if runner.state().phase == Phase::End
            && is_priority(runner.state())
            && runner.state().stack.is_empty()
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is accepted");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .declare_attackers(&[])
                    .expect("declaring no attackers is accepted");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("the identity order is accepted");
            }
            WaitingFor::PayAmountChoice {
                resource: PayableResource::LoopCollapse { .. },
                max,
                ..
            } => {
                let amount = *max;
                runner
                    .act(GameAction::SubmitPayAmount { amount })
                    .expect("the collapse amount is accepted");
            }
            other => panic!("unexpected prompt on the way to the end step: {other:?}"),
        }
    }
    panic!("the end step was not reached within 200 beats");
}

/// CR 603.4 + CR 732.2c: Angelic Accord ("At the beginning of each end step, if you gained 4 or
/// more life this turn, create a 4/4 white Angel creature token with flying.") has an intervening
/// "if" that reads the life P0 gained this turn, and a taken shortcut gains what performing its
/// cycles would even while P0's total is held at `i32::MAX`. On [`AltarBoard::netting_white`] with
/// one Soul Warden ("Whenever another creature enters, you gain 1 life."), the cycle that offers
/// the shortcut and a `Fixed(3)` take gain 4, so the end step makes one Angel, and its entry gains
/// a fifth, as declining and performing the three cycles does; `Fixed(2)` gains 3 and makes none.
#[test]
fn angelic_accord_counts_the_life_a_shortcut_held_at_the_cap_gains() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (shortcut count, or `None` to decline and perform three cycles; gained before the end step;
    // Angel tokens; gained after it)
    for (count, gained, angels, gained_after) in
        [(Some(3), 4, 1, 5), (None, 4, 1, 5), (Some(2), 3, 0, 3)]
    {
        let mut board = AltarBoard::netting_white(
            db,
            &[WARDEN],
            AltarSeats::Duel(i32::MAX),
            |scenario: &mut GameScenario| {
                scenario.add_enchantment_from_oracle(P0, "Angelic Accord", ANGELIC_ACCORD);
            },
        );
        board.cycle_to_the_offer();
        assert_recorded_period_offer(&board.runner, &[ResourceAxis::Mana(ManaType::White)]);
        match count {
            Some(count) => {
                board.take(count);
            }
            None => board.perform(3),
        }
        assert_eq!(
            board.life_reading(),
            (i32::MAX, gained, 0),
            "{count:?}: life gained this turn"
        );

        settle_end_step(&mut board.runner);

        let state = board.runner.state();
        let angel_tokens = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token && object.name == "Angel")
            .count();
        assert_eq!(angel_tokens, angels, "{count:?}: Angel tokens");
        assert_eq!(
            board.life_reading(),
            (i32::MAX, gained_after, 0),
            "{count:?}: after the end step"
        );
    }
}

/// Children of Korlis' verbatim Oracle text (not a member of the test fixture).
const CHILDREN_OF_KORLIS: &str = "Sacrifice this creature: You gain life equal to the life you've \
                                  lost this turn. (Damage causes loss of life.)";

/// Serra Ascendant's verbatim Oracle text (not a member of the test fixture).
const SERRA_ASCENDANT: &str = "Lifelink (Damage dealt by this creature also causes you to gain \
                               that much life.)\nAs long as you have 30 or more life, this \
                               creature gets +5/+5 and has flying.";

const WARDEN: &str = "Soul Warden";
const CARNIVAL: &str = "Carnival of Souls";

/// A performed take of two Soul Wardens' cycle: each kept repetition reports both gains.
const BOTH_GAINS: Landing = Landing::Performed(&[1, 1]);

/// How an [`AltarBoard`]'s creature returns from the graveyard to be sacrificed each cycle.
#[derive(Clone, Copy)]
enum AltarLoop {
    /// P0 casts Gravecrawler ("This creature can't block." / "You may cast this card from your
    /// graveyard as long as you control a Zombie."), with Walking Corpse (a Zombie with no rules
    /// text) on the battlefield, and sacrifices it to Phyrexian Altar ("Sacrifice a creature: Add
    /// one mana of any color."), which makes this color.
    Gravecrawler(ManaType),
    /// P0 activates Reassembling Skeleton ("{1}{B}: Return this card from your graveyard to the
    /// battlefield tapped.") and sacrifices it to Ashnod's Altar ("Sacrifice a creature: Add
    /// {C}{C}.").
    ReassemblingSkeleton,
}

/// Who sits at an [`AltarBoard`], and the stored life fields of P0's side.
#[derive(Clone, Copy)]
enum AltarSeats {
    /// A duel, P0 at this life.
    Duel(i32),
    /// Two-Headed Giant, team A's fields `(P0, P1)` and team B's members at 20.
    TwoHeadedGiant(i32, i32),
}

/// P0's board of an [`AltarLoop`] and a Swamp, in Interactive loop detection. Each cycle returns
/// the loop's creature from the graveyard and sacrifices it to the Altar. `reader` is what
/// `add_reader` returns: the cards a row uses after the shortcut, or `()` for a row that uses none.
struct AltarBoard<R> {
    runner: GameRunner,
    altar_loop: AltarLoop,
    altar: ObjectId,
    fodder: ObjectId,
    reader: R,
    responders: Vec<PlayerId>,
}

impl<R> AltarBoard<R> {
    /// The Gravecrawler board, its Altar making {B}, with `payoffs` on P0's battlefield, in order,
    /// these `seats`, P0's lost counter preset to `lost`, and what `add_reader` adds.
    fn new(
        db: &'static engine::database::card_db::CardDatabase,
        payoffs: &[&str],
        seats: AltarSeats,
        lost: u32,
        add_reader: impl FnOnce(&mut GameScenario) -> R,
    ) -> Self {
        let altar_loop = AltarLoop::Gravecrawler(ManaType::Black);
        Self::build(db, altar_loop, payoffs, seats, lost, add_reader)
    }

    /// The Gravecrawler board on which the cycle nets mana: Intruder Alarm ("Creatures don't untap
    /// during their controllers' untap steps." / "Whenever a creature enters, untap all
    /// creatures.") untaps Birds of Paradise ("Flying" / "{T}: Add one mana of any color.") on each
    /// Gravecrawler's entry, Birds pays for Gravecrawler, and the Altar's white mana is net. So a
    /// cycle whose life an `i32` limit holds back still grows an axis, and its shortcut stays on
    /// offer. `payoffs`, `seats` and `add_reader` are as in [`Self::new`].
    fn netting_white(
        db: &'static engine::database::card_db::CardDatabase,
        payoffs: &[&str],
        seats: AltarSeats,
        add_reader: impl FnOnce(&mut GameScenario) -> R,
    ) -> Self {
        let altar_loop = AltarLoop::Gravecrawler(ManaType::White);
        Self::build(db, altar_loop, payoffs, seats, 0, |scenario| {
            add_alarm_and_birds(scenario, db);
            add_reader(scenario)
        })
    }

    /// The Reassembling Skeleton board in a duel at 20: Intruder Alarm untaps Birds of Paradise on
    /// each Skeleton's entry (the texts at [`Self::netting_white`]), so Birds and one of the
    /// Altar's two {C} pay for the next activation and the other {C} is net. `add_reader` is as in
    /// [`Self::new`].
    fn reassembling_skeleton(
        db: &'static engine::database::card_db::CardDatabase,
        add_reader: impl FnOnce(&mut GameScenario) -> R,
    ) -> Self {
        let altar_loop = AltarLoop::ReassemblingSkeleton;
        Self::build(db, altar_loop, &[], AltarSeats::Duel(20), 0, |scenario| {
            add_alarm_and_birds(scenario, db);
            add_reader(scenario)
        })
    }

    fn build(
        db: &'static engine::database::card_db::CardDatabase,
        altar_loop: AltarLoop,
        payoffs: &[&str],
        seats: AltarSeats,
        lost: u32,
        add_reader: impl FnOnce(&mut GameScenario) -> R,
    ) -> Self {
        let (mut scenario, fields, responders) = match seats {
            AltarSeats::Duel(life) => (two_player(), vec![(P0, life)], vec![P1]),
            AltarSeats::TwoHeadedGiant(p0, p1) => (
                two_headed_giant(),
                vec![(P0, p0), (P1, p1), (P2, 20), (PlayerId(3), 20)],
                vec![P1, P2, PlayerId(3)],
            ),
        };
        for (player, life) in fields {
            scenario.with_life(player, life);
        }
        let (altar, fodder) = match altar_loop {
            AltarLoop::Gravecrawler(_) => {
                let altar = scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db);
                scenario.add_real_card(P0, "Walking Corpse", Zone::Battlefield, db);
                let fodder = scenario.add_real_card(P0, "Gravecrawler", Zone::Graveyard, db);
                (altar, fodder)
            }
            AltarLoop::ReassemblingSkeleton => {
                let altar = scenario.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
                let fodder =
                    scenario.add_real_card(P0, "Reassembling Skeleton", Zone::Graveyard, db);
                (altar, fodder)
            }
        };
        scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
        for payoff in payoffs {
            scenario.add_real_card(P0, payoff, Zone::Battlefield, db);
        }
        let reader = add_reader(&mut scenario);
        for seat in [P0, P1] {
            for _ in 0..10 {
                scenario.add_real_card(seat, "Swamp", Zone::Library, db);
            }
        }
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        runner.state_mut().players[0].life_lost_this_turn = lost;
        AltarBoard {
            runner,
            altar_loop,
            altar,
            fodder,
            reader,
            responders,
        }
    }

    /// Settles, sacrificing the loop's creature to the Altar. Phyrexian Altar's color is the one
    /// its [`AltarLoop`] names; Ashnod's Altar asks for none, so the black here decides nothing.
    fn settle(&mut self) {
        let color = match self.altar_loop {
            AltarLoop::Gravecrawler(color) => color,
            AltarLoop::ReassemblingSkeleton => ManaType::Black,
        };
        let fodder = [self.fodder];
        let score = |action: &GameAction| {
            2 * crate::play_trace::names(&fodder)(action)
                + crate::play_trace::chooses_color(color)(action)
        };
        crate::play_trace::settle(&mut self.runner, &score);
    }

    fn return_fodder(&mut self) {
        match self.altar_loop {
            AltarLoop::Gravecrawler(_) => crate::play_trace::cast(
                &mut self.runner,
                self.fodder,
                vec![],
                CastPaymentMode::Auto,
            ),
            AltarLoop::ReassemblingSkeleton => {
                let index = crate::play_trace::ability(self.runner.state(), self.fodder, false);
                crate::play_trace::activate(&mut self.runner, self.fodder, index);
            }
        }
        self.settle();
    }

    fn sacrifice_fodder(&mut self) {
        let index = crate::play_trace::ability(self.runner.state(), self.altar, true);
        crate::play_trace::activate(&mut self.runner, self.altar, index);
        self.settle();
    }

    /// One return and one sacrifice, which offers the shortcut.
    fn cycle_to_the_offer(&mut self) {
        self.return_fodder();
        self.sacrifice_fodder();
        assert!(
            crate::play_trace::is_offer(self.runner.state()),
            "REACH-GUARD: one cycle offers the loop, got {:?}",
            self.runner.state().waiting_for
        );
    }

    fn decline_offers(&mut self) {
        while crate::play_trace::is_offer(self.runner.state()) {
            self.runner
                .act(GameAction::DeclineShortcut)
                .expect("declining the shortcut is accepted");
            self.settle();
        }
    }

    /// The reference: declines and performs `cycles` cycles.
    fn perform(&mut self, cycles: u32) {
        self.decline_offers();
        for _ in 0..cycles {
            self.return_fodder();
            self.decline_offers();
            self.sacrifice_fodder();
            self.decline_offers();
        }
    }

    /// Declares `Fixed(cycles)` and every responder accepts; returns the accepts' events.
    fn take(&mut self, cycles: u32) -> Vec<GameEvent> {
        self.runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(cycles),
                template: None,
            })
            .expect("the shortcut declaration is accepted");
        accept_for_every_responder(&mut self.runner, &self.responders)
    }

    /// P0's life, gained counter and lost counter.
    fn life_reading(&self) -> (i32, u32, u32) {
        let player = &self.runner.state().players[0];
        (
            player.life,
            player.life_gained_this_turn,
            player.life_lost_this_turn,
        )
    }

    fn pool(&self) -> usize {
        self.runner.state().players[0].mana_pool.total()
    }
}

/// Adds the Intruder Alarm and Birds of Paradise of [`AltarBoard::netting_white`] to P0's
/// battlefield.
fn add_alarm_and_birds(
    scenario: &mut GameScenario,
    db: &'static engine::database::card_db::CardDatabase,
) {
    scenario.add_real_card(P0, "Intruder Alarm", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Birds of Paradise", Zone::Battlefield, db);
}

/// CR 732.2c + CR 119.3 + CR 810.9: a cycle that both gains and loses life is performed at the take
/// wherever its total stands, since a lump would move P0's gained counter by the net and its lost
/// counter by nothing. On P0's board of Phyrexian Altar ("Sacrifice a creature: Add one mana of any
/// color."), Gravecrawler ("This creature can't block." / "You may cast this card from your
/// graveyard as long as you control a Zombie.") and Walking Corpse (a Zombie with no rules text),
/// each Soul Warden ("Whenever another creature enters, you gain 1 life.") gains 1 and Carnival of
/// Souls ("Whenever a creature enters, you lose 1 life and add {B}.") loses 1 a cycle, in the order
/// P0 stacks their triggers (CR 603.3b), and each `Fixed` count performs that many more cycles
/// after the one that offers it. Two gains then a loss from `i32::MAX - 4` hold the last cycle's
/// second gain at the cap before its loss, so P0 ends at `i32::MAX - 1`, and from `i32::MAX - 6` no
/// cycle reaches the cap and P0 ends at `i32::MAX - 2`. From `i32::MAX - 2` the second gain is held
/// in every cycle after the offer, at `Fixed(1)` as at `Fixed(3)`, and P0 ends at `i32::MAX - 1`. A
/// loss then two gains from `i32::MAX - 5` peaks one below the cap, and from `i32::MAX - 4` reaches
/// it exactly. Far below the cap P0 goes from 20 to 24, in a duel and on P0's own field in
/// Two-Headed Giant, where a Warden and Carnival leave the fields where they began. With the team's
/// total at `i32::MAX - 2` the second gain is held at the cap in every cycle after the offer, so
/// P0's field goes from `i32::MAX - 7` to `i32::MAX - 6` while P1's stays at 5. In every row the
/// offer names P0's life axis only where the cycle after it raises P0's field by the cycle's whole
/// net, the record keeps the first and last cycle (CR 732.1b), P1's gained counter stays 0, and
/// every counter, field and total matches declining and performing the cycles.
#[test]
fn a_gain_and_loss_shortcut_is_performed_and_every_counter_matches_performing() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    /// The payoffs, in order, and the `LifeChanged` amounts of the cycle their triggers make.
    type Cycle = (&'static [&'static str], &'static [i64]);
    const TWO_GAINS_THEN_A_LOSS: Cycle = (&[CARNIVAL, WARDEN, WARDEN], &[1, 1, -1]);
    const A_LOSS_THEN_TWO_GAINS: Cycle = (&[WARDEN, WARDEN, CARNIVAL], &[-1, 1, 1]);
    const A_LOSS_THEN_A_GAIN: Cycle = (&[WARDEN, CARNIVAL], &[-1, 1]);
    struct Row {
        name: &'static str,
        cycle: Cycle,
        seats: AltarSeats,
        cycles: u32,
        unbounded: Vec<ResourceAxis>,
        /// The `(P0, P1)` fields after.
        fields: (i32, i32),
        gained: u32,
        lost: u32,
    }
    let mana = ResourceAxis::Mana(ManaType::Black);
    let life = ResourceAxis::Life(P0);
    let max = i32::MAX;
    let rows = [
        Row {
            name: "two gains then a loss, the last cycle's second gain held at the cap",
            cycle: TWO_GAINS_THEN_A_LOSS,
            seats: AltarSeats::Duel(max - 4),
            cycles: 3,
            unbounded: vec![mana, life],
            fields: (max - 1, 20),
            gained: 8,
            lost: 4,
        },
        Row {
            name: "two gains then a loss no cycle carries to the cap",
            cycle: TWO_GAINS_THEN_A_LOSS,
            seats: AltarSeats::Duel(max - 6),
            cycles: 3,
            unbounded: vec![mana, life],
            fields: (max - 2, 20),
            gained: 8,
            lost: 4,
        },
        Row {
            name: "two gains then a loss, the second gain held at the cap in the take's one cycle",
            cycle: TWO_GAINS_THEN_A_LOSS,
            seats: AltarSeats::Duel(max - 2),
            cycles: 1,
            unbounded: vec![mana],
            fields: (max - 1, 20),
            gained: 4,
            lost: 2,
        },
        Row {
            name: "two gains then a loss, the second gain held at the cap every cycle",
            cycle: TWO_GAINS_THEN_A_LOSS,
            seats: AltarSeats::Duel(max - 2),
            cycles: 3,
            unbounded: vec![mana],
            fields: (max - 1, 20),
            gained: 8,
            lost: 4,
        },
        Row {
            name: "a loss then two gains peaking one below the cap",
            cycle: A_LOSS_THEN_TWO_GAINS,
            seats: AltarSeats::Duel(max - 5),
            cycles: 3,
            unbounded: vec![mana, life],
            fields: (max - 1, 20),
            gained: 8,
            lost: 4,
        },
        Row {
            name: "a loss then two gains reaching the cap exactly",
            cycle: A_LOSS_THEN_TWO_GAINS,
            seats: AltarSeats::Duel(max - 4),
            cycles: 3,
            unbounded: vec![mana, life],
            fields: (max, 20),
            gained: 8,
            lost: 4,
        },
        Row {
            name: "two gains then a loss far below the cap",
            cycle: TWO_GAINS_THEN_A_LOSS,
            seats: AltarSeats::Duel(20),
            cycles: 3,
            unbounded: vec![mana, life],
            fields: (24, 20),
            gained: 8,
            lost: 4,
        },
        Row {
            name: "Two-Headed Giant, two gains then a loss on P0's own field",
            cycle: TWO_GAINS_THEN_A_LOSS,
            seats: AltarSeats::TwoHeadedGiant(20, 20),
            cycles: 3,
            unbounded: vec![mana, life],
            fields: (24, 20),
            gained: 8,
            lost: 4,
        },
        Row {
            name: "Two-Headed Giant, a loss then a gain that net nothing",
            cycle: A_LOSS_THEN_A_GAIN,
            seats: AltarSeats::TwoHeadedGiant(20, 20),
            cycles: 3,
            unbounded: vec![mana],
            fields: (20, 20),
            gained: 4,
            lost: 4,
        },
        Row {
            name: "Two-Headed Giant, the team's total holds the second gain at the cap",
            cycle: TWO_GAINS_THEN_A_LOSS,
            seats: AltarSeats::TwoHeadedGiant(max - 7, 5),
            cycles: 3,
            unbounded: vec![mana],
            fields: (max - 6, 5),
            gained: 8,
            lost: 4,
        },
    ];
    for row in rows {
        let name = row.name;
        let (payoffs, cycle) = row.cycle;
        let mut taken = AltarBoard::new(db, payoffs, row.seats, 0, |_| ());
        taken.cycle_to_the_offer();
        assert_recorded_period_offer(&taken.runner, &row.unbounded);
        let events = taken.take(row.cycles);
        assert_kept_cycles(name, &events, row.cycles, cycle);
        assert_eq!(stash(&taken.runner), None, "{name}: nothing is stashed");
        assert_eq!(
            taken.runner.state().pending_materialization_count.get(&P0),
            None,
            "{name}: no collapse bound"
        );
        assert_eq!(
            taken.runner.state().unbounded_resources.get(&P0),
            None,
            "{name}: a performed take marks nothing"
        );
        pass_to_a_boundary_without_a_collapse(&mut taken.runner);
        // The record is the take's alone: `play_trace` drives the reference without its events.
        assert_eq!(
            LifeOutcome::read(&taken.runner, &events).last_new_total,
            Some(row.fields.0),
            "{name}: the record's last new total"
        );
        let shortcut = LifeOutcome::read(&taken.runner, &[]);

        let mut performed = AltarBoard::new(db, payoffs, row.seats, 0, |_| ());
        performed.cycle_to_the_offer();
        performed.perform(row.cycles);
        pass_to_a_boundary_without_a_collapse(&mut performed.runner);

        assert_eq!(
            shortcut,
            LifeOutcome::read(&performed.runner, &[]),
            "{name}"
        );
        assert_eq!(shortcut.fields, row.fields, "{name}: the fields");
        assert_eq!(
            (shortcut.gained, shortcut.lost),
            (row.gained, row.lost),
            "{name}: P0's counters"
        );
        assert_eq!(shortcut.teammate_gained, 0, "{name}: P1's gained counter");
    }
}

/// CR 732.2c + CR 119.3 + CR 810.9 + CR 810.9a: on P0's board of Phyrexian Altar ("Sacrifice a
/// creature: Add one mana of any color."), Gravecrawler ("This creature can't block." / "You may
/// cast this card from your graveyard as long as you control a Zombie.") and Walking Corpse (a
/// Zombie with no rules text), two Soul Wardens ("Whenever another creature enters, you gain 1
/// life.") gain 2 a cycle, and each `Fixed` count is that many more cycles after the one that
/// offers it. A taken shortcut leaves the game as if its cycles were performed at the take, so P0's
/// counters, both stored fields and the shared total end where declining and performing the cycles
/// leaves them, also when Ancient Tomb ("{T}: Add {C}{C}. This land deals 2 damage to you.") costs
/// P0 2 life (CR 120.3a) or Soulmender ("{T}: You gain 1 life.") gains 1 after the accept and
/// before the step boundary. A take whose cycles only gain, each gain stored as performing stores
/// it, applies the lump at the take: below the cap, reaching it exactly, also with a pinned
/// counter, short of it before Soulmender's gain carries the total to it, reaching it before
/// Soulmender's gain is held there and the Tomb's loss, from 1 before two Tombs' losses that would
/// be lethal without the lump, and beside a bystander pinned at the cap or another team's member
/// whose gained counter is pinned. Every other take performs its cycles, and its record shows the
/// first and last cycle (CR 732.1b): a lump one cycle past the cap, one past the shared total's
/// cap, and one P0's own field cannot hold, where the spread stores P0's gain in P1's field, also
/// where P0's counter is pinned and cannot say who gained. The offer names P0's life axis, never
/// P1's, and none where the counters cannot attribute the gain. The boundary asks no collapse
/// amount, and nothing is marked.
#[test]
fn a_soul_warden_shortcut_matches_performing_the_cycles() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    struct Row {
        name: &'static str,
        seats: AltarSeats,
        /// P1's life, where it is not the one `seats` gives.
        p1_life: Option<i32>,
        /// The seat whose gained counter starts pinned at `u32::MAX`.
        pinned: Option<PlayerId>,
        after_the_take: &'static [AfterTheTake],
        cycles: u32,
        unbounded: Vec<ResourceAxis>,
        landing: Landing,
        /// The `(P0, P1)` fields after.
        fields: (i32, i32),
        gained: u32,
        lost: u32,
    }
    let life = ResourceAxis::Life(P0);
    let max = i32::MAX;
    use AfterTheTake::{AncientTomb, Soulmender};
    use AltarSeats::{Duel, TwoHeadedGiant};
    let rows = [
        Row {
            name: "below the cap",
            seats: Duel(20),
            p1_life: None,
            pinned: None,
            after_the_take: &[],
            cycles: 3,
            unbounded: vec![life],
            landing: Landing::Lump(6),
            fields: (28, 20),
            gained: 8,
            lost: 0,
        },
        Row {
            name: "below the cap, then Ancient Tomb",
            seats: Duel(20),
            p1_life: None,
            pinned: None,
            after_the_take: &[AncientTomb],
            cycles: 3,
            unbounded: vec![life],
            landing: Landing::Lump(6),
            fields: (26, 20),
            gained: 8,
            lost: 2,
        },
        Row {
            name: "reaching the cap exactly, then Ancient Tomb",
            seats: Duel(max - 8),
            p1_life: None,
            pinned: None,
            after_the_take: &[AncientTomb],
            cycles: 3,
            unbounded: vec![life],
            landing: Landing::Lump(6),
            fields: (max - 2, 20),
            gained: 8,
            lost: 2,
        },
        Row {
            name: "reaching the cap exactly with the gained counter pinned",
            seats: Duel(max - 8),
            p1_life: None,
            pinned: Some(P0),
            after_the_take: &[],
            cycles: 3,
            unbounded: vec![life],
            landing: Landing::Lump(6),
            fields: (max, 20),
            gained: u32::MAX,
            lost: 0,
        },
        Row {
            name: "one past the cap, then Ancient Tomb",
            seats: Duel(max - 7),
            p1_life: None,
            pinned: None,
            after_the_take: &[AncientTomb],
            cycles: 3,
            unbounded: vec![life],
            landing: BOTH_GAINS,
            fields: (max - 2, 20),
            gained: 8,
            lost: 2,
        },
        Row {
            name: "a lump below the cap, then Soulmender carries the total to it",
            seats: Duel(max - 9),
            p1_life: None,
            pinned: None,
            after_the_take: &[Soulmender],
            cycles: 3,
            unbounded: vec![life],
            landing: Landing::Lump(6),
            fields: (max, 20),
            gained: 9,
            lost: 0,
        },
        Row {
            name: "a lump reaching the cap, then Soulmender held at it and Ancient Tomb",
            seats: Duel(max - 8),
            p1_life: None,
            pinned: None,
            after_the_take: &[Soulmender, AncientTomb],
            cycles: 3,
            unbounded: vec![life],
            landing: Landing::Lump(6),
            fields: (max - 2, 20),
            gained: 9,
            lost: 2,
        },
        Row {
            name: "a lump, then two Ancient Tombs that would be lethal without it",
            seats: Duel(1),
            p1_life: None,
            pinned: None,
            after_the_take: &[AncientTomb, AncientTomb],
            cycles: 3,
            unbounded: vec![life],
            landing: Landing::Lump(6),
            fields: (5, 20),
            gained: 8,
            lost: 4,
        },
        Row {
            name: "a bystander pinned at the cap",
            seats: Duel(20),
            p1_life: Some(max),
            pinned: Some(P1),
            after_the_take: &[],
            cycles: 3,
            unbounded: vec![life],
            landing: Landing::Lump(6),
            fields: (28, max),
            gained: 8,
            lost: 0,
        },
        Row {
            name: "Two-Headed Giant, both fields grow to the shared cap, then Ancient Tomb",
            seats: TwoHeadedGiant(max - 3, -20),
            p1_life: None,
            pinned: None,
            after_the_take: &[AncientTomb],
            cycles: 15,
            unbounded: vec![life],
            landing: BOTH_GAINS,
            fields: (max - 2, 0),
            gained: 32,
            lost: 2,
        },
        Row {
            name: "Two-Headed Giant, P0's field one below the cap, the team has room",
            seats: TwoHeadedGiant(max - 3, -20),
            p1_life: None,
            pinned: None,
            after_the_take: &[],
            cycles: 5,
            unbounded: vec![life],
            landing: BOTH_GAINS,
            fields: (max, -11),
            gained: 12,
            lost: 0,
        },
        Row {
            name:
                "Two-Headed Giant, P0's field one below the cap, the team has room, then Ancient \
                   Tomb",
            seats: TwoHeadedGiant(max - 3, -20),
            p1_life: None,
            pinned: None,
            after_the_take: &[AncientTomb],
            cycles: 5,
            unbounded: vec![life],
            landing: BOTH_GAINS,
            fields: (max - 2, -11),
            gained: 12,
            lost: 2,
        },
        Row {
            name: "Two-Headed Giant, P0's field at the cap, the team has room",
            seats: TwoHeadedGiant(max, -100),
            p1_life: None,
            pinned: None,
            after_the_take: &[],
            cycles: 5,
            unbounded: vec![life],
            landing: BOTH_GAINS,
            fields: (max, -88),
            gained: 12,
            lost: 0,
        },
        Row {
            name: "Two-Headed Giant, P0's field at the cap, the take passes the shared cap",
            seats: TwoHeadedGiant(max, -5),
            p1_life: None,
            pinned: None,
            after_the_take: &[],
            cycles: 3,
            unbounded: vec![life],
            landing: BOTH_GAINS,
            fields: (max, 0),
            gained: 8,
            lost: 0,
        },
        Row {
            name: "Two-Headed Giant, a lump that fits, short of P0's own field's cap",
            seats: TwoHeadedGiant(max - 5, -1),
            p1_life: None,
            pinned: None,
            after_the_take: &[],
            cycles: 1,
            unbounded: vec![life],
            landing: Landing::Lump(2),
            fields: (max - 1, -1),
            gained: 4,
            lost: 0,
        },
        Row {
            name: "Two-Headed Giant, the lump would spill onto P1's field, then Ancient Tomb",
            seats: TwoHeadedGiant(max - 6, -20),
            p1_life: None,
            pinned: None,
            after_the_take: &[AncientTomb],
            cycles: 5,
            unbounded: vec![life],
            landing: BOTH_GAINS,
            fields: (max - 2, -14),
            gained: 12,
            lost: 2,
        },
        Row {
            name: "Two-Headed Giant below the cap, then Ancient Tomb",
            seats: TwoHeadedGiant(20, 20),
            p1_life: None,
            pinned: None,
            after_the_take: &[AncientTomb],
            cycles: 5,
            unbounded: vec![life],
            landing: Landing::Lump(10),
            fields: (30, 20),
            gained: 12,
            lost: 2,
        },
        Row {
            name: "Two-Headed Giant below the cap, the other team's member's gained counter \
                   pinned",
            seats: TwoHeadedGiant(20, 20),
            p1_life: None,
            pinned: Some(P2),
            after_the_take: &[],
            cycles: 3,
            unbounded: vec![life],
            landing: Landing::Lump(6),
            fields: (28, 20),
            gained: 8,
            lost: 0,
        },
        Row {
            name: "Two-Headed Giant, P0's field at the cap with its gained counter pinned, the \
                   team has room",
            seats: TwoHeadedGiant(max, -100),
            p1_life: None,
            pinned: Some(P0),
            after_the_take: &[],
            cycles: 5,
            unbounded: vec![],
            landing: BOTH_GAINS,
            fields: (max, -88),
            gained: u32::MAX,
            lost: 0,
        },
    ];
    for row in rows {
        let name = row.name;
        let board = || {
            let mut board = AltarBoard::new(
                db,
                &[WARDEN, WARDEN],
                row.seats,
                0,
                |scenario: &mut GameScenario| {
                    if let Some(life) = row.p1_life {
                        scenario.with_life(P1, life);
                    }
                    add_after_the_take(scenario, db, row.after_the_take)
                },
            );
            if let Some(seat) = row.pinned {
                board.runner.state_mut().players[usize::from(seat.0)].life_gained_this_turn =
                    u32::MAX;
            }
            board.cycle_to_the_offer();
            assert_recorded_period_offer(&board.runner, &row.unbounded);
            board
        };
        let shortcut = assert_take_matches_performing(name, &board, row.cycles, row.landing);
        assert_eq!(shortcut.fields, row.fields, "{name}: the fields");
        assert_eq!(
            (shortcut.gained, shortcut.lost),
            (row.gained, row.lost),
            "{name}: P0's counters"
        );
        let p1_gained = if row.pinned == Some(P1) { u32::MAX } else { 0 };
        assert_eq!(
            shortcut.teammate_gained, p1_gained,
            "{name}: P1's gained counter"
        );
    }
}

/// CR 732.2c + CR 119.3: on P0's board of Phyrexian Altar ("Sacrifice a creature: Add one mana of
/// any color."), Gravecrawler ("This creature can't block." / "You may cast this card from your
/// graveyard as long as you control a Zombie.") and Walking Corpse (a Zombie with no rules text),
/// each cycle casts Gravecrawler and sacrifices it. Soul Warden ("Whenever another creature
/// enters, you gain 1 life.") and Carnival of Souls ("Whenever a creature enters, you lose 1 life
/// and add {B}.") make that cycle gain and lose life, in the order P0 stacks their triggers
/// (CR 603.3b). A lump carries only a net gain, so any cycle with a loss is performed at the take:
/// `Fixed(3)` leaves P0's life, gained counter, lost counter and mana where declining and
/// performing the three cycles leaves them, and Children of Korlis ("Sacrifice this creature: You
/// gain life equal to the life you've lost this turn. (Damage causes loss of life.)") then gains
/// what performing lost, so with two Wardens and Carnival P0 ends at 28. That holds with the loss
/// first or last, a loss between two gains that a later gain refills, a cycle that nets nothing
/// (where a lump has no item at all), only a loss, and a lost counter pinned at `u32::MAX`, which
/// cannot hide the loss. Control: a cycle of only gains lands in one lump at the take with the same
/// outcome, and Korlis then gains nothing (CR 119.9).
#[test]
fn a_shortcut_whose_cycle_loses_life_lands_every_counter_where_performing_leaves_it() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    struct Row {
        name: &'static str,
        payoffs: &'static [&'static str],
        lost: u32,
        offer: (i32, u32, u32),
        taken: (i32, u32, u32),
        landing: Landing,
        after_korlis: Option<(i32, u32, u32)>,
    }
    let rows = [
        Row {
            name: "two gains and a loss, the loss first",
            payoffs: &[WARDEN, WARDEN, CARNIVAL],
            lost: 0,
            offer: (21, 2, 1),
            taken: (24, 8, 4),
            landing: Landing::Performed(&[-1, 1, 1]),
            after_korlis: Some((28, 12, 4)),
        },
        Row {
            name: "a loss between two gains",
            payoffs: &[WARDEN, CARNIVAL, WARDEN],
            lost: 0,
            offer: (21, 2, 1),
            taken: (24, 8, 4),
            landing: Landing::Performed(&[1, -1, 1]),
            after_korlis: Some((28, 12, 4)),
        },
        Row {
            name: "two gains and a loss, the loss last",
            payoffs: &[CARNIVAL, WARDEN, WARDEN],
            lost: 0,
            offer: (21, 2, 1),
            taken: (24, 8, 4),
            landing: Landing::Performed(&[1, 1, -1]),
            after_korlis: Some((28, 12, 4)),
        },
        Row {
            name: "a gain and a loss that net nothing",
            payoffs: &[WARDEN, CARNIVAL],
            lost: 0,
            offer: (20, 1, 1),
            taken: (20, 4, 4),
            landing: Landing::Performed(&[-1, 1]),
            after_korlis: Some((24, 8, 4)),
        },
        Row {
            name: "only a loss",
            payoffs: &[CARNIVAL],
            lost: 0,
            offer: (19, 0, 1),
            taken: (16, 0, 4),
            landing: Landing::Performed(&[-1]),
            after_korlis: Some((20, 4, 4)),
        },
        Row {
            name: "the lost counter pinned",
            payoffs: &[WARDEN, WARDEN, CARNIVAL],
            lost: u32::MAX,
            offer: (21, 2, u32::MAX),
            taken: (24, 8, u32::MAX),
            landing: Landing::Performed(&[-1, 1, 1]),
            after_korlis: None,
        },
        Row {
            name: "control: only gains, one lump",
            payoffs: &[WARDEN, WARDEN],
            lost: 0,
            offer: (22, 2, 0),
            taken: (28, 8, 0),
            landing: Landing::Lump(6),
            after_korlis: Some((28, 8, 0)),
        },
    ];
    let add_korlis = |scenario: &mut GameScenario| {
        scenario
            .add_creature_from_oracle(P0, "Children of Korlis", 1, 1, CHILDREN_OF_KORLIS)
            .id()
    };
    for row in rows {
        let name = row.name;
        let seats = AltarSeats::Duel(20);
        let mut taken = AltarBoard::new(db, row.payoffs, seats, row.lost, add_korlis);
        taken.cycle_to_the_offer();
        assert_eq!(taken.life_reading(), row.offer, "{name}: the offer");
        let events = taken.take(3);
        assert_eq!(taken.life_reading(), row.taken, "{name}: the take");
        assert_landing(name, &events, 3, row.landing);
        assert_eq!(stash(&taken.runner), None, "{name}: nothing is stashed");

        let mut performed = AltarBoard::new(db, row.payoffs, seats, row.lost, add_korlis);
        performed.cycle_to_the_offer();
        performed.perform(3);
        assert_eq!(performed.life_reading(), row.taken, "{name}: performing");
        assert_eq!(taken.pool(), performed.pool(), "{name}: P0's mana");

        if let Some(after) = row.after_korlis {
            for board in [&mut taken, &mut performed] {
                let reader = board.reader;
                activate_and_settle(&mut board.runner, reader, 0);
                assert_eq!(
                    board.life_reading(),
                    after,
                    "{name}: after Children of Korlis"
                );
            }
        }
    }
}

/// CR 611.3a + CR 603.4 + CR 732.2c: readers of a life total and of the life gained this turn see
/// what performing leaves after a shortcut whose cycle loses life, on the board and cycle of two
/// Soul Wardens ("Whenever another creature enters, you gain 1 life.") and Carnival of Souls
/// ("Whenever a creature enters, you lose 1 life and add {B}."). Serra Ascendant ("As long as you
/// have 30 or more life, this creature gets +5/+5 and has flying.") is a 1/1 at the offer on 28 and
/// a 6/6 on 31 after `Fixed(3)`. Angelic Accord ("At the beginning of each end step, if you gained
/// 4 or more life this turn, create a 4/4 white Angel creature token with flying.") sees 4 gained
/// after `Fixed(1)` and makes one Angel, whose entry triggers both Wardens and Carnival.
#[test]
fn life_readers_see_what_performing_leaves_after_a_shortcut_that_loses_life() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    const PAYOFFS: &[&str] = &[WARDEN, WARDEN, CARNIVAL];
    // Serra Ascendant: 28 at the offer, 31 after three cycles.
    let add_serra = |scenario: &mut GameScenario| {
        scenario
            .add_creature_from_oracle(P0, "Serra Ascendant", 1, 1, SERRA_ASCENDANT)
            .id()
    };
    let serra = |board: &AltarBoard<ObjectId>| {
        let object = &board.runner.state().objects[&board.reader];
        (object.power, object.toughness)
    };
    let mut taken = AltarBoard::new(db, PAYOFFS, AltarSeats::Duel(27), 0, add_serra);
    taken.cycle_to_the_offer();
    assert_eq!(
        serra(&taken),
        (Some(1), Some(1)),
        "REACH-GUARD: below 30 at the offer"
    );
    taken.take(3);
    let mut performed = AltarBoard::new(db, PAYOFFS, AltarSeats::Duel(27), 0, add_serra);
    performed.cycle_to_the_offer();
    performed.perform(3);
    for (name, board) in [("taken", &taken), ("performed", &performed)] {
        assert_eq!(board.life_reading(), (31, 8, 4), "{name}");
        assert_eq!(
            serra(board),
            (Some(6), Some(6)),
            "{name}: Serra Ascendant at 30 or more"
        );
    }

    // Angelic Accord: 2 gained at the offer, 4 after one more cycle.
    let add_accord = |scenario: &mut GameScenario| {
        scenario
            .add_enchantment_from_oracle(P0, "Angelic Accord", ANGELIC_ACCORD)
            .id()
    };
    let mut taken = AltarBoard::new(db, PAYOFFS, AltarSeats::Duel(20), 0, add_accord);
    taken.cycle_to_the_offer();
    taken.take(1);
    let mut performed = AltarBoard::new(db, PAYOFFS, AltarSeats::Duel(20), 0, add_accord);
    performed.cycle_to_the_offer();
    performed.perform(1);
    for (name, board) in [("taken", &mut taken), ("performed", &mut performed)] {
        assert_eq!(
            board.life_reading(),
            (22, 4, 2),
            "{name}: before the end step"
        );
        settle_end_step(&mut board.runner);
        let state = board.runner.state();
        let angels = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token && object.name == "Angel")
            .count();
        assert_eq!(angels, 1, "{name}: 4 gained makes one Angel");
        assert_eq!(
            board.life_reading(),
            (23, 6, 3),
            "{name}: the Angel's entry"
        );
    }
}

/// CR 119.3 + CR 704.5a + CR 732.2c: on P0's board of Phyrexian Altar ("Sacrifice a creature: Add
/// one mana of any color."), Gravecrawler ("This creature can't block." / "You may cast this card
/// from your graveyard as long as you control a Zombie.") and Walking Corpse (a Zombie with no
/// rules text), Platinum Angel ("You can't lose the game and your opponents can't win the game.")
/// keeps a seat in the game while a cycle takes its total down to exactly `i32::MIN`. Zulaport
/// Cutthroat ("Whenever this creature or another creature you control dies, each opponent loses 1
/// life and you gain 1 life.") drains P1, who did not propose, and the offer names P0's life axis.
/// Carnival of Souls ("Whenever a creature enters, you lose 1 life and add {B}.") drains P0, and
/// the offer names only its `Mana(Black)` axis, with no `Life` item to batch. From `i32::MIN + 2`
/// the cycle that offers the shortcut leaves the seat at `i32::MIN + 1`. A `Fixed(1)` take's cycle
/// reaches `i32::MIN`, and a loss is not a gain a lump carries, so the take performs the cycle
/// whether the seat's lost counter is readable or pinned at `u32::MAX`: nothing is stashed or
/// marked, the boundary asks nothing, the accept reports the seat's loss at `i32::MIN`, and the
/// seat's life and lost counter and P0's life and gained counter end where declining and performing
/// the cycle leaves them.
#[test]
fn a_shortcut_losing_a_total_down_to_i32_min_is_performed_whatever_its_lost_counter() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    struct Row {
        name: &'static str,
        payoff: &'static str,
        /// The seat the cycle drains, whose Platinum Angel keeps it in the game.
        drained: PlayerId,
        unbounded: ResourceAxis,
        /// The `LifeChanged` events of the take's cycle.
        changes: Vec<(PlayerId, i32, Option<i32>)>,
        /// P0's life and gained counter after.
        p0: (i32, u32),
    }
    let min = i32::MIN;
    let rows = [
        Row {
            name: "Zulaport Cutthroat drains P1",
            payoff: "Zulaport Cutthroat",
            drained: P1,
            unbounded: ResourceAxis::Life(P0),
            changes: vec![(P1, -1, Some(min)), (P0, 1, Some(22))],
            p0: (22, 2),
        },
        Row {
            name: "Carnival of Souls drains P0",
            payoff: CARNIVAL,
            drained: P0,
            unbounded: ResourceAxis::Mana(ManaType::Black),
            changes: vec![(P0, -1, Some(min))],
            p0: (min, 0),
        },
    ];
    for row in rows {
        let seat = usize::from(row.drained.0);
        let board = |lost: u32| {
            let mut board = AltarBoard::new(
                db,
                &[row.payoff],
                AltarSeats::Duel(20),
                0,
                |scenario: &mut GameScenario| {
                    scenario.add_real_card(row.drained, "Platinum Angel", Zone::Battlefield, db);
                    scenario.with_life(row.drained, min + 2);
                },
            );
            board.runner.state_mut().players[seat].life_lost_this_turn = lost;
            board.cycle_to_the_offer();
            assert_recorded_period_offer(&board.runner, &[row.unbounded]);
            assert_eq!(
                board.runner.life(row.drained),
                min + 1,
                "REACH-GUARD {}: one cycle drains 1",
                row.name
            );
            assert!(
                !board.runner.state().players[seat].is_eliminated,
                "REACH-GUARD {}: Platinum Angel keeps the seat in the game",
                row.name
            );
            board
        };
        // The drained seat's life and lost counter, P0's life and gained counter.
        let reading = |board: &AltarBoard<()>| {
            let players = &board.runner.state().players;
            (
                players[seat].life,
                players[seat].life_lost_this_turn,
                players[0].life,
                players[0].life_gained_this_turn,
            )
        };
        // (the seat's lost counter at the start, after)
        for (lost, lost_after) in [(0, 2), (u32::MAX, u32::MAX)] {
            let name = format!("{}, lost counter {lost}", row.name);
            let mut taken = board(lost);
            let events = taken.take(1);
            assert_eq!(
                life_changes(&events),
                row.changes,
                "{name}: the take performs the cycle"
            );
            assert_eq!(
                markers(&events),
                vec![(P0, 1)],
                "{name}: the record keeps the cycle"
            );
            assert_eq!(stash(&taken.runner), None, "{name}: nothing is stashed");
            assert_eq!(
                taken.runner.state().unbounded_resources.get(&P0),
                None,
                "{name}: a performed take marks nothing"
            );
            pass_to_a_boundary_without_a_collapse(&mut taken.runner);

            let mut performed = board(lost);
            performed.perform(1);
            pass_to_a_boundary_without_a_collapse(&mut performed.runner);

            assert_eq!(reading(&taken), reading(&performed), "{name}");
            assert_eq!(
                reading(&taken),
                (min, lost_after, row.p0.0, row.p0.1),
                "{name}"
            );
        }
    }
}

/// Moonlit Wake's verbatim Oracle text (not a member of the test fixture).
const MOONLIT_WAKE: &str = "Whenever a creature dies, you gain 1 life.";

/// Gavony Unhallowed's verbatim Oracle text (not a member of the test fixture).
const GAVONY_UNHALLOWED: &str =
    "Whenever another creature you control dies, put a +1/+1 counter on this creature.";

/// Golgari Germination's verbatim Oracle text (not a member of the test fixture).
const GOLGARI_GERMINATION: &str =
    "Whenever a nontoken creature you control dies, create a 1/1 green Saproling creature token.";

/// CR 732.2c + CR 119.3: a marked take lands its batched life at the take, at the accepted count,
/// but its counters and tokens at the CR 500.5 boundary, at the count its controller names there,
/// so a take that batches life beside either is performed instead. Moonlit Wake ("Whenever a
/// creature dies, you gain 1 life.") gains 1 each cycle. Gavony Unhallowed ("Whenever another
/// creature you control dies, put a +1/+1 counter on this creature.") grows a counter beside it, on
/// [`AltarBoard::reassembling_skeleton`], since the engine reads Gravecrawler's "as long as you
/// control a Zombie" as observing that growth, which would perform the take for that reason alone.
/// Golgari Germination ("Whenever a nontoken creature you control dies, create a 1/1 green
/// Saproling creature token.") creates a token beside it on P0's board of Phyrexian Altar,
/// Gravecrawler and Walking Corpse, with no battlefield-entry trigger on the board. A `Fixed(3)`
/// take performs all three cycles at the take: a record keeping the first and last cycle (CR
/// 732.1b), each with its `LifeChanged`, real counters or tokens at the take, nothing stashed or
/// marked, and no boundary prompt. P0's life, gained counter, +1/+1 counters and Saprolings then
/// end where declining and performing the cycles leaves them.
#[test]
fn a_life_shortcut_that_also_grows_tokens_or_counters_is_performed_at_the_take() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let life = ResourceAxis::Life(P0);
    let on_the_skeleton_board = || {
        AltarBoard::reassembling_skeleton(db, |scenario: &mut GameScenario| {
            scenario.add_enchantment_from_oracle(P0, "Moonlit Wake", MOONLIT_WAKE);
            scenario.add_creature_from_oracle(P0, "Gavony Unhallowed", 2, 4, GAVONY_UNHALLOWED);
        })
    };
    let on_the_gravecrawler_board = || {
        AltarBoard::new(
            db,
            &[],
            AltarSeats::Duel(20),
            0,
            |scenario: &mut GameScenario| {
                scenario.add_enchantment_from_oracle(P0, "Moonlit Wake", MOONLIT_WAKE);
                scenario.add_enchantment_from_oracle(
                    P0,
                    "Golgari Germination",
                    GOLGARI_GERMINATION,
                );
            },
        )
    };
    // (name, the board, the offer's axes, P0's +1/+1 counters and Saprolings after the take)
    type Row<'a> = (
        &'static str,
        &'a dyn Fn() -> AltarBoard<()>,
        Vec<ResourceAxis>,
        (u32, usize),
    );
    let rows: [Row; 2] = [
        (
            "Gavony Unhallowed's +1/+1 counter",
            &on_the_skeleton_board,
            vec![
                ResourceAxis::Mana(ManaType::Colorless),
                life,
                ResourceAxis::Counter(CounterClass::Plus1Plus1, ObjectClass::Creature),
            ],
            (4, 0),
        ),
        (
            "Golgari Germination's Saproling, no ETB trigger",
            &on_the_gravecrawler_board,
            vec![life, ResourceAxis::TokensCreated],
            (0, 4),
        ),
    ];
    // P0's life, P0's gained counter, Gavony Unhallowed's +1/+1 counters, P0's Saprolings.
    let reading = |runner: &GameRunner| {
        let state = runner.state();
        let on_the_battlefield = || {
            state
                .battlefield
                .iter()
                .filter_map(|id| state.objects.get(id))
        };
        (
            runner.life(P0),
            state.players[0].life_gained_this_turn,
            on_the_battlefield()
                .filter(|object| object.name == "Gavony Unhallowed")
                .filter_map(|object| object.counters.get(&CounterType::Plus1Plus1))
                .copied()
                .sum::<u32>(),
            on_the_battlefield()
                .filter(|object| object.is_token && object.name == "Saproling")
                .count(),
        )
    };
    for (name, board, unbounded, growth_after) in rows {
        let offered = || {
            let mut board = board();
            board.cycle_to_the_offer();
            assert_recorded_period_offer(&board.runner, &unbounded);
            board
        };

        // The shortcut.
        let mut taken = offered();
        let events = taken.take(3);
        assert_eq!(
            markers(&events),
            vec![(P0, 1), (P0, 3)],
            "{name}: the record keeps the first and last cycle"
        );
        assert_eq!(
            repetitions(&events)
                .into_iter()
                .map(|cycle| life_changes(cycle)
                    .into_iter()
                    .map(|(player, amount, _)| (player, amount))
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec![(P0, 1)]; 2],
            "{name}: each kept cycle's LifeChanged"
        );
        assert_eq!(stash(&taken.runner), None, "{name}: nothing is stashed");
        assert_eq!(
            taken.runner.state().unbounded_resources.get(&P0),
            None,
            "{name}: a performed take marks nothing"
        );
        assert_eq!(
            reading(&taken.runner),
            (24, 4, growth_after.0, growth_after.1),
            "{name}: every cycle's growth lands at the take"
        );
        pass_to_a_boundary_without_a_collapse(&mut taken.runner);
        let shortcut = reading(&taken.runner);

        // The reference: decline, perform three cycles.
        let mut performed = offered();
        performed.perform(3);
        pass_to_a_boundary_without_a_collapse(&mut performed.runner);

        assert_eq!(shortcut, reading(&performed.runner), "{name}");
    }
}

/// CR 104.4b + CR 732.5 + CR 732.2c: a loop of mandatory triggers that another player can change
/// only with an out is offered as a draw without end, at the cap as below it, and a counted take
/// performs it. [`dragon_loop`]'s Worldgorger Dragon and Animate Dead repeat with no player choice,
/// and P1's Soul Warden ("Whenever another creature enters, you gain 1 life.") gains 1 each time
/// the Dragon enters, while P0's opponent (P2 in Two-Headed Giant) holds Lightning Bolt ("Lightning
/// Bolt deals 3 damage to any target.") and a Mountain. Bolt cannot kill the Dragon, but it can
/// kill the Warden, and with that creature card in a graveyard P0 chooses what Animate Dead
/// enchants as it returns (CR 303.4f), so P0 can end the loop and it is not one with no way to
/// stop. How many gains have resolved at the offer is where the detection first sees the
/// recurrence, which the cards do not fix. In a duel at 20 and at `i32::MAX`, for a
/// Two-Headed Giant team whose shared total is at the cap (CR 810.4), and with P1's own field at
/// the cap so each gain is stored on P0's field (CR 810.9), the first recurrence offers P0 a draw
/// without end with no winner and no unbounded or rising axis. A `Fixed(3)` take performs three
/// cycles, each gaining 1 (CR 119.3), and its record keeps the first and last (CR 732.1b); it marks
/// nothing and leaves P0 with priority. The rows pin that the loop is offered at the cap as below
/// it.
#[test]
fn an_out_only_life_gain_loop_is_offered_at_the_cap_as_below_it() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    struct Row {
        name: &'static str,
        two_headed_giant: bool,
        /// Team A's stored fields at the start, `(P0, P1)`.
        fields: (i32, i32),
        /// Team A's stored fields and P1's gained counter at the offer and after the take.
        at_offer: ((i32, i32), u32),
        after_take: ((i32, i32), u32),
    }
    let max = i32::MAX;
    let rows = [
        Row {
            name: "control: a duel at 20",
            two_headed_giant: false,
            fields: (20, 20),
            at_offer: ((20, 21), 1),
            after_take: ((20, 24), 4),
        },
        Row {
            name: "a duel at the cap",
            two_headed_giant: false,
            fields: (20, max),
            at_offer: ((20, max), 2),
            after_take: ((20, max), 5),
        },
        Row {
            name: "Two-Headed Giant team at the cap",
            two_headed_giant: true,
            fields: (5, max - 5),
            at_offer: ((5, max - 5), 2),
            after_take: ((5, max - 5), 5),
        },
        Row {
            name: "Two-Headed Giant, P1's field at the cap, the gains spill to P0",
            two_headed_giant: true,
            fields: (-100, max),
            at_offer: ((-99, max), 1),
            after_take: ((-96, max), 4),
        },
    ];
    for row in rows {
        let name = row.name;
        let (mut scenario, out_holder, responders) = if row.two_headed_giant {
            (two_headed_giant(), P2, vec![P1, P2, PlayerId(3)])
        } else {
            (two_player(), P1, vec![P1])
        };
        scenario.with_life(P0, row.fields.0);
        scenario.with_life(P1, row.fields.1);
        let mut runner = dragon_loop(
            db,
            scenario,
            LoopDetectionMode::Interactive,
            |scenario: &mut GameScenario| {
                scenario.add_real_card(P1, WARDEN, Zone::Battlefield, db);
                scenario.add_real_card(out_holder, "Mountain", Zone::Battlefield, db);
                scenario.add_real_card(out_holder, "Lightning Bolt", Zone::Hand, db);
            },
        );
        drive_mandatory_loop(&mut runner, 60, Some(WARDEN));

        let WaitingFor::LoopShortcut {
            proposer,
            predicted_winner,
            certificate,
            schema,
            road,
            counted_take,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "{name}: the first recurrence offers the loop, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*proposer, P0, "{name}");
        assert_eq!(*predicted_winner, None, "{name}: no winner");
        assert!(
            certificate.unbounded.is_empty() && certificate.rising.is_empty(),
            "{name}: no unbounded or rising axis, got {:?} / {:?}",
            certificate.unbounded,
            certificate.rising
        );
        assert_eq!(
            schema.iteration_count,
            IterationCount::WithoutEnd,
            "{name}: a draw without end"
        );
        assert_eq!(*road, OfferRoad::Ring, "{name}");
        assert_eq!(*counted_take, CountedTake::Performs, "{name}");
        assert_eq!(
            (
                team_a_fields(runner.state()),
                runner.state().players[1].life_gained_this_turn
            ),
            row.at_offer,
            "{name}: the fields and P1's gained counter at the offer"
        );

        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(3),
                template: None,
            })
            .expect("the shortcut declaration is accepted");
        let events = accept_for_every_responder(&mut runner, &responders);

        let state = runner.state();
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
            "{name}: the take hands P0 priority, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            markers(&events),
            vec![(P0, 1), (P0, 3)],
            "{name}: the record keeps the first and last cycle"
        );
        assert_eq!(
            repetitions(&events)
                .into_iter()
                .map(|cycle| life_changes(cycle)
                    .into_iter()
                    .map(|(player, amount, _)| (player, amount))
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec![(P1, 1)]; 2],
            "{name}: each kept cycle gains 1"
        );
        assert_eq!(
            (team_a_fields(state), state.players[1].life_gained_this_turn),
            row.after_take,
            "{name}: the fields and P1's gained counter after the take"
        );
        assert!(
            state.unbounded_resources.is_empty(),
            "{name}: the take marks nothing"
        );
        assert!(
            derive_views(state, None).life_magnitude.is_empty(),
            "{name}: no readout claims ∞"
        );
    }
}

/// What [`drive_mandatory_loop`] answered and saw.
struct MandatoryDrive {
    /// How many target prompts were answered.
    target_prompts: usize,
    /// Every driven action's events.
    events: Vec<GameEvent>,
}

/// Passes priority and answers `runner`'s prompts for at most `beats` actions, until the game ends,
/// a loop shortcut is offered or the stack empties. A trigger-ordering prompt puts the trigger of
/// the card named `resolves_first` on the stack last, so it resolves first. A target prompt names
/// P0.
fn drive_mandatory_loop(
    runner: &mut GameRunner,
    beats: usize,
    resolves_first: Option<&str>,
) -> MandatoryDrive {
    let mut drive = MandatoryDrive {
        target_prompts: 0,
        events: Vec::new(),
    };
    for _ in 0..beats {
        let state = runner.state();
        let action = match &state.waiting_for {
            WaitingFor::GameOver { .. } | WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::Priority { .. } if state.stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            // The trigger listed last goes on the stack last and resolves first.
            WaitingFor::OrderTriggers { triggers, .. } => {
                let mut order: Vec<usize> = (0..triggers.len()).collect();
                order.sort_by_key(|&index| {
                    state
                        .objects
                        .get(&triggers[index].source_id)
                        .is_some_and(|object| Some(object.name.as_str()) == resolves_first)
                });
                GameAction::OrderTriggers { order }
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                drive.target_prompts += 1;
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P0)),
                }
            }
            other => panic!("unexpected prompt while driving the loop: {other:?}"),
        };
        drive.events.extend(
            runner
                .act(action)
                .expect("every driven action is legal")
                .events,
        );
    }
    drive
}

/// CR 104.4b + CR 810.9 + CR 810.9a: P0's Worldgorger Dragon ("Flying, trample" / "When this
/// creature enters, exile all other permanents you control." / "When this creature leaves the
/// battlefield, return the exiled cards to the battlefield under their owners' control.") and
/// Animate Dead ("Enchant creature card in a graveyard" / "When this Aura enters, if it's on the
/// battlefield, it loses "enchant creature card in a graveyard" and gains "enchant creature put
/// onto the battlefield with this Aura." Return enchanted creature card to the battlefield under
/// your control and attach this Aura to it. When this Aura leaves the battlefield, that creature's
/// controller sacrifices it." / "Enchanted creature gets -1/-0.") repeat with no player choice, and
/// each cycle the Dragon dies. P1's Blood Artist ("Whenever this creature or another creature dies,
/// target player loses 1 life and you gain 1 life.") then makes a player lose what P1 gains. With
/// Ivory Mask ("You have shroud. (You can't be the target of spells or abilities.)") under P1 and
/// Leyline of Sanctity ("If this card is in your opening hand, you may begin the game with it on
/// the battlefield." / "You have hexproof. (You can't be the target of spells or abilities your
/// opponents control.)") under P2 and P3, P0 is its one legal target, so the loop holds only
/// mandatory actions. P0 and P1 share a total at `i32::MAX`, with P1's own field at the cap: P0's
/// loss lands on P0's field, and P1's gain, which P1's own field cannot hold, spills back onto
/// P0's, so the total and both fields end each cycle where they began, and only the per-turn
/// counters move.
/// The loop is a draw whichever way the team orders its triggers (CR 805.7): with Blood Artist's
/// first, P0 has lost and P1 has gained life by then; with the Dragon's first, Blood Artist's
/// trigger stays on the stack and nobody has. From `(1, i32::MAX - 1)` the first cycle moves the
/// fields to that shape and the loop is drawn after it. With Lightning Bolt ("Lightning Bolt deals
/// 3 damage to any target.") and a Mountain under P2, which can kill Blood Artist so that P0
/// chooses what Animate Dead enchants as it returns (CR 303.4f), the loop is offered as a draw
/// without end (CR 732.5), and a `Fixed(3)` take performs three cycles, its record keeping the
/// first and last (CR 732.1b). Control: with no Ivory Mask or Leylines, P1 chooses a target each
/// cycle and could drain the other team, so the loop holds a choice and is neither drawn nor
/// offered.
#[test]
fn a_mandatory_loop_that_leaves_a_team_at_the_cap_where_it_stood_is_drawn() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let p3 = PlayerId(3);
    let max = i32::MAX;
    let board = |fields: (i32, i32), one_legal_target: bool, out: bool| {
        let mut scenario = two_headed_giant();
        for (player, life) in [(P0, fields.0), (P1, fields.1), (P2, 20), (p3, 20)] {
            scenario.with_life(player, life);
        }
        dragon_loop(
            db,
            scenario,
            LoopDetectionMode::Interactive,
            |scenario: &mut GameScenario| {
                scenario.add_real_card(P1, "Blood Artist", Zone::Battlefield, db);
                if one_legal_target {
                    scenario.add_real_card(P1, "Ivory Mask", Zone::Battlefield, db);
                    scenario.add_real_card(P2, "Leyline of Sanctity", Zone::Battlefield, db);
                    scenario.add_real_card(p3, "Leyline of Sanctity", Zone::Battlefield, db);
                }
                if out {
                    scenario.add_real_card(P2, "Mountain", Zone::Battlefield, db);
                    scenario.add_real_card(P2, "Lightning Bolt", Zone::Hand, db);
                }
            },
        )
    };
    // P0's lost counter and P1's gained counter.
    let moved = |runner: &GameRunner| {
        let players = &runner.state().players;
        (
            players[0].life_lost_this_turn,
            players[1].life_gained_this_turn,
        )
    };
    let is_draw = |runner: &GameRunner| {
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: None }
        )
    };
    let at_the_cap = |runner: &GameRunner, name: &str| {
        assert_eq!(
            team_a_fields(runner.state()),
            (0, max),
            "{name}: the fields"
        );
        assert_eq!(
            team_life_sum(runner.state(), P0),
            i64::from(max),
            "{name}: the total"
        );
    };

    // Drawn, with Blood Artist's trigger resolving each cycle.
    for (name, fields, cycles_before_the_shape) in [
        ("the fields already in the shape", (0, max), 0),
        ("one cycle moves the fields to the shape", (1, max - 1), 1),
    ] {
        let mut runner = board(fields, true, false);
        let drive = drive_mandatory_loop(&mut runner, 60, Some("Blood Artist"));
        assert_eq!(
            drive.target_prompts, 0,
            "REACH-GUARD {name}: P0 is the one legal target"
        );
        assert!(
            is_draw(&runner),
            "{name}: drawn, got {:?}",
            runner.state().waiting_for
        );
        let (lost, gained) = moved(&runner);
        assert!(
            lost > cycles_before_the_shape && gained > cycles_before_the_shape,
            "REACH-GUARD {name}: Blood Artist's trigger resolved, got {lost} and {gained}"
        );
        at_the_cap(&runner, name);
    }

    // Drawn, with Blood Artist's trigger staying on the stack.
    let name = "the Dragon's trigger first";
    let mut runner = board((0, max), true, false);
    drive_mandatory_loop(&mut runner, 60, Some("Worldgorger Dragon"));
    assert!(
        is_draw(&runner),
        "{name}: drawn, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(moved(&runner), (0, 0), "{name}: no life moved");
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            engine::types::game_state::StackEntryKind::TriggeredAbility { source_name, .. }
                if source_name == "Blood Artist"
        )),
        "REACH-GUARD {name}: Blood Artist's trigger is on the stack"
    );
    at_the_cap(&runner, name);

    // Offered as a draw without end.
    let name = "an opponent holds an out";
    let mut runner = board((0, max), true, true);
    drive_mandatory_loop(&mut runner, 60, Some("Blood Artist"));
    let WaitingFor::LoopShortcut {
        predicted_winner,
        schema,
        road,
        counted_take,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("{name}: offered, got {:?}", runner.state().waiting_for);
    };
    assert_eq!(*predicted_winner, None, "{name}");
    assert_eq!(schema.iteration_count, IterationCount::WithoutEnd, "{name}");
    assert_eq!(*road, OfferRoad::Ring, "{name}");
    assert_eq!(*counted_take, CountedTake::Performs, "{name}");
    let at_the_offer = moved(&runner);
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: None,
        })
        .expect("the shortcut declaration is accepted");
    let events = accept_for_every_responder(&mut runner, &[P1, P2, p3]);
    assert_eq!(
        markers(&events),
        vec![(P0, 1), (P0, 3)],
        "{name}: the record keeps the first and last cycle"
    );
    assert_eq!(
        moved(&runner),
        (at_the_offer.0 + 3, at_the_offer.1 + 3),
        "{name}: every performed cycle counts"
    );
    at_the_cap(&runner, name);

    // Control: a target chosen among all four players.
    let name = "P1 chooses among four targets";
    let mut runner = board((0, max), false, false);
    let target_prompts = drive_mandatory_loop(&mut runner, 40, Some("Blood Artist")).target_prompts;
    assert!(
        target_prompts >= 2,
        "REACH-GUARD {name}: P1 chose a target each cycle, got {target_prompts}"
    );
    assert!(
        moved(&runner).0 >= 2,
        "REACH-GUARD {name}: the loop kept running, got {:?}",
        moved(&runner)
    );
    assert!(
        is_priority(runner.state()),
        "{name}: neither drawn nor offered, got {:?}",
        runner.state().waiting_for
    );
    at_the_cap(&runner, name);
}

/// CR 104.4b + CR 732.5 + CR 732.2c: P0's Exquisite Blood and Bloodthirsty Conqueror (each
/// "Whenever an opponent loses life, you gain that much life.") and Marauding Blight-Priest
/// ("Whenever you gain life, each opponent loses 1 life.") feed each other after Blessed Wine's
/// gain, P1 starts at `i32::MIN`, where the limit holds its total, Platinum Angel ("You can't lose
/// the game and your opponents can't win the game.") keeps P1 in the game (CR 704.5a), and each
/// loss makes two gains, so the triggers on the stack grow every
/// cycle. The loop has no way to stop and is a draw, at `i32::MAX` as at 20; the cards do not fix
/// how many triggers have resolved when it is called, which is where the detection first sees the
/// recurrence. With Lightning Bolt
/// ("Lightning Bolt deals 3 damage to any target.") and a Mountain under P1 it is offered as a draw
/// without end, and a `Fixed(3)` take performs three cycles, each counted (CR 119.3).
#[test]
fn a_growing_life_cascade_is_drawn_or_offered_at_the_cap_as_below_it() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    // (P0's life at the start, P0's life at the draw, P0's life after the take)
    for (start, at_the_draw, after_the_take) in [(20, 22, 25), (i32::MAX, i32::MAX, i32::MAX)] {
        let board = |out: bool| {
            let mut scenario = two_player();
            scenario.with_life(P0, start);
            scenario.with_life(P1, i32::MIN);
            scenario.add_real_card(P1, "Platinum Angel", Zone::Battlefield, db);
            scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, db);
            scenario.add_real_card(P0, "Bloodthirsty Conqueror", Zone::Battlefield, db);
            scenario.add_real_card(P0, "Marauding Blight-Priest", Zone::Battlefield, db);
            if out {
                scenario.add_real_card(P1, "Mountain", Zone::Battlefield, db);
                scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
            }
            let kickoff = scenario
                .add_spell_to_hand_from_oracle(P0, "Blessed Wine", true, BLESSED_WINE)
                .id();
            let mut runner = scenario.build();
            runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
            drop(runner.cast(kickoff).commit());
            runner
        };
        // P0's life, P0's gained counter, P1's lost counter.
        let reading = |runner: &GameRunner| {
            let players = &runner.state().players;
            (
                players[0].life,
                players[0].life_gained_this_turn,
                players[1].life_lost_this_turn,
            )
        };

        let mut runner = board(false);
        drive_mandatory_loop(&mut runner, 60, None);
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::GameOver { winner: None }
            ),
            "from {start}: drawn, got {:?}",
            runner.state().waiting_for
        );
        assert_eq!(reading(&runner), (at_the_draw, 2, 1), "from {start}");
        assert_eq!(runner.life(P1), i32::MIN, "from {start}");

        let mut runner = board(true);
        drive_mandatory_loop(&mut runner, 60, None);
        let WaitingFor::LoopShortcut {
            schema,
            road,
            counted_take,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "from {start}: offered, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(
            schema.iteration_count,
            IterationCount::WithoutEnd,
            "from {start}"
        );
        assert_eq!(*road, OfferRoad::Ring, "from {start}");
        assert_eq!(*counted_take, CountedTake::Performs, "from {start}");
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(3),
                template: None,
            })
            .expect("the shortcut declaration is accepted");
        accept_for_every_responder(&mut runner, &[P1]);
        assert_eq!(reading(&runner), (after_the_take, 5, 4), "from {start}");
        assert_eq!(
            runner.state().stack.len(),
            5,
            "from {start}: the triggers keep growing"
        );
    }
}
