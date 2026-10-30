//! CR 119.3 + CR 107.1b: a life total stops at the `i32` limit a gain or loss
//! moves it toward, and the `LifeChanged` event and the per-turn gained/lost
//! counters report the rules amount, even when the stored total moved less.
//! Energy and player counters keep refusing a total past `u32::MAX`. A life
//! amount past `i32::MAX` is carried exactly, set/exchange/redistribute land
//! exactly across the whole range, and in Two-Headed Giant a change caps the
//! team's shared total (CR 810.4 + CR 810.9).

use engine::game::players::team_life_sum;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{EffectKind, QuantityModification, ReplacementDefinition};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, PayableResource, WaitingFor};
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
