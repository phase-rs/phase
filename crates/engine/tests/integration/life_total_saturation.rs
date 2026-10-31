//! CR 119.3 + CR 107.1b: a life total stops at the `i32` limit a gain or loss
//! moves it toward, and the `LifeChanged` event and the per-turn gained/lost
//! counters report the rules amount, even when the stored total moved less.
//! Energy and player counters keep refusing a total past `u32::MAX`. A life
//! amount past `i32::MAX` is carried exactly, set/exchange/redistribute land
//! exactly across the whole range, and in Two-Headed Giant a change caps the
//! team's shared total (CR 810.4 + CR 810.9). A life replacement quantity past
//! `u32` lands on `u32::MAX`. A mandatory life-gain loop at the cap is drawn
//! (CR 104.4b + CR 732.4), as is a mandatory loop that drains a seat held at
//! `i32::MIN`; an optional one is never drawn or offered a shortcut. A
//! shortcut's batched life lands at the take, and a shortcut whose cycles' life
//! a lump would not store as performing stores it (a change an `i32` limit
//! holds back, a loss, a change stored on a teammate, or life beside batched
//! tokens or counters) is taken by performing its cycles, so its counters,
//! events and total end as performing them leaves them, even after a later
//! life change (CR 732.2c).

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::analysis::resource::{CounterClass, ObjectClass, ResourceAxis};
use engine::game::mana_abilities::is_mana_ability;
use engine::game::players::team_life_sum;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityDefinition, Effect, EffectKind, QuantityModification, ReplacementDefinition,
    TapStateChange, TargetRef,
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

/// Luminescent Rain's verbatim Oracle text (not a member of the test fixture).
const LUMINESCENT_RAIN: &str =
    "Choose a creature type. You gain 2 life for each permanent you control of that type.";

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(9_999), false, Vec::new())
}

/// Every `LifeChanged` as `(player, amount, new total)`, in order.
fn life_changes(events: &[GameEvent]) -> Vec<(PlayerId, i32, Option<i32>)> {
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
/// same.
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

/// A self-feeding life-gain trigger (synthetic text, as in
/// `loop_shortcut.rs`): with one copy every gain triggers another, so the loop
/// is mandatory and never stops.
const SELF_LIFE_ENGINE: &str = "Whenever you gain life, you gain 1 life.";

/// The synthetic kickoff gain that starts a life-gain loop.
const KICKOFF: &str = "You gain 1 life.";

/// A synthetic drain trigger: each gain makes every opponent lose 1 life.
const DRAIN_ENGINE: &str = "Whenever you gain life, each opponent loses 1 life.";

/// A synthetic free life-gain engine (as in `combo_infinite_pile.rs`): tap for
/// {C}, gain `n` life, untap, repeat. The mana and the life are its unbounded
/// axes.
fn lifedynamo(n: u32) -> String {
    format!("{{T}}: Add {{C}}.\n{{0}}: You gain {n} life.\n{{0}}: Untap Lifedynamo.")
}

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

/// Declines the shortcut offer and performs `cycles` more dynamo cycles (the
/// `indices` activations each), declining any offer that follows; returns every
/// event.
fn decline_and_perform(
    runner: &mut GameRunner,
    dynamo: ObjectId,
    indices: &[usize],
    cycles: u32,
) -> Vec<GameEvent> {
    /// Declines every loop shortcut offered (at most four in a row), settling
    /// after each; returns every event.
    fn decline_offers(runner: &mut GameRunner) -> Vec<GameEvent> {
        let mut events = Vec::new();
        for _ in 0..4 {
            if !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }) {
                return events;
            }
            events.extend(
                runner
                    .act(GameAction::DeclineShortcut)
                    .expect("declining the shortcut is accepted")
                    .events,
            );
            events.extend(settle(runner));
        }
        assert!(
            !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
            "a loop shortcut is still offered after four declines"
        );
        events
    }

    let mut events = decline_offers(runner);
    for _ in 0..cycles {
        for &index in indices {
            events.extend(activate_and_settle(runner, dynamo, index));
            events.extend(decline_offers(runner));
        }
    }
    assert!(
        is_priority(runner.state()) && runner.state().stack.is_empty(),
        "the performed cycles end at priority with an empty stack, got {:?}",
        runner.state().waiting_for
    );
    events
}

/// The life state a shortcut or the performed cycles leave: P0's gained and lost
/// counters, P1's gained counter, team A's stored fields (`(P0, P1)`), P0's
/// shared total, and the sum and last new total of P0's `LifeChanged` events.
#[derive(Debug, PartialEq, Eq)]
struct LifeOutcome {
    gained: u32,
    lost: u32,
    teammate_gained: u32,
    fields: (i32, i32),
    team: i64,
    life_changed_total: i64,
    last_new_total: Option<i32>,
}

impl LifeOutcome {
    fn read(runner: &GameRunner, events: &[GameEvent]) -> Self {
        let p0_changes: Vec<(i32, Option<i32>)> = life_changes(events)
            .into_iter()
            .filter(|(player, _, _)| *player == P0)
            .map(|(_, amount, new_total)| (amount, new_total))
            .collect();
        LifeOutcome {
            gained: runner.state().players[0].life_gained_this_turn,
            lost: runner.state().players[0].life_lost_this_turn,
            teammate_gained: runner.state().players[1].life_gained_this_turn,
            fields: team_a_fields(runner.state()),
            team: team_life_sum(runner.state(), P0),
            life_changed_total: p0_changes
                .iter()
                .map(|(amount, _)| i64::from(*amount))
                .sum(),
            last_new_total: p0_changes.last().and_then(|(_, new_total)| *new_total),
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

/// A mandatory life-gain loop in Interactive loop detection: P0 controls one
/// `SELF_LIFE_ENGINE` creature and holds the `KICKOFF` spell, with the given
/// stored life fields and P0's gained-this-turn counter preset to `gained`.
fn mandatory_life_gain_loop(
    mut scenario: GameScenario,
    life_fields: &[(PlayerId, i32)],
    gained: u32,
) -> (GameRunner, ObjectId) {
    for &(player, life) in life_fields {
        scenario.with_life(player, life);
    }
    scenario.add_creature_from_oracle(P0, "Test Life Engine", 2, 2, SELF_LIFE_ENGINE);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state_mut().players[0].life_gained_this_turn = gained;
    (runner, kickoff)
}

/// A synthetic free engine of P0's (as in `combo_infinite_pile.rs`): tap for
/// {C}, change life, untap, repeat. The mana, and any life it gains, are its
/// unbounded axes.
#[derive(Clone, Copy, Debug)]
enum Dynamo {
    /// `lifedynamo(n)`: P0 gains `n` life each cycle.
    Gain(u32),
    /// P0 gains `gain` life, then loses `loss` life, each cycle.
    GainThenLose { gain: u32, loss: u32 },
    /// P0 loses `loss` life, then gains `gain` life, each cycle.
    LoseThenGain { loss: u32, gain: u32 },
    /// Each of P0's opponents loses `n` life each cycle.
    Drain(u32),
}

impl Dynamo {
    /// The dynamo's synthetic text: the two orders of a gain and a loss share
    /// one text and differ only in the order its abilities are activated.
    fn text(self) -> String {
        match self {
            Dynamo::Gain(n) => lifedynamo(n),
            Dynamo::GainThenLose { gain, loss } | Dynamo::LoseThenGain { loss, gain } => format!(
                "{{T}}: Add {{C}}.\n{{0}}: You gain {gain} life.\n{{0}}: You lose {loss} life.\n\
                 {{0}}: Untap Lifedynamo."
            ),
            Dynamo::Drain(n) => format!(
                "{{T}}: Add {{C}}.\n{{0}}: Each opponent loses {n} life.\n{{0}}: Untap Lifedynamo."
            ),
        }
    }

    /// The `LifeChanged` events P0's own life reports each cycle.
    fn p0_changes_per_cycle(self) -> usize {
        match self {
            Dynamo::Gain(_) => 1,
            Dynamo::GainThenLose { .. } | Dynamo::LoseThenGain { .. } => 2,
            Dynamo::Drain(_) => 0,
        }
    }

    /// What one cycle moves P0's life by where no limit holds a change back.
    fn p0_net_per_cycle(self) -> i64 {
        match self {
            Dynamo::Gain(gain) => i64::from(gain),
            Dynamo::GainThenLose { gain, loss } | Dynamo::LoseThenGain { loss, gain } => {
                i64::from(gain) - i64::from(loss)
            }
            Dynamo::Drain(_) => 0,
        }
    }
}

/// Whether `ability` gains life.
fn gains_life(ability: &AbilityDefinition) -> bool {
    matches!(&*ability.effect, Effect::GainLife { .. })
}

/// Whether `ability` loses life.
fn loses_life(ability: &AbilityDefinition) -> bool {
    matches!(&*ability.effect, Effect::LoseLife { .. })
}

/// Whether `ability` untaps.
fn untaps(ability: &AbilityDefinition) -> bool {
    matches!(
        &*ability.effect,
        Effect::SetTapState {
            state: TapStateChange::Untap,
            ..
        }
    )
}

/// The index of `source`'s first ability `matches` accepts.
fn ability_index(
    runner: &GameRunner,
    source: ObjectId,
    what: &str,
    matches: impl Fn(&AbilityDefinition) -> bool,
) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(matches)
        .unwrap_or_else(|| panic!("reach guard: the {what} ability parses"))
}

/// A P0 engine named Lifedynamo with `text` in Interactive loop detection, with
/// the given stored life fields and gained-this-turn counter presets (any seat).
fn engine_board(
    mut scenario: GameScenario,
    life_fields: &[(PlayerId, i32)],
    text: &str,
    counters: &[(PlayerId, u32)],
) -> (GameRunner, ObjectId) {
    for &(player, life) in life_fields {
        scenario.with_life(player, life);
    }
    let engine = scenario
        .add_creature_from_oracle(P0, "Lifedynamo", 2, 2, text)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    for &(seat, gained) in counters {
        let player = runner
            .state_mut()
            .players
            .iter_mut()
            .find(|player| player.id == seat)
            .expect("the preset seat is in the game");
        player.life_gained_this_turn = gained;
    }
    (runner, engine)
}

/// P0's `dynamo` on `engine_board`. Returns the runner, the dynamo and the
/// ability indices of one cycle in the variant's activation order, each found
/// by its effect.
fn dynamo_board(
    scenario: GameScenario,
    life_fields: &[(PlayerId, i32)],
    dynamo: Dynamo,
    counters: &[(PlayerId, u32)],
) -> (GameRunner, ObjectId, Vec<usize>) {
    let (runner, id) = engine_board(scenario, life_fields, &dynamo.text(), counters);
    let index = |what: &str, matches: fn(&AbilityDefinition) -> bool| {
        ability_index(&runner, id, what, matches)
    };
    let mana = index("mana", is_mana_ability);
    let untap = index("untap", untaps);
    let indices = match dynamo {
        Dynamo::Gain(_) => vec![mana, index("life-gain", gains_life), untap],
        Dynamo::GainThenLose { .. } => vec![
            mana,
            index("life-gain", gains_life),
            index("life-loss", loses_life),
            untap,
        ],
        Dynamo::LoseThenGain { .. } => vec![
            mana,
            index("life-loss", loses_life),
            index("life-gain", gains_life),
            untap,
        ],
        Dynamo::Drain(_) => vec![mana, index("life-loss", loses_life), untap],
    };
    (runner, id, indices)
}

/// Performs one dynamo cycle (the `indices` activations) and asserts the
/// shortcut offer that follows: P0's, with exactly `unbounded` as its unbounded
/// axes.
fn cycle_to_the_shortcut_offer(
    runner: &mut GameRunner,
    dynamo: ObjectId,
    indices: &[usize],
    unbounded: &[ResourceAxis],
) {
    for &index in indices {
        activate_and_settle(runner, dynamo, index);
    }
    let WaitingFor::LoopShortcut {
        proposer,
        certificate,
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

/// How a taken life shortcut lands at its accept (CR 732.2c).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Take {
    /// The take performs its cycles: one `LifeChanged` per change, no ∞ mark.
    Performed,
    /// The take marks its ∞ `Mana(C)` axis and applies its batched life at the
    /// take, as one P0 `LifeChanged` of the cycles' net.
    Lump,
}

/// A dynamo shortcut board: P0's dynamo, the stored life fields, the
/// gained-this-turn counter presets, and the texts of P0's `{0}` abilities
/// activated after the take or the performed cycles, in order.
struct ShortcutBoard {
    two_headed_giant: bool,
    fields: Vec<(PlayerId, i32)>,
    dynamo: Dynamo,
    counters: Vec<(PlayerId, u32)>,
    between: Vec<&'static str>,
}

impl ShortcutBoard {
    /// A two-player board with these stored life fields.
    fn duel(fields: Vec<(PlayerId, i32)>, dynamo: Dynamo) -> Self {
        ShortcutBoard {
            two_headed_giant: false,
            fields,
            dynamo,
            counters: Vec::new(),
            between: Vec::new(),
        }
    }

    /// A Two-Headed Giant board with team A's fields `(p0, p1)` and team B's
    /// members at 20.
    fn team(p0: i32, p1: i32, dynamo: Dynamo) -> Self {
        ShortcutBoard {
            two_headed_giant: true,
            fields: vec![(P0, p0), (P1, p1), (P2, 20), (PlayerId(3), 20)],
            dynamo,
            counters: Vec::new(),
            between: Vec::new(),
        }
    }

    /// The board with these gained-this-turn counter presets.
    fn counters(self, counters: Vec<(PlayerId, u32)>) -> Self {
        ShortcutBoard { counters, ..self }
    }

    /// The board with these abilities activated after the take.
    fn between(self, between: Vec<&'static str>) -> Self {
        ShortcutBoard { between, ..self }
    }

    /// The runner, the dynamo, one cycle's ability indices and the `between`
    /// abilities' sources.
    fn build(&self) -> (GameRunner, ObjectId, Vec<usize>, Vec<ObjectId>) {
        let mut scenario = if self.two_headed_giant {
            two_headed_giant()
        } else {
            two_player()
        };
        let between = self
            .between
            .iter()
            .map(|text| {
                scenario
                    .add_creature_from_oracle(P0, "Test Lifeshifter", 1, 1, text)
                    .id()
            })
            .collect();
        let (runner, dynamo, indices) =
            dynamo_board(scenario, &self.fields, self.dynamo, &self.counters);
        (runner, dynamo, indices, between)
    }

    /// P0's opponents, who answer the offer, in turn order.
    fn responders(&self) -> Vec<PlayerId> {
        if self.two_headed_giant {
            vec![P1, P2, PlayerId(3)]
        } else {
            vec![P1]
        }
    }

    /// The shortcut: one cycle offers it with exactly `unbounded`, it is
    /// accepted at `Fixed(cycles)`, `between` is activated, and the step ends.
    /// Asserts that the accept stashed nothing, wrote no collapse bound and
    /// marked no ∞ `Life` axis, that it landed as `take` says, and that the
    /// boundary asks no collapse amount. Returns the life outcome of the accept
    /// onward.
    fn take(&self, name: &str, cycles: u32, unbounded: &[ResourceAxis], take: Take) -> LifeOutcome {
        let (mut runner, dynamo, indices, between) = self.build();
        cycle_to_the_shortcut_offer(&mut runner, dynamo, &indices, unbounded);
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(cycles),
                template: None,
            })
            .expect("the shortcut declaration is accepted");
        let mut events = accept_for_every_responder(&mut runner, &self.responders());
        let p0_amounts: Vec<i64> = life_changes(&events)
            .into_iter()
            .filter(|&(player, _, _)| player == P0)
            .map(|(_, amount, _)| i64::from(amount))
            .collect();
        assert_eq!(stash(&runner), None, "{name}: nothing is stashed");
        assert_eq!(
            runner.state().pending_materialization_count.get(&P0),
            None,
            "{name}: no collapse bound"
        );
        let marks = runner.state().unbounded_resources.get(&P0);
        assert!(
            marks.is_none_or(|axes| !axes
                .iter()
                .any(|axis| matches!(axis, ResourceAxis::Life(_)))),
            "{name}: no ∞ Life mark after the accept, got {marks:?}"
        );
        match take {
            Take::Performed => {
                assert_eq!(marks, None, "{name}: a performed take marks nothing");
                assert_eq!(
                    p0_amounts.len(),
                    self.dynamo.p0_changes_per_cycle() * cycles as usize,
                    "{name}: one LifeChanged per change, got {p0_amounts:?}"
                );
            }
            Take::Lump => {
                assert!(
                    marks.is_some_and(|axes| axes.contains(&ResourceAxis::Mana(ManaType::Colorless))),
                    "REACH-GUARD {name}: the Mark route marks ∞ Mana(C), got {marks:?}"
                );
                assert_eq!(
                    p0_amounts,
                    vec![self.dynamo.p0_net_per_cycle() * i64::from(cycles)],
                    "{name}: one lump at the accept"
                );
            }
        }
        for &id in &between {
            events.extend(activate_and_settle(&mut runner, id, 0));
        }
        pass_to_a_boundary_without_a_collapse(&mut runner);
        LifeOutcome::read(&runner, &events)
    }

    /// The reference: one cycle offers the shortcut with exactly `unbounded`,
    /// it is declined, `cycles` more cycles are performed, `between` is
    /// activated, and the step ends. Returns the life outcome of the decline
    /// onward.
    fn perform(&self, cycles: u32, unbounded: &[ResourceAxis]) -> LifeOutcome {
        let (mut runner, dynamo, indices, between) = self.build();
        cycle_to_the_shortcut_offer(&mut runner, dynamo, &indices, unbounded);
        let mut events = decline_and_perform(&mut runner, dynamo, &indices, cycles);
        for &id in &between {
            events.extend(activate_and_settle(&mut runner, id, 0));
        }
        pass_to_a_boundary_without_a_collapse(&mut runner);
        LifeOutcome::read(&runner, &events)
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

/// CR 104.4b + CR 732.4: a mandatory loop (one "Whenever you gain
/// life, you gain 1 life." trigger after one kickoff gain) is a draw. At
/// `i32::MAX` the stored total cannot move, but every gain still carries its
/// rules amount, so the loop is drawn exactly as it is at 20, including from
/// one below the cap and for a Two-Headed Giant team whose shared total is at
/// the cap (CR 810.4). Residuals: a counter (gained or lost) pinned at
/// `u32::MAX` gives the cycle nothing to read, so the loop is not detected and
/// keeps firing; and with loop detection off nothing is drawn.
#[test]
fn a_mandatory_life_gain_loop_at_the_cap_is_drawn() {
    // Two players: (start life, life when drawn).
    for (start_life, drawn_life) in [(20, 22), (i32::MAX, i32::MAX), (i32::MAX - 1, i32::MAX)] {
        let (mut runner, kickoff) = mandatory_life_gain_loop(two_player(), &[(P0, start_life)], 0);
        runner.cast(kickoff).resolve();
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::GameOver { winner: None }
            ),
            "{start_life}: drawn, got {:?}",
            runner.state().waiting_for
        );
        assert_eq!(runner.life(P0), drawn_life, "{start_life}");
        assert_eq!(
            runner.state().players[0].life_gained_this_turn,
            2,
            "{start_life}"
        );
    }

    // Two-Headed Giant: (P0 and P1 fields, fields when drawn, team total).
    type TeamRow = ((i32, i32), (i32, i32), i64);
    let rows: [TeamRow; 2] = [
        ((i32::MAX - 5, 5), (i32::MAX - 5, 5), i64::from(i32::MAX)),
        ((20, 20), (22, 20), 42),
    ];
    for ((p0, p1), fields, team) in rows {
        let (mut runner, kickoff) = mandatory_life_gain_loop(
            two_headed_giant(),
            &[(P0, p0), (P1, p1), (P2, 20), (PlayerId(3), 20)],
            0,
        );
        runner.cast(kickoff).resolve();
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
        assert_eq!(runner.state().players[0].life_gained_this_turn, 2);
    }

    // Residual: the gained counter is pinned at `u32::MAX`.
    let (mut runner, kickoff) = mandatory_life_gain_loop(two_player(), &[(P0, i32::MAX)], u32::MAX);
    runner.cast(kickoff).resolve();
    let events = pass_beats(&mut runner, 100);
    assert!(
        is_priority(runner.state()),
        "the pinned counter is not drawn, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::LifeChanged {
                player_id,
                amount: 1,
                ..
            } if *player_id == P0
        )),
        "reach guard: the loop keeps gaining life"
    );
    assert_eq!(runner.state().players[0].life_gained_this_turn, u32::MAX);
    assert_eq!(runner.life(P0), i32::MAX);

    // Residual: the lost counter is pinned at `u32::MAX` while the gained counter
    // moves.
    let (mut runner, kickoff) = mandatory_life_gain_loop(two_player(), &[(P0, i32::MAX)], 0);
    runner.state_mut().players[0].life_lost_this_turn = u32::MAX;
    runner.cast(kickoff).resolve();
    pass_beats(&mut runner, 100);
    assert!(
        is_priority(runner.state()),
        "the pinned lost counter is not drawn, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().players[0].life_gained_this_turn > 2,
        "reach guard: the loop keeps gaining life past the drawn beat"
    );
    assert_eq!(runner.state().players[0].life_lost_this_turn, u32::MAX);
    assert_eq!(runner.life(P0), i32::MAX);

    // Loop detection off: nothing is drawn.
    let (mut runner, kickoff) = mandatory_life_gain_loop(two_player(), &[(P0, i32::MAX)], 0);
    runner.state_mut().loop_detection = LoopDetectionMode::Off;
    runner.cast(kickoff).resolve();
    pass_beats(&mut runner, 100);
    assert!(
        is_priority(runner.state()),
        "detection off: not drawn, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.life(P0), i32::MAX);
}

/// CR 104.4b: P0's two mandatory triggers feed each other (Exquisite Blood,
/// "Whenever an opponent loses life, you gain that much life.", and a
/// synthetic "Whenever you gain life, each opponent loses 1 life."), P1 can't
/// lose and P0 can't win (Platinum Angel), so the loop has no way to stop and
/// is a draw. P1's life total is held at `i32::MIN`, which Path B's at-limit
/// reading does not read.
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
    scenario.add_creature_from_oracle(P0, "Test Drain Engine", 2, 2, DRAIN_ENGINE);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
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

/// CR 119.3 + CR 732.2c: a shortcut of 1,000 Lifedynamo cycles at 3,000,000
/// life each gains 3,000,000,000, more than the total can take under
/// `i32::MAX`, so the take performs the cycles: nothing is stashed, the step
/// boundary asks no collapse amount, the total stops at `i32::MAX`, the gained
/// counter counts every cycle, and each cycle reports its 3,000,000.
#[test]
fn a_life_shortcut_gaining_past_i32_max_is_performed_at_the_take() {
    let (mut runner, dynamo, indices) =
        dynamo_board(two_player(), &[(P0, 20)], Dynamo::Gain(3_000_000), &[]);
    cycle_to_the_shortcut_offer(
        &mut runner,
        dynamo,
        &indices,
        &[
            ResourceAxis::Mana(ManaType::Colorless),
            ResourceAxis::Life(P0),
        ],
    );
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1000),
            template: None,
        })
        .expect("the shortcut declaration is accepted");
    let events = accept_for_every_responder(&mut runner, &[P1]);

    assert_eq!(stash(&runner), None, "nothing is stashed");
    assert_eq!(runner.life(P0), i32::MAX);
    assert_eq!(
        runner.state().players[0].life_gained_this_turn,
        3_003_000_000
    );
    let changes = life_changes(&events);
    assert_eq!(changes.len(), 1000, "one LifeChanged per cycle");
    assert!(changes
        .iter()
        .all(|&(player, amount, _)| player == P0 && amount == 3_000_000));
    assert_eq!(changes.first(), Some(&(P0, 3_000_000, Some(6_000_020))));
    assert_eq!(changes.last(), Some(&(P0, 3_000_000, Some(i32::MAX))));
    assert!(is_priority(runner.state()));
    pass_to_a_boundary_without_a_collapse(&mut runner);
    assert_eq!(runner.life(P0), i32::MAX);
}

/// CR 104.4b + CR 732.2a + CR 732.2c: at `i32::MAX` a Lifedynamo cycle's gain
/// cannot grow the stored total, so the shortcut offer carries no life axis.
/// The cycles' 3,000,000,000 cannot fit under the cap, so accepting performs
/// the 1,000 cycles at the take: nothing is stashed, the total stays at
/// `i32::MAX`, the gained counter counts every cycle, and each cycle reports
/// its 3,000,000. With the gained counter pinned at `u32::MAX` the take is
/// performed too, since each cycle's gain is held at the cap (the cycle's
/// `LifeChanged` events report it where the pinned counter cannot): nothing is
/// stashed and the boundary asks no collapse amount.
#[test]
fn a_life_shortcut_offer_at_the_cap_carries_no_life_axis() {
    // Row A: the take performs the cycles.
    let (mut runner, dynamo, indices) = dynamo_board(
        two_player(),
        &[(P0, i32::MAX)],
        Dynamo::Gain(3_000_000),
        &[],
    );
    cycle_to_the_shortcut_offer(
        &mut runner,
        dynamo,
        &indices,
        &[ResourceAxis::Mana(ManaType::Colorless)],
    );
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1000),
            template: None,
        })
        .expect("the shortcut declaration is accepted");
    let events = accept_for_every_responder(&mut runner, &[P1]);

    assert_eq!(stash(&runner), None, "nothing is stashed");
    assert_eq!(runner.life(P0), i32::MAX);
    assert_eq!(
        runner.state().players[0].life_gained_this_turn,
        3_003_000_000
    );
    assert_eq!(
        life_changes(&events),
        vec![(P0, 3_000_000, Some(i32::MAX)); 1000],
        "one LifeChanged per cycle"
    );
    assert!(is_priority(runner.state()));
    pass_to_a_boundary_without_a_collapse(&mut runner);

    // Row B: the gained counter is pinned at `u32::MAX`.
    let (mut runner, dynamo, indices) = dynamo_board(
        two_player(),
        &[(P0, i32::MAX)],
        Dynamo::Gain(5),
        &[(P0, u32::MAX)],
    );
    cycle_to_the_shortcut_offer(
        &mut runner,
        dynamo,
        &indices,
        &[ResourceAxis::Mana(ManaType::Colorless)],
    );
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: None,
        })
        .expect("the shortcut declaration is accepted");
    accept_for_every_responder(&mut runner, &[P1]);
    assert!(
        !stash(&runner).is_some_and(|items| items
            .iter()
            .any(|item| matches!(item, PersistentAxisMaterialization::Life { .. }))),
        "pinned: no life growth is stashed: {:?}",
        stash(&runner)
    );
    pass_to_next_boundary(&mut runner);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::PayAmountChoice { .. }
        ),
        "pinned: the boundary asks no collapse amount, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.life(P0), i32::MAX);
    assert_eq!(runner.state().players[0].life_gained_this_turn, u32::MAX);
}

/// CR 119.3 + CR 732.2c: each of 1000 cycles gains 2 life. From one below the
/// cap their 2,000 cannot fit under `i32::MAX`, so the take performs the
/// cycles: nothing is stashed, the boundary asks no collapse amount, the total
/// stops at the cap, the gained counter grows by 2,000 and each cycle reports 2.
/// In Two-Headed Giant the cap is the team's shared total (CR 810.4 +
/// CR 810.9a). With the gained counter pinned at `u32::MAX` the counter stays
/// pinned and each performed cycle still reports 2. Control: from 22 the gain
/// fits, so it lands at the take in one lump of 2,000, and the boundary asks
/// nothing.
#[test]
fn a_life_shortcut_period_crossing_the_cap_gains_the_rules_amount() {
    struct Row {
        name: &'static str,
        two_headed_giant: bool,
        fields: Vec<(PlayerId, i32)>,
        gained: u32,
        unbounded: Vec<ResourceAxis>,
        p0_life_at_offer: i32,
        responders: Vec<PlayerId>,
        take: Take,
        p0_life_after: i32,
        gained_after: u32,
    }
    let mana = ResourceAxis::Mana(ManaType::Colorless);
    let rows = [
        Row {
            name: "crossing the cap",
            two_headed_giant: false,
            fields: vec![(P0, i32::MAX - 3)],
            gained: 0,
            unbounded: vec![mana],
            p0_life_at_offer: i32::MAX - 1,
            responders: vec![P1],
            take: Take::Performed,
            p0_life_after: i32::MAX,
            gained_after: 2002,
        },
        Row {
            name: "Two-Headed Giant team crossing the cap",
            two_headed_giant: true,
            fields: vec![(P0, i32::MAX - 8), (P1, 5)],
            gained: 0,
            unbounded: vec![mana],
            p0_life_at_offer: i32::MAX - 6,
            responders: vec![P1, P2, PlayerId(3)],
            take: Take::Performed,
            p0_life_after: i32::MAX - 5,
            gained_after: 2002,
        },
        Row {
            name: "control below the cap",
            two_headed_giant: false,
            fields: vec![(P0, 20)],
            gained: 0,
            unbounded: vec![mana, ResourceAxis::Life(P0)],
            p0_life_at_offer: 22,
            responders: vec![P1],
            take: Take::Lump,
            p0_life_after: 2022,
            gained_after: 2002,
        },
        Row {
            name: "gained counter pinned at u32::MAX",
            two_headed_giant: false,
            fields: vec![(P0, i32::MAX - 3)],
            gained: u32::MAX,
            unbounded: vec![mana],
            p0_life_at_offer: i32::MAX - 1,
            responders: vec![P1],
            take: Take::Performed,
            p0_life_after: i32::MAX,
            gained_after: u32::MAX,
        },
    ];
    for row in rows {
        let name = row.name;
        let scenario = if row.two_headed_giant {
            two_headed_giant()
        } else {
            two_player()
        };
        let (mut runner, dynamo, indices) =
            dynamo_board(scenario, &row.fields, Dynamo::Gain(2), &[(P0, row.gained)]);
        cycle_to_the_shortcut_offer(&mut runner, dynamo, &indices, &row.unbounded);
        assert_eq!(runner.life(P0), row.p0_life_at_offer, "{name}");
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1000),
                template: None,
            })
            .expect("the shortcut declaration is accepted");
        let events = accept_for_every_responder(&mut runner, &row.responders);

        assert_eq!(stash(&runner), None, "{name}: nothing is stashed");
        let changes = life_changes(&events);
        match row.take {
            Take::Lump => {
                assert!(
                    runner
                        .state()
                        .unbounded_resources
                        .get(&P0)
                        .is_some_and(|axes| axes.contains(&mana)),
                    "REACH-GUARD {name}: the Mark route marks ∞ Mana(C)"
                );
                assert_eq!(
                    changes,
                    vec![(P0, 2000, Some(row.p0_life_after))],
                    "{name}: one lump at the accept"
                );
            }
            Take::Performed => {
                assert_eq!(changes.len(), 1000, "{name}: one LifeChanged per cycle");
                assert!(
                    changes
                        .iter()
                        .all(|&(player, amount, _)| player == P0 && amount == 2),
                    "{name}: each cycle reports 2"
                );
            }
        }
        assert!(is_priority(runner.state()), "{name}");
        pass_to_a_boundary_without_a_collapse(&mut runner);

        assert_eq!(runner.life(P0), row.p0_life_after, "{name}");
        assert_eq!(
            changes.last().and_then(|&(_, _, new_total)| new_total),
            Some(row.p0_life_after),
            "{name}"
        );
        if row.two_headed_giant {
            assert_eq!(team_a_fields(runner.state()), (i32::MAX - 5, 5), "{name}");
            assert_eq!(
                team_life_sum(runner.state(), P0),
                i64::from(i32::MAX),
                "{name}"
            );
        }
        assert_eq!(
            runner.state().players[0].life_gained_this_turn,
            row.gained_after,
            "{name}"
        );
    }
}

/// P0's `{0}` ability that loses 4 life, activated between a shortcut's accept
/// and the step boundary.
const LOSE_FOUR: &str = "{0}: You lose 4 life.";

/// P0's `{0}` ability that gains 98 life, activated between a shortcut's accept
/// and the step boundary.
const GAIN_NINETY_EIGHT: &str = "{0}: You gain 98 life.";

/// P0's `{0}` ability that gains 108 life, activated between a shortcut's
/// accept and the step boundary.
const GAIN_108: &str = "{0}: You gain 108 life.";

/// P0's `{0}` ability that loses 25 life, activated between a shortcut's accept
/// and the step boundary.
const LOSE_TWENTY_FIVE: &str = "{0}: You lose 25 life.";

/// CR 732.2c + CR 119.3: a taken shortcut leaves the game as if its cycles were
/// performed at the take, so P0's gained and lost counters, both stored life
/// fields, the shared total and the sum of P0's `LifeChanged` amounts end where
/// declining the shortcut and performing the cycles leaves them, also when P0
/// loses or gains life after the accept and before the step boundary. A take
/// whose cycles' life a lump stores as performing stores it applies the lump at
/// the take: below the cap, reaching it exactly, far below it before a later
/// gain held at the cap and a loss, before a loss that would be lethal without
/// the lump, beside a bystander pinned at the cap, and reaching the cap exactly
/// with a pinned counter. Every other take performs its cycles: where a cycle's
/// change is held at the cap (a total held at it, a period crossing it, a lump
/// one cycle past it, a gain then a loss that touches it, with a readable or a
/// pinned counter), where P0's own field cannot hold the lump, and where a cycle
/// stores P0's gain in P1's field (CR 810.9 + CR 810.9a), also where P0's
/// counter is pinned and cannot say who gained. The offer names P0's life axis,
/// never P1's, and none where the counters cannot attribute the gain. The
/// boundary asks no collapse amount, and no ∞ life mark follows the accept.
#[test]
fn a_life_shortcut_held_at_the_cap_matches_performing_the_cycles() {
    struct Row {
        name: &'static str,
        board: ShortcutBoard,
        cycles: u32,
        unbounded: Vec<ResourceAxis>,
        take: Take,
        gained: u32,
    }
    let mana = ResourceAxis::Mana(ManaType::Colorless);
    let life = ResourceAxis::Life(P0);
    let max = i32::MAX;
    let duel = |p0: i32, dynamo: Dynamo| ShortcutBoard::duel(vec![(P0, p0)], dynamo);
    let pinned = || vec![(P0, u32::MAX)];
    let rows = [
        Row {
            name: "held at the cap",
            board: duel(max, Dynamo::Gain(5)),
            cycles: 3,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: 20,
        },
        Row {
            name: "held at the cap, then P0 loses 4",
            board: duel(max, Dynamo::Gain(5)).between(vec![LOSE_FOUR]),
            cycles: 3,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: 20,
        },
        Row {
            name: "Two-Headed Giant team held at the cap",
            board: ShortcutBoard::team(max - 5, 5, Dynamo::Gain(2)),
            cycles: 3,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: 8,
        },
        Row {
            name: "Two-Headed Giant team held at the cap, then P0 loses 4",
            board: ShortcutBoard::team(max - 5, 5, Dynamo::Gain(2)).between(vec![LOSE_FOUR]),
            cycles: 3,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: 8,
        },
        Row {
            name: "crossing the cap",
            board: duel(max - 3, Dynamo::Gain(2)),
            cycles: 3,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: 8,
        },
        Row {
            name: "crossing the cap, then P0 loses 4",
            board: duel(max - 3, Dynamo::Gain(2)).between(vec![LOSE_FOUR]),
            cycles: 3,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: 8,
        },
        Row {
            name: "one past the cap, then P0 loses 4",
            board: duel(max - 7, Dynamo::Gain(2)).between(vec![LOSE_FOUR]),
            cycles: 3,
            unbounded: vec![mana, life],
            take: Take::Performed,
            gained: 8,
        },
        Row {
            name: "Two-Headed Giant, both fields grow, then P0 loses 4",
            board: ShortcutBoard::team(max - 3, -20, Dynamo::Gain(2)).between(vec![LOSE_FOUR]),
            cycles: 15,
            unbounded: vec![mana, life],
            take: Take::Performed,
            gained: 32,
        },
        Row {
            name: "Two-Headed Giant, P0's field one below the cap, the team has room",
            board: ShortcutBoard::team(max - 3, -20, Dynamo::Gain(2)),
            cycles: 5,
            unbounded: vec![mana, life],
            take: Take::Performed,
            gained: 12,
        },
        Row {
            name: "Two-Headed Giant, P0's field one below the cap, the team has room, then P0 \
                   loses 4",
            board: ShortcutBoard::team(max - 3, -20, Dynamo::Gain(2)).between(vec![LOSE_FOUR]),
            cycles: 5,
            unbounded: vec![mana, life],
            take: Take::Performed,
            gained: 12,
        },
        Row {
            name: "Two-Headed Giant, P0's field at the cap, the team has room",
            board: ShortcutBoard::team(max, -100, Dynamo::Gain(2)),
            cycles: 5,
            unbounded: vec![mana, life],
            take: Take::Performed,
            gained: 12,
        },
        Row {
            name: "Two-Headed Giant, P0's field at the cap, the cap holds back the cycle the \
                   offer reads",
            board: ShortcutBoard::team(max, -400, Dynamo::Gain(150)),
            cycles: 3,
            unbounded: vec![mana, life],
            take: Take::Performed,
            gained: 600,
        },
        Row {
            name: "Two-Headed Giant, a lump that fits, the cap holds back the cycle the offer \
                   reads",
            board: ShortcutBoard::team(max - 45, -10, Dynamo::Gain(20)),
            cycles: 1,
            unbounded: vec![mana, life],
            take: Take::Lump,
            gained: 40,
        },
        Row {
            name: "Two-Headed Giant, the lump would spill onto P1's field, then P0 loses 4",
            board: ShortcutBoard::team(max - 6, -20, Dynamo::Gain(2)).between(vec![LOSE_FOUR]),
            cycles: 5,
            unbounded: vec![mana, life],
            take: Take::Performed,
            gained: 12,
        },
        Row {
            name: "Two-Headed Giant below the cap, then P0 loses 4",
            board: ShortcutBoard::team(20, 20, Dynamo::Gain(2)).between(vec![LOSE_FOUR]),
            cycles: 5,
            unbounded: vec![mana, life],
            take: Take::Lump,
            gained: 12,
        },
        Row {
            name: "reaching the cap exactly, then P0 loses 4",
            board: duel(max - 8, Dynamo::Gain(2)).between(vec![LOSE_FOUR]),
            cycles: 3,
            unbounded: vec![mana, life],
            take: Take::Lump,
            gained: 8,
        },
        Row {
            name: "below the cap",
            board: duel(20, Dynamo::Gain(2)),
            cycles: 3,
            unbounded: vec![mana, life],
            take: Take::Lump,
            gained: 8,
        },
        Row {
            name: "below the cap, then P0 loses 4",
            board: duel(20, Dynamo::Gain(2)).between(vec![LOSE_FOUR]),
            cycles: 3,
            unbounded: vec![mana, life],
            take: Take::Lump,
            gained: 8,
        },
        Row {
            name: "a lump below the cap, then a gain carries the total to it",
            board: duel(max - 102, Dynamo::Gain(2)).between(vec![GAIN_NINETY_EIGHT]),
            cycles: 3,
            unbounded: vec![mana, life],
            take: Take::Lump,
            gained: 106,
        },
        Row {
            name: "a lump far below the cap, then a gain held at the cap and a loss",
            board: duel(max - 110, Dynamo::Gain(2)).between(vec![GAIN_108, LOSE_FOUR]),
            cycles: 3,
            unbounded: vec![mana, life],
            take: Take::Lump,
            gained: 116,
        },
        Row {
            name: "a lump below the cap, then a loss that would be lethal without it",
            board: duel(20, Dynamo::Gain(2)).between(vec![LOSE_TWENTY_FIVE]),
            cycles: 3,
            unbounded: vec![mana, life],
            take: Take::Lump,
            gained: 8,
        },
        Row {
            name: "Two-Headed Giant, P0's field at the cap with its gained counter pinned, the \
                   team has room",
            board: ShortcutBoard::team(max, -100, Dynamo::Gain(2)).counters(pinned()),
            cycles: 5,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: u32::MAX,
        },
        Row {
            name: "held at the cap with the gained counter pinned",
            board: duel(max, Dynamo::Gain(5)).counters(pinned()),
            cycles: 3,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: u32::MAX,
        },
        Row {
            name: "crossing the cap in one cycle with the gained counter pinned",
            board: duel(max - 3, Dynamo::Gain(2)).counters(pinned()),
            cycles: 1,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: u32::MAX,
        },
        Row {
            name: "a gain then a loss whose gain touches the cap",
            board: duel(max - 20, Dynamo::GainThenLose { gain: 15, loss: 2 }),
            cycles: 1,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: 30,
        },
        Row {
            name: "a gain then a loss held at the cap every cycle",
            board: duel(max - 2, Dynamo::GainThenLose { gain: 15, loss: 2 }),
            cycles: 3,
            unbounded: vec![mana],
            take: Take::Performed,
            gained: 60,
        },
        Row {
            name: "a bystander pinned at the cap",
            board: ShortcutBoard::duel(vec![(P0, 20), (P1, max)], Dynamo::Gain(2))
                .counters(vec![(P1, u32::MAX)]),
            cycles: 3,
            unbounded: vec![mana, life],
            take: Take::Lump,
            gained: 8,
        },
        Row {
            name: "reaching the cap exactly with the gained counter pinned",
            board: duel(max - 4, Dynamo::Gain(2)).counters(pinned()),
            cycles: 1,
            unbounded: vec![mana],
            take: Take::Lump,
            gained: u32::MAX,
        },
    ];
    for row in rows {
        let name = row.name;
        let shortcut = row.board.take(name, row.cycles, &row.unbounded, row.take);
        let performed = row.board.perform(row.cycles, &row.unbounded);
        assert_eq!(shortcut, performed, "{name}");
        assert_eq!(shortcut.gained, row.gained, "{name}: the gained counter");
    }
}

/// CR 732.2c + CR 119.3: two life shortcuts taken in one step leave the game as
/// if their cycles were performed at each take. P0 performs one Lifedynamo cycle
/// (gain 2), accepts `Fixed(4)`, performs one more cycle, accepts `Fixed(4)`
/// again, then loses 4, and the step ends; the reference declines and performs
/// the same ten cycles, then loses 4. Each take that fits applies its lump at
/// the take, so neither stashes anything or records a collapse bound, and the
/// boundary asks nothing. From 20 both lumps fit: P0 is at 40 after the second
/// take and ends at 36. From `i32::MAX - 12` the first lump fits and the second
/// take, from the cap, performs its cycles held at it: P0 is at `i32::MAX`
/// after the second take and ends at `i32::MAX - 4`. Both have gained 20.
#[test]
fn two_life_shortcuts_in_one_step_match_performing_the_cycles() {
    let mana = ResourceAxis::Mana(ManaType::Colorless);
    let life = ResourceAxis::Life(P0);
    // (name, P0's starting life, P0's life after the second take)
    for (name, start, after_takes) in [
        ("past the cap", i32::MAX - 12, i32::MAX),
        ("below the cap", 20, 40),
    ] {
        let board = || {
            let mut scenario = two_player();
            let loser = scenario
                .add_creature_from_oracle(P0, "Test Lifeshifter", 1, 1, LOSE_FOUR)
                .id();
            let (runner, dynamo, indices) =
                dynamo_board(scenario, &[(P0, start)], Dynamo::Gain(2), &[]);
            (runner, dynamo, indices, loser)
        };
        let accept = |runner: &mut GameRunner| {
            runner
                .act(GameAction::DeclareShortcut {
                    count: IterationCount::Fixed(4),
                    template: None,
                })
                .expect("the shortcut declaration is accepted");
            accept_for_every_responder(runner, &[P1])
        };

        // The shortcut: one cycle, accept 4; one more cycle, accept 4; lose 4.
        let (mut runner, dynamo, indices, loser) = board();
        cycle_to_the_shortcut_offer(&mut runner, dynamo, &indices, &[mana, life]);
        let mut events = accept(&mut runner);
        for &index in &indices {
            events.extend(activate_and_settle(&mut runner, dynamo, index));
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
            "{name}: REACH-GUARD: the second cycle offers the shortcut again, got {:?}",
            runner.state().waiting_for
        );
        events.extend(accept(&mut runner));
        assert_eq!(stash(&runner), None, "{name}: nothing is stashed");
        assert_eq!(
            runner.state().pending_materialization_count.get(&P0),
            None,
            "{name}: no bound is recorded"
        );
        assert_eq!(runner.life(P0), after_takes, "{name}: P0 after the takes");
        events.extend(activate_and_settle(&mut runner, loser, 0));
        pass_to_a_boundary_without_a_collapse(&mut runner);
        let shortcut = LifeOutcome::read(&runner, &events);

        // The reference: decline, perform nine more cycles, lose 4.
        let (mut runner, dynamo, indices, loser) = board();
        cycle_to_the_shortcut_offer(&mut runner, dynamo, &indices, &[mana, life]);
        let mut events = decline_and_perform(&mut runner, dynamo, &indices, 9);
        events.extend(activate_and_settle(&mut runner, loser, 0));
        pass_to_a_boundary_without_a_collapse(&mut runner);
        let performed = LifeOutcome::read(&runner, &events);

        assert_eq!(shortcut, performed, "{name}");
        assert_eq!(shortcut.gained, 20, "{name}: the gained counter");
    }
}

/// Angelic Accord's verbatim Oracle text (Scryfall).
const ANGELIC_ACCORD: &str = "At the beginning of each end step, if you gained 4 or more life \
                              this turn, create a 4/4 white Angel creature token with flying.";

/// Passes P0's turn forward to the end step (declaring no attackers, and
/// collapsing a stashed loop at its maximum where a boundary asks) until the end
/// step holds priority with an empty stack, so its beginning-of-step trigger has
/// resolved.
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

/// CR 603.4 + CR 732.2c: Angelic Accord's intervening "if" reads the life P0
/// gained this turn, and a taken shortcut gains what performing its cycles
/// would even while P0's total is held at `i32::MAX`. One Lifedynamo cycle
/// gaining 1 plus a `Fixed(3)` shortcut gains 4, so the end step makes one
/// Angel, as declining and performing the three cycles does; `Fixed(2)` gains 3
/// and makes none.
#[test]
fn angelic_accord_counts_the_life_a_shortcut_held_at_the_cap_gains() {
    // (shortcut count, or `None` to decline and perform three cycles; gained;
    // Angel tokens)
    for (count, gained, angels) in [(Some(3), 4, 1), (None, 4, 1), (Some(2), 3, 0)] {
        let mut scenario = two_player();
        scenario.add_enchantment_from_oracle(P0, "Angelic Accord", ANGELIC_ACCORD);
        let (mut runner, dynamo, indices) =
            dynamo_board(scenario, &[(P0, i32::MAX)], Dynamo::Gain(1), &[]);
        cycle_to_the_shortcut_offer(
            &mut runner,
            dynamo,
            &indices,
            &[ResourceAxis::Mana(ManaType::Colorless)],
        );
        match count {
            Some(count) => {
                runner
                    .act(GameAction::DeclareShortcut {
                        count: IterationCount::Fixed(count),
                        template: None,
                    })
                    .expect("the shortcut declaration is accepted");
                accept_for_every_responder(&mut runner, &[P1]);
            }
            None => {
                decline_and_perform(&mut runner, dynamo, &indices, 3);
            }
        }

        settle_end_step(&mut runner);

        let state = runner.state();
        let angel_tokens = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token && object.name == "Angel")
            .count();
        assert_eq!(
            state.players[0].life_gained_this_turn, gained,
            "{count:?}: life gained this turn"
        );
        assert_eq!(angel_tokens, angels, "{count:?}: Angel tokens");
        assert_eq!(runner.life(P0), i32::MAX, "{count:?}");
    }
}

/// CR 732.2c + CR 119.3: a period that gains 15 and then loses 2 nets 13 a
/// cycle, so a `Fixed(3)` lump of 39 can fit under `i32::MAX` while a later
/// cycle's gain, performed, is held at the cap before its loss. From
/// `i32::MAX - 53` (`i32::MAX - 40` at the take) the third cycle's gain passes
/// the cap, so the take performs the cycles and every counter, event and total
/// ends where performing them leaves it: P0 at `i32::MAX - 2`, having gained 60
/// and lost 8. Two above it fits: from `i32::MAX - 55` no cycle reaches the cap,
/// and the lump lands at the take. A period that loses 2 before it gains 15
/// peaks at its net, so from `i32::MAX - 53` and from `i32::MAX - 52` (reaching
/// the cap exactly) its lump lands at the take too. Where the lump lands, the
/// fields, the total and P0's `LifeChanged` sum match performing; its gained
/// and lost counters are not compared, since a lump moves the gained counter by
/// the net and the lost counter by nothing.
#[test]
fn a_gain_then_loss_shortcut_is_performed_where_a_later_cycle_passes_the_cap() {
    let mana = ResourceAxis::Mana(ManaType::Colorless);
    let life = ResourceAxis::Life(P0);
    let max = i32::MAX;
    let gain_then_lose = Dynamo::GainThenLose { gain: 15, loss: 2 };
    let lose_then_gain = Dynamo::LoseThenGain { loss: 2, gain: 15 };
    // (name, P0's starting life, the dynamo, the take, P0's life after)
    let rows = [
        (
            "a gain then a loss whose third cycle's gain passes the cap",
            max - 53,
            gain_then_lose,
            Take::Performed,
            max - 2,
        ),
        (
            "control: a gain then a loss no cycle carries to the cap",
            max - 55,
            gain_then_lose,
            Take::Lump,
            max - 3,
        ),
        (
            "a loss then a gain peaking one below the cap",
            max - 53,
            lose_then_gain,
            Take::Lump,
            max - 1,
        ),
        (
            "a loss then a gain reaching the cap exactly",
            max - 52,
            lose_then_gain,
            Take::Lump,
            max,
        ),
    ];
    for (name, start, dynamo, take, p0_after) in rows {
        let board = ShortcutBoard::duel(vec![(P0, start)], dynamo);
        let shortcut = board.take(name, 3, &[mana, life], take);
        let performed = board.perform(3, &[mana, life]);
        let fields_and_events = |outcome: &LifeOutcome| {
            (
                outcome.fields,
                outcome.team,
                outcome.life_changed_total,
                outcome.last_new_total,
            )
        };
        assert_eq!(
            fields_and_events(&shortcut),
            fields_and_events(&performed),
            "{name}"
        );
        assert_eq!(shortcut.fields.0, p0_after, "{name}: P0's life");
        if take == Take::Performed {
            assert_eq!(shortcut, performed, "{name}: every counter too");
            assert_eq!((shortcut.gained, shortcut.lost), (60, 8), "{name}");
        }
    }
}

/// CR 119.3 + CR 704.5a + CR 732.2c: Platinum Angel ("You can't lose the game
/// and your opponents can't win the game.") keeps P1 in the game at
/// `i32::MIN + 4` while P0's drain dynamo makes each opponent lose 2 a cycle.
/// One cycle leaves P1 at `i32::MIN + 2` and offers the shortcut with only its
/// ∞ `Mana(C)` axis. A `Fixed(1)` take's cycle loses P1's total down to exactly
/// `i32::MIN`, and a loss is not a gain a lump carries, so the take performs the
/// cycle whether P1's lost counter is readable or pinned at `u32::MAX`: nothing
/// is stashed or marked, the boundary asks nothing, the accept reports P1's loss
/// of 2 at `i32::MIN`, and P1's life and lost counter and P0's life end where
/// declining and performing the cycle leaves them.
#[test]
fn a_shortcut_losing_a_total_down_to_i32_min_is_performed_whatever_its_lost_counter() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let mana = ResourceAxis::Mana(ManaType::Colorless);
    let board = |lost: u32| {
        let mut scenario = two_player();
        scenario.add_real_card(P1, "Platinum Angel", Zone::Battlefield, db);
        let (mut runner, dynamo, indices) = dynamo_board(
            scenario,
            &[(P0, 20), (P1, i32::MIN + 4)],
            Dynamo::Drain(2),
            &[],
        );
        runner.state_mut().players[1].life_lost_this_turn = lost;
        cycle_to_the_shortcut_offer(&mut runner, dynamo, &indices, &[mana]);
        assert_eq!(
            runner.life(P1),
            i32::MIN + 2,
            "REACH-GUARD: one cycle drains P1 by 2"
        );
        assert!(
            !runner.state().players[1].is_eliminated,
            "REACH-GUARD: Platinum Angel keeps P1 in the game"
        );
        (runner, dynamo, indices)
    };
    // P1's life, P1's lost counter, P0's life.
    let reading = |runner: &GameRunner| {
        (
            runner.life(P1),
            runner.state().players[1].life_lost_this_turn,
            runner.life(P0),
        )
    };
    // (name, P1's lost counter at the start, after)
    for (name, lost, lost_after) in [("readable", 0, 4), ("pinned", u32::MAX, u32::MAX)] {
        // The shortcut.
        let (mut runner, _, _) = board(lost);
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1),
                template: None,
            })
            .expect("the shortcut declaration is accepted");
        let events = accept_for_every_responder(&mut runner, &[P1]);
        assert_eq!(
            life_changes(&events),
            vec![(P1, -2, Some(i32::MIN))],
            "{name}: the take performs the cycle's loss"
        );
        assert_eq!(stash(&runner), None, "{name}: nothing is stashed");
        assert_eq!(
            runner.state().unbounded_resources.get(&P0),
            None,
            "{name}: a performed take marks nothing"
        );
        pass_to_a_boundary_without_a_collapse(&mut runner);
        let shortcut = reading(&runner);

        // The reference: decline, perform the cycle.
        let (mut runner, dynamo, indices) = board(lost);
        decline_and_perform(&mut runner, dynamo, &indices, 1);
        pass_to_a_boundary_without_a_collapse(&mut runner);
        let performed = reading(&runner);

        assert_eq!(shortcut, performed, "{name}");
        assert_eq!(shortcut, (i32::MIN, lost_after, 20), "{name}");
    }
}

/// A synthetic free engine of P0's whose cycle gains 2 life and puts a charge
/// counter on itself.
const LIFE_AND_CHARGE_DYNAMO: &str = "{T}: Add {C}.\n{0}: You gain 2 life.\n{0}: Put a charge \
                                      counter on Lifedynamo.\n{0}: Untap Lifedynamo.";

/// A synthetic free engine of P0's whose cycle gains 2 life and creates a Servo
/// token.
const LIFE_AND_SERVO_DYNAMO: &str = "{T}: Add {C}.\n{0}: You gain 2 life.\n{0}: Create a 1/1 \
                                     colorless Servo artifact creature token.\n{0}: Untap \
                                     Lifedynamo.";

/// CR 732.2c + CR 119.3: a marked take lands its batched life at the take, at
/// the accepted count, but its counters and tokens at the CR 500.5 boundary, at
/// the count its controller names there, so a take that batches life beside
/// either is performed instead. A `Fixed(3)` take of a cycle that gains 2 life
/// and puts a charge counter on the dynamo, and of one that gains 2 life and
/// creates a Servo with no battlefield-entry trigger on the board, performs all
/// three cycles at the take: a `LifeChanged` per cycle, real counters or tokens
/// at the take, nothing stashed or marked, and no boundary prompt. P0's life,
/// gained counter, charge counters and Servos then end where declining and
/// performing the cycles leaves them.
#[test]
fn a_life_shortcut_that_also_grows_tokens_or_counters_is_performed_at_the_take() {
    let mana = ResourceAxis::Mana(ManaType::Colorless);
    let life = ResourceAxis::Life(P0);
    let charge = CounterType::Generic("charge".to_string());
    let puts_a_counter =
        |ability: &AbilityDefinition| matches!(&*ability.effect, Effect::PutCounter { .. });
    let creates_a_token =
        |ability: &AbilityDefinition| matches!(&*ability.effect, Effect::Token { .. });
    // (name, the engine's text, its growth ability, the offer's growth axis, P0's charge
    // counters and Servos after the take)
    type Row = (
        &'static str,
        &'static str,
        fn(&AbilityDefinition) -> bool,
        ResourceAxis,
        (u32, usize),
    );
    let rows: [Row; 2] = [
        (
            "charge counter",
            LIFE_AND_CHARGE_DYNAMO,
            puts_a_counter,
            ResourceAxis::Counter(CounterClass::Other, ObjectClass::Creature),
            (4, 0),
        ),
        (
            "Servo token, no ETB trigger",
            LIFE_AND_SERVO_DYNAMO,
            creates_a_token,
            ResourceAxis::TokensCreated,
            (0, 4),
        ),
    ];
    for (name, text, grows, growth_axis, growth_after) in rows {
        let board = || {
            let (runner, engine) = engine_board(two_player(), &[(P0, 20)], text, &[]);
            let indices = [
                ability_index(&runner, engine, "mana", is_mana_ability),
                ability_index(&runner, engine, "life-gain", gains_life),
                ability_index(&runner, engine, "growth", grows),
                ability_index(&runner, engine, "untap", untaps),
            ];
            (runner, engine, indices)
        };
        // P0's life, P0's gained counter, the engine's charge counters, P0's Servos.
        let reading = |runner: &GameRunner, engine: ObjectId| {
            let state = runner.state();
            let servos = state
                .battlefield
                .iter()
                .filter_map(|id| state.objects.get(id))
                .filter(|object| object.is_token && object.name == "Servo")
                .count();
            (
                runner.life(P0),
                state.players[0].life_gained_this_turn,
                state.objects[&engine]
                    .counters
                    .get(&charge)
                    .copied()
                    .unwrap_or(0),
                servos,
            )
        };

        // The shortcut.
        let (mut runner, engine, indices) = board();
        cycle_to_the_shortcut_offer(&mut runner, engine, &indices, &[mana, life, growth_axis]);
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(3),
                template: None,
            })
            .expect("the shortcut declaration is accepted");
        let events = accept_for_every_responder(&mut runner, &[P1]);
        assert_eq!(
            life_changes(&events)
                .into_iter()
                .map(|(player, amount, _)| (player, amount))
                .collect::<Vec<_>>(),
            vec![(P0, 2); 3],
            "{name}: a LifeChanged per performed cycle"
        );
        assert_eq!(stash(&runner), None, "{name}: nothing is stashed");
        assert_eq!(
            runner.state().unbounded_resources.get(&P0),
            None,
            "{name}: a performed take marks nothing"
        );
        assert_eq!(
            reading(&runner, engine),
            (28, 8, growth_after.0, growth_after.1),
            "{name}: every cycle's growth lands at the take"
        );
        pass_to_a_boundary_without_a_collapse(&mut runner);
        let shortcut = reading(&runner, engine);

        // The reference: decline, perform three cycles.
        let (mut runner, engine, indices) = board();
        cycle_to_the_shortcut_offer(&mut runner, engine, &indices, &[mana, life, growth_axis]);
        decline_and_perform(&mut runner, engine, &indices, 3);
        pass_to_a_boundary_without_a_collapse(&mut runner);
        let performed = reading(&runner, engine);

        assert_eq!(shortcut, performed, "{name}");
    }
}

/// CR 104.4b + CR 810.9 + CR 810.9a: an optional life-gain loop (P2 holds
/// Lightning Bolt and a Mountain, so a player can break it) is marked as P0's
/// revocable ∞ life capability. In Two-Headed Giant with P0's own field at
/// `i32::MAX`, every gain P0 makes is stored on P1's field, and the mark still
/// names P0's life axis, the member whose counters gained, never P1's. Control:
/// with room on P0's field the gains stay there and the mark names P0's life
/// axis too.
#[test]
fn an_optional_life_gain_loop_marks_the_two_headed_giant_member_who_gains() {
    // (name, team A's fields)
    for (name, p0, p1) in [
        (
            "P0's field at the cap, the gains spill to P1",
            i32::MAX,
            -100,
        ),
        ("control: room on P0's field", 20, 20),
    ] {
        let mut scenario = two_headed_giant();
        scenario.with_life(P0, p0);
        scenario.with_life(P1, p1);
        scenario.add_creature_from_oracle(P0, "Test Life Engine", 2, 2, SELF_LIFE_ENGINE);
        scenario.add_basic_land(P2, engine::types::mana::ManaColor::Red);
        scenario.add_bolt_to_hand(P2);
        let kickoff = scenario
            .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
            .id();
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        runner.cast(kickoff).resolve();

        // Drive the loop's beats until it is marked.
        for _ in 0..500 {
            if runner.state().unbounded_resources.contains_key(&P0) {
                break;
            }
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => {
                    runner
                        .act(GameAction::PassPriority)
                        .expect("passing priority is accepted");
                }
                WaitingFor::OrderTriggers { triggers, .. } => {
                    runner
                        .act(GameAction::OrderTriggers {
                            order: (0..triggers.len()).collect(),
                        })
                        .expect("ordering the triggers is accepted");
                }
                other => panic!("{name}: unexpected prompt while the loop runs: {other:?}"),
            }
        }

        let state = runner.state();
        assert!(
            state.players[0].life_gained_this_turn > 0
                && state.players[1].life_gained_this_turn == 0,
            "REACH-GUARD {name}: P0 gains and P1 does not, got ({}, {})",
            state.players[0].life_gained_this_turn,
            state.players[1].life_gained_this_turn
        );
        if p0 == i32::MAX {
            assert!(
                state.players[0].life == i32::MAX && state.players[1].life > p1,
                "REACH-GUARD {name}: the gains are stored on P1's field, got {:?}",
                team_a_fields(state)
            );
        }
        assert_eq!(
            state.unbounded_resources.get(&P0),
            Some(&std::collections::BTreeSet::from([ResourceAxis::Life(P0)])),
            "{name}: the mark names P0's life axis"
        );
    }
}
