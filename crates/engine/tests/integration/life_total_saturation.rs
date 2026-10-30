//! CR 119.3 + CR 107.1b: a life total stops at the `i32` limit a gain or loss
//! moves it toward, and the `LifeChanged` event and the per-turn gained/lost
//! counters report the rules amount, even when the stored total moved less.
//! Energy and player counters keep refusing a total past `u32::MAX`. A life
//! amount past `i32::MAX` is carried exactly, set/exchange/redistribute land
//! exactly across the whole range, and in Two-Headed Giant a change caps the
//! team's shared total (CR 810.4 + CR 810.9). A life replacement quantity past
//! `u32` lands on `u32::MAX`. A mandatory life-gain loop at the cap is drawn
//! (CR 104.4b + CR 732.4), as is a mandatory loop that drains a seat held at
//! `i32::MIN`; an optional one is never drawn or offered a shortcut; a
//! shortcut's lump gain past `i32::MAX` saturates; and a shortcut whose period
//! crosses the cap collapses at the rules amount.

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::analysis::resource::ResourceAxis;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::players::team_life_sum;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    Effect, EffectKind, QuantityModification, ReplacementDefinition, TapStateChange, TargetRef,
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
/// command on the same prefix reproduces both fields. A total outside `i32`
/// (two members at `i32::MAX`) is never moved past the limit by a gain and
/// moves exactly by a loss. A player who has left the game adjusts only its
/// own total.
#[test]
fn the_edit_authority_caps_a_shared_life_total_and_replays_its_spread() {
    let fields = |state: &GameState| -> Vec<i32> {
        state.players.iter().map(|player| player.life).collect()
    };

    // Cap: (MAX - 10, 5) + 100 -> (MAX - 5, 5), and its journal replay.
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 7);
    state.players[0].life = i32::MAX - 10;
    state.players[1].life = 5;
    let pre_state = state.clone();
    state
        .resolve_and_apply_player_edit(P0, ResolvedPlayerEdit::Life { delta: 100 })
        .expect("the gain caps the team total");
    assert_eq!(team_a_fields(&state), (i32::MAX - 5, 5));
    assert_eq!(team_life_sum(&state, P0), i64::from(i32::MAX));
    assert_eq!(state.players[0].life_gained_this_turn, 100);
    let command = state
        .resolved_rules_journal
        .entries()
        .iter()
        .find_map(|entry| match &entry.command {
            Some(ResolvedRulesCommand::PlayerEdit(command)) => Some(command.clone()),
            _ => None,
        })
        .expect("the capped edit is journaled");
    assert_eq!(command.edit, ResolvedPlayerEdit::Life { delta: 100 });
    let mut replay = pre_state;
    replay
        .apply_resolved_player_edit(&command)
        .expect("the recorded command replays");
    assert_eq!(fields(&replay), fields(&state), "the spread replays");
    assert_eq!(replay.players[0], state.players[0]);

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

    // A player who has left the game adjusts only its own total.
    let mut state = GameState::new_two_player(7);
    state.players[1].life = i32::MAX;
    state.players[1].is_eliminated = true;
    state
        .resolve_and_apply_player_edit(P1, ResolvedPlayerEdit::Life { delta: 5 })
        .expect("the departed player's gain is accepted");
    assert_eq!(state.players[1].life, i32::MAX);
    assert_eq!(state.players[1].life_gained_this_turn, 5);
    assert_eq!(state.players[0].life, 20);
}

/// Angel of Grace ("Your life total becomes 10.") in Two-Headed Giant sets
/// the team's total to 10 (CR 810.9c): P0 gains or loses the difference from
/// the team's exact total (CR 810.9a), which may lie outside `i32`, and the
/// part of a change P0's field cannot hold is stored on the teammate's.
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

/// The brief's self-feeding life-gain trigger (synthetic text, as in
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

/// Activates `source`'s ability `index`, then passes until the stack is empty
/// at priority or a loop shortcut is offered.
fn activate_and_settle(runner: &mut GameRunner, source: ObjectId, index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the activation is accepted");
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::LoopShortcut { .. } => return,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is accepted");
            }
            other => panic!("unexpected prompt while settling an activation: {other:?}"),
        }
    }
    panic!("the activation never settled");
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

/// The brief's mandatory loop in Interactive loop detection: P0 controls one
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

/// A P0 Lifedynamo (`lifedynamo(n)`) in Interactive loop detection, with the
/// given stored life fields and P0's gained-this-turn counter preset to
/// `gained`. Returns the runner, the Lifedynamo and its mana, life-gain and
/// untap ability indices, in activation order.
fn lifedynamo_board(
    mut scenario: GameScenario,
    life_fields: &[(PlayerId, i32)],
    n: u32,
    gained: u32,
) -> (GameRunner, ObjectId, [usize; 3]) {
    for &(player, life) in life_fields {
        scenario.with_life(player, life);
    }
    let dynamo = scenario
        .add_creature_from_oracle(P0, "Lifedynamo", 2, 2, &lifedynamo(n))
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state_mut().players[0].life_gained_this_turn = gained;
    let abilities = &runner.state().objects[&dynamo].abilities;
    let mana = abilities
        .iter()
        .position(is_mana_ability)
        .expect("reach guard: Lifedynamo's mana ability parses");
    let gain = abilities
        .iter()
        .position(|ability| matches!(&*ability.effect, Effect::GainLife { .. }))
        .expect("reach guard: Lifedynamo's life-gain ability parses");
    let untap = abilities
        .iter()
        .position(|ability| {
            matches!(
                &*ability.effect,
                Effect::SetTapState {
                    state: TapStateChange::Untap,
                    ..
                }
            )
        })
        .expect("reach guard: Lifedynamo's untap ability parses");
    (runner, dynamo, [mana, gain, untap])
}

/// Performs one Lifedynamo cycle (mana, gain, untap) and asserts the shortcut
/// offer that follows: P0's, with exactly `unbounded` as its unbounded axes.
fn cycle_to_the_shortcut_offer(
    runner: &mut GameRunner,
    dynamo: ObjectId,
    indices: [usize; 3],
    unbounded: &[ResourceAxis],
) {
    for index in indices {
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
/// responders are exactly `expected`, in order.
fn accept_for_every_responder(runner: &mut GameRunner, expected: &[PlayerId]) {
    for &responder in expected {
        let WaitingFor::RespondToShortcut { player, .. } = &runner.state().waiting_for else {
            panic!(
                "expected {responder:?} to respond to the shortcut, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, responder);
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("accepting the shortcut is accepted");
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "no further responder: {:?}",
        runner.state().waiting_for
    );
}

/// P0's stashed persistent-growth items.
fn stash(runner: &GameRunner) -> Option<&Vec<PersistentAxisMaterialization>> {
    runner.state().pending_unbounded_materialization.get(&P0)
}

/// Passes to the step boundary, asserts P0's `LoopCollapse` amount prompt
/// capped at `max`, and submits `max`; returns the submission's events.
fn collapse(runner: &mut GameRunner, max: u32) -> Vec<GameEvent> {
    pass_to_next_boundary(runner);
    let WaitingFor::PayAmountChoice {
        player,
        resource: PayableResource::LoopCollapse { .. },
        max: offered,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the loop-collapse amount prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*offered, max);
    runner
        .act(GameAction::SubmitPayAmount { amount: max })
        .expect("the collapse amount is accepted")
        .events
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

/// CR 104.4b + CR 732.4: the brief's mandatory loop (one "Whenever you gain
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

/// CR 119.3 + CR 732.1b: a shortcut of 1,000 Lifedynamo cycles at 3,000,000
/// life each gains 3,000,000,000 in one lump at the step boundary: the total
/// stops at `i32::MAX`, the gained counter counts the whole lump, and the event
/// reports the rules amount clamped to its field.
#[test]
fn a_life_shortcut_lump_gain_past_i32_max_saturates() {
    let (mut runner, dynamo, indices) = lifedynamo_board(two_player(), &[(P0, 20)], 3_000_000, 0);
    cycle_to_the_shortcut_offer(
        &mut runner,
        dynamo,
        indices,
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
    accept_for_every_responder(&mut runner, &[P1]);
    assert_eq!(
        stash(&runner),
        Some(&vec![PersistentAxisMaterialization::Life {
            player: P0,
            per_cycle_delta: 3_000_000,
        }])
    );

    let events = collapse(&mut runner, 1000);

    assert_eq!(runner.life(P0), i32::MAX);
    assert_eq!(
        runner.state().players[0].life_gained_this_turn,
        3_003_000_000
    );
    assert_eq!(life_changes(&events), vec![(P0, i32::MAX, Some(i32::MAX))]);
    assert!(is_priority(runner.state()));
}

/// CR 104.4b + CR 732.2a: at `i32::MAX` a Lifedynamo cycle's gain cannot grow
/// the stored total, so the shortcut offer carries no life axis and accepting
/// it stashes no life growth to collapse.
#[test]
fn a_life_shortcut_offer_at_the_cap_carries_no_life_axis() {
    let (mut runner, dynamo, indices) =
        lifedynamo_board(two_player(), &[(P0, i32::MAX)], 3_000_000, 0);
    cycle_to_the_shortcut_offer(
        &mut runner,
        dynamo,
        indices,
        &[ResourceAxis::Mana(ManaType::Colorless)],
    );
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1000),
            template: None,
        })
        .expect("the shortcut declaration is accepted");
    accept_for_every_responder(&mut runner, &[P1]);
    assert!(
        !stash(&runner).is_some_and(|items| items
            .iter()
            .any(|item| matches!(item, PersistentAxisMaterialization::Life { .. }))),
        "no life growth is stashed: {:?}",
        stash(&runner)
    );
    pass_to_next_boundary(&mut runner);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::PayAmountChoice { .. }
        ),
        "the boundary asks no collapse amount, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.life(P0), i32::MAX);
}

/// CR 119.3 + CR 732.1b: each of the 1000 elided cycles gains 2 life, so the
/// collapse gains 2000 and reports 2000 even though the stored total stops at
/// `i32::MAX`. The period is measured from one below the cap, where the cap
/// holds the stored gain to 1; in Two-Headed Giant the cap is the team's shared
/// total (CR 810.4). Control: from 22 the stored gain is the rules amount.
/// Residual: with the gained counter pinned at `u32::MAX` the period carries
/// no reading, so the collapse falls back to the stored gain of 1 per cycle
/// while the total still lands on `i32::MAX`.
#[test]
fn a_life_shortcut_period_crossing_the_cap_collapses_at_the_rules_amount() {
    struct Row {
        name: &'static str,
        two_headed_giant: bool,
        fields: Vec<(PlayerId, i32)>,
        gained: u32,
        unbounded: Vec<ResourceAxis>,
        p0_life_at_offer: i32,
        responders: Vec<PlayerId>,
        per_cycle_delta: u32,
        p0_life_after: i32,
        gained_after: u32,
        amount: i32,
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
            per_cycle_delta: 2,
            p0_life_after: i32::MAX,
            gained_after: 2002,
            amount: 2000,
        },
        Row {
            name: "Two-Headed Giant team crossing the cap",
            two_headed_giant: true,
            fields: vec![(P0, i32::MAX - 8), (P1, 5)],
            gained: 0,
            unbounded: vec![mana],
            p0_life_at_offer: i32::MAX - 6,
            responders: vec![P1, P2, PlayerId(3)],
            per_cycle_delta: 2,
            p0_life_after: i32::MAX - 5,
            gained_after: 2002,
            amount: 2000,
        },
        Row {
            name: "control below the cap",
            two_headed_giant: false,
            fields: vec![(P0, 20)],
            gained: 0,
            unbounded: vec![mana, ResourceAxis::Life(P0)],
            p0_life_at_offer: 22,
            responders: vec![P1],
            per_cycle_delta: 2,
            p0_life_after: 2022,
            gained_after: 2002,
            amount: 2000,
        },
        Row {
            name: "gained counter pinned at u32::MAX",
            two_headed_giant: false,
            fields: vec![(P0, i32::MAX - 3)],
            gained: u32::MAX,
            unbounded: vec![mana],
            p0_life_at_offer: i32::MAX - 1,
            responders: vec![P1],
            per_cycle_delta: 1,
            p0_life_after: i32::MAX,
            gained_after: u32::MAX,
            amount: 1000,
        },
    ];
    for row in rows {
        let name = row.name;
        let scenario = if row.two_headed_giant {
            two_headed_giant()
        } else {
            two_player()
        };
        let (mut runner, dynamo, indices) = lifedynamo_board(scenario, &row.fields, 2, row.gained);
        cycle_to_the_shortcut_offer(&mut runner, dynamo, indices, &row.unbounded);
        assert_eq!(runner.life(P0), row.p0_life_at_offer, "{name}");
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1000),
                template: None,
            })
            .expect("the shortcut declaration is accepted");
        accept_for_every_responder(&mut runner, &row.responders);
        assert_eq!(
            stash(&runner),
            Some(&vec![PersistentAxisMaterialization::Life {
                player: P0,
                per_cycle_delta: row.per_cycle_delta,
            }]),
            "{name}"
        );

        let events = collapse(&mut runner, 1000);

        assert_eq!(runner.life(P0), row.p0_life_after, "{name}");
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
        assert_eq!(
            life_changes(&events),
            vec![(P0, row.amount, Some(row.p0_life_after))],
            "{name}"
        );
        assert!(is_priority(runner.state()), "{name}");
        assert_eq!(stash(&runner), None, "{name}: the stash is cleared");
    }
}
