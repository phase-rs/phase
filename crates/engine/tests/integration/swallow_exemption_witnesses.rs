//! Runtime witnesses for printed cards the swallow detector used to flag even
//! though they resolve as printed: Plea for Power and Path of the Ghosthunter
//! (threshold-vote conditionals), Goblin Traprunner, Mirror March and Crazed
//! Firecat (per-win coin-flip quantifier), Twice Upon a Time (a dynamic word in
//! the card's own name). Oracle text is verbatim from MTGJSON 5.3.0+20261115.

use engine::game::scenario::{GameRunner, P0};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameRng, RandomDraw, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use rand::Rng;

/// A printed mana cost: `shards` are the colored (or X) symbols, `generic` the
/// generic part.
fn printed_cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

fn mana_units(color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

/// Make the engine's next `outcomes.len()` coin flips come out as `outcomes`
/// (`true` = won) by searching for a seed whose first `RandomDraw::Outcome`
/// draws are that sequence, then restarting the state RNG on it. The same
/// mechanism as `coin_flip_branch_targets.rs`.
fn pin_outcomes(runner: &mut GameRunner, outcomes: &[bool]) {
    let seed = (0..100_000)
        .find(|&seed| {
            let mut rng = GameRng::seed_from_u64(seed);
            outcomes
                .iter()
                .all(|&won| rng.draw(RandomDraw::Outcome).random_bool(0.5) == won)
        })
        .expect("a seed in range produces the requested flip sequence");
    let state = runner.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
}

/// Every flip the events record, asserting P0 flipped it (each printed text
/// says "you" flip, so the controller of the ability is the flipper).
fn coin_flips(events: &[GameEvent]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|event| {
            if let GameEvent::CoinFlipped { player_id, won } = event {
                assert_eq!(*player_id, P0, "the controller flips the coin");
                Some(*won)
            } else {
                None
            }
        })
        .collect()
}

/// Pass priority (draining a trigger-ordering prompt) until the stack is
/// empty, returning every event produced. Any other prompt fails the test.
fn settle(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..64 {
        let waiting = runner.state().waiting_for.clone();
        if matches!(waiting, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
        } else if matches!(waiting, WaitingFor::Priority { .. }) {
            if runner.state().stack.is_empty() {
                return events;
            }
            events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes")
                    .events,
            );
        } else {
            panic!("resolution stuck at {waiting:?}");
        }
    }
    panic!("resolution did not settle");
}

mod votes {
    use super::{mana_units, printed_cost, settle};
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::types::ability::EffectKind;
    use engine::types::actions::GameAction;
    use engine::types::events::GameEvent;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCostShard, ManaType};
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    pub(super) const PLEA_FOR_POWER: &str = "Will of the council — Starting with you, each player votes for time or knowledge. If time gets more votes, take an extra turn after this one. If knowledge gets more votes or the vote is tied, draw three cards.";
    pub(super) const PATH_OF_THE_GHOSTHUNTER: &str = "Create X 1/1 white Spirit creature tokens with flying.\nWill of the Planeswalkers — Starting with you, each player votes for planeswalk or chaos. If planeswalk gets more votes, planeswalk. If chaos gets more votes or the vote is tied, chaos ensues.";

    /// Cast `spell` and stop at the controller's vote prompt, checking that the
    /// printed ballot is offered and the other seat votes next (CR 701.38a).
    fn cast_to_first_vote(
        runner: &mut GameRunner,
        spell: ObjectId,
        x: Option<u32>,
        ballot: [&str; 2],
    ) {
        let cast = runner.cast(spell);
        let outcome = match x {
            Some(x) => cast.x(x).resolve(),
            None => cast.resolve(),
        };
        let WaitingFor::VoteChoice {
            player,
            options,
            remaining_voters,
            ..
        } = outcome.final_waiting_for().clone()
        else {
            panic!(
                "the spell must present its vote, got {:?}",
                outcome.final_waiting_for()
            );
        };
        assert_eq!(
            player, P0,
            "'Starting with you': the controller votes first"
        );
        assert_eq!(options, ballot.map(String::from).to_vec());
        let waiting: Vec<PlayerId> = remaining_voters.iter().map(|(seat, _)| *seat).collect();
        assert_eq!(waiting, vec![P1]);
    }

    /// Submit the two votes in turn order and settle the stack, returning every
    /// event produced.
    fn vote_and_resolve(runner: &mut GameRunner, p0_vote: &str, p1_vote: &str) -> Vec<GameEvent> {
        let mut events = Vec::new();
        for (voter, choice) in [(P0, p0_vote), (P1, p1_vote)] {
            let WaitingFor::VoteChoice { player, .. } = runner.state().waiting_for.clone() else {
                panic!(
                    "expected {voter:?}'s vote, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(player, voter, "votes proceed in turn order");
            events.extend(
                runner
                    .act(GameAction::ChooseOption {
                        choice: choice.to_string(),
                    })
                    .expect("the vote is accepted")
                    .events,
            );
        }
        events.extend(settle(runner));
        events
    }

    fn plea_for_power() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, mana_units(ManaType::Blue, 4));
        let plea = scenario
            .add_spell_to_hand_from_oracle(P0, "Plea for Power", false, PLEA_FOR_POWER)
            .with_mana_cost(printed_cost(&[ManaCostShard::Blue], 3))
            .id();
        for _ in 0..6 {
            scenario.add_land_to_library_top(P0, "Plains");
        }
        (scenario.build(), plea)
    }

    fn path_of_the_ghosthunter() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, mana_units(ManaType::White, 4));
        let path = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Path of the Ghosthunter",
                false,
                PATH_OF_THE_GHOSTHUNTER,
            )
            .with_mana_cost(printed_cost(&[ManaCostShard::X, ManaCostShard::White], 1))
            .id();
        (scenario.build(), path)
    }

    fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
        runner.state().players[player.0 as usize].hand.len()
    }

    /// "If time gets more votes, take an extra turn after this one." Time wins
    /// 2-0: the controller gets the extra turn and the draw clause does not run.
    #[test]
    fn plea_for_power_time_wins_an_extra_turn_and_no_draw() {
        let (mut runner, plea) = plea_for_power();
        cast_to_first_vote(&mut runner, plea, None, ["time", "knowledge"]);
        vote_and_resolve(&mut runner, "time", "time");

        let extra_turns = &runner.state().extra_turns;
        assert_eq!(extra_turns.len(), 1, "exactly one extra turn");
        assert_eq!(extra_turns[0].player, P0);
        assert_eq!(
            extra_turns[0].anchor, P0,
            "CR 500.7: directly after this turn"
        );
        assert_eq!(hand_size(&runner, P0), 0, "the tie branch must not draw");
        assert_eq!(runner.state().objects[&plea].zone, Zone::Graveyard);
    }

    /// "If knowledge gets more votes ... draw three cards." Knowledge wins 2-0:
    /// three cards, no extra turn.
    #[test]
    fn plea_for_power_knowledge_wins_draws_three() {
        let (mut runner, plea) = plea_for_power();
        cast_to_first_vote(&mut runner, plea, None, ["time", "knowledge"]);
        vote_and_resolve(&mut runner, "knowledge", "knowledge");

        assert_eq!(hand_size(&runner, P0), 3);
        assert_eq!(hand_size(&runner, P1), 0, "only the controller draws");
        assert!(runner.state().extra_turns.is_empty());
        assert_eq!(runner.state().objects[&plea].zone, Zone::Graveyard);
    }

    /// "... or the vote is tied, draw three cards." A 1-1 tie takes the
    /// knowledge branch, not the extra turn.
    #[test]
    fn plea_for_power_tied_vote_draws_three() {
        let (mut runner, plea) = plea_for_power();
        cast_to_first_vote(&mut runner, plea, None, ["time", "knowledge"]);
        vote_and_resolve(&mut runner, "time", "knowledge");

        assert_eq!(hand_size(&runner, P0), 3);
        assert!(runner.state().extra_turns.is_empty());
    }

    /// "If planeswalk gets more votes, planeswalk." with X = 1 for the first
    /// line: one Spirit token and a planeswalk. No planar deck is staged, so the
    /// event's endpoints are empty but the event itself must fire.
    #[test]
    fn path_of_the_ghosthunter_planeswalk_wins() {
        let (mut runner, path) = path_of_the_ghosthunter();
        cast_to_first_vote(&mut runner, path, Some(1), ["planeswalk", "chaos"]);
        let events = vote_and_resolve(&mut runner, "planeswalk", "planeswalk");

        assert!(events
            .iter()
            .any(|event| matches!(event, GameEvent::Planeswalked { .. })));
        assert_eq!(runner.battlefield_count(P0), 1, "X = 1 makes one Spirit");
        assert_eq!(runner.state().objects[&path].zone, Zone::Graveyard);
    }

    /// "If chaos gets more votes or the vote is tied, chaos ensues." A 1-1 tie
    /// resolves the chaos branch and does not planeswalk (the planeswalk test
    /// above shows this instrument sees a planeswalk).
    #[test]
    fn path_of_the_ghosthunter_tied_vote_chaos_ensues() {
        let (mut runner, path) = path_of_the_ghosthunter();
        cast_to_first_vote(&mut runner, path, Some(0), ["planeswalk", "chaos"]);
        let events = vote_and_resolve(&mut runner, "planeswalk", "chaos");

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChaosEnsues,
                ..
            }
        )));
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::Planeswalked { .. })));
        assert_eq!(runner.battlefield_count(P0), 0, "X = 0 makes no Spirit");
        assert_eq!(runner.state().objects[&path].zone, Zone::Graveyard);
    }
}

mod coin_flip_wins {
    use super::{coin_flips, mana_units, pin_outcomes, printed_cost, settle};
    use engine::game::combat::AttackTarget;
    use engine::game::keywords::object_has_effective_keyword_kind;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::types::counter::CounterType;
    use engine::types::events::GameEvent;
    use engine::types::identifiers::ObjectId;
    use engine::types::keywords::KeywordKind;
    use engine::types::mana::{ManaCostShard, ManaType};
    use engine::types::phase::Phase;

    pub(super) const GOBLIN_TRAPRUNNER: &str = "Whenever this creature attacks, flip three coins. For each flip you win, create a 1/1 red Goblin creature token that's tapped and attacking.";
    pub(super) const MIRROR_MARCH: &str = "Whenever a nontoken creature you control enters, flip a coin until you lose a flip. For each flip you won, create a token that's a copy of that creature. Those tokens gain haste. Exile them at the beginning of the next end step.";
    pub(super) const CRAZED_FIRECAT: &str = "When this creature enters, flip a coin until you lose a flip. Put a +1/+1 counter on this creature for each flip you won.";

    /// Attack with the printed Goblin Traprunner, pin its three flips, and pass
    /// priority until the attack trigger has resolved.
    fn traprunner_attack(pinned: &[bool]) -> (GameRunner, Vec<GameEvent>) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let traprunner = scenario
            .add_creature(P0, "Goblin Traprunner", 4, 2)
            .with_subtypes(vec!["Goblin"])
            .with_mana_cost(printed_cost(&[ManaCostShard::Red], 3))
            .from_oracle_text(GOBLIN_TRAPRUNNER)
            .id();
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(traprunner, AttackTarget::Player(P1))])
            .expect("the Traprunner can attack");
        assert!(
            runner
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == traprunner),
            "declaring the attack queued the printed trigger"
        );
        pin_outcomes(&mut runner, pinned);

        let events = settle(&mut runner);
        (runner, events)
    }

    fn controlled_tokens(runner: &GameRunner) -> Vec<ObjectId> {
        let state = runner.state();
        state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token && object.controller == P0)
            .map(|object| object.id)
            .collect()
    }

    /// Win, lose, win: three flips, two won, so two 1/1 red Goblin tokens that
    /// enter tapped and attacking the defending player.
    #[test]
    fn goblin_traprunner_makes_a_token_per_won_flip() {
        let (runner, events) = traprunner_attack(&[true, false, true]);
        assert_eq!(coin_flips(&events), [true, false, true]);

        let tokens = controlled_tokens(&runner);
        assert_eq!(tokens.len(), 2);
        let combat = runner.state().combat.as_ref().expect("combat is live");
        for token in tokens {
            let object = &runner.state().objects[&token];
            assert_eq!((object.power, object.toughness), (Some(1), Some(1)));
            assert!(object.tapped, "'tapped and attacking'");
            assert!(object.card_types.subtypes.iter().any(|s| s == "Goblin"));
            let attacker = combat
                .attackers
                .iter()
                .find(|attacker| attacker.object_id == token)
                .expect("the token is attacking");
            assert_eq!(attacker.attack_target, AttackTarget::Player(P1));
        }
    }

    /// Three lost flips make no token; the trigger still ran (three flips).
    #[test]
    fn goblin_traprunner_makes_nothing_when_every_flip_is_lost() {
        let (runner, events) = traprunner_attack(&[false, false, false]);
        assert_eq!(coin_flips(&events), [false, false, false]);
        assert!(controlled_tokens(&runner).is_empty());
    }

    /// Three won flips make three tokens.
    #[test]
    fn goblin_traprunner_makes_three_tokens_when_every_flip_is_won() {
        let (runner, events) = traprunner_attack(&[true, true, true]);
        assert_eq!(coin_flips(&events), [true, true, true]);
        assert_eq!(controlled_tokens(&runner).len(), 3);
    }

    /// Cast the printed Grizzly Bears under the printed Mirror March with the
    /// flips pinned, and settle the entering trigger.
    fn bear_under_mirror_march(pinned: &[bool]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        scenario.with_mana_pool(P0, mana_units(ManaType::Green, 2));
        scenario
            .add_enchantment_from_oracle(P0, "Mirror March", MIRROR_MARCH)
            .with_mana_cost(printed_cost(&[ManaCostShard::Red], 5));
        let bear = scenario
            .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(printed_cost(&[ManaCostShard::Green], 1))
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, pinned);
        let mut events = runner.cast(bear).resolve().events().to_vec();
        events.extend(settle(&mut runner));
        // Reach guard: the printed trigger ran and flipped exactly the pinned run.
        assert_eq!(coin_flips(&events), pinned);
        let copies = controlled_tokens(&runner);
        (runner, bear, copies)
    }

    /// Win, win, lose: two token copies of the Bears, each with haste; both are
    /// exiled at the next end step and the original stays.
    #[test]
    fn mirror_march_copies_the_creature_once_per_won_flip() {
        let (mut runner, bear, copies) = bear_under_mirror_march(&[true, true, false]);
        assert_eq!(copies.len(), 2);
        for &copy in &copies {
            let object = &runner.state().objects[&copy];
            assert_eq!(object.name, "Grizzly Bears");
            assert_eq!((object.power, object.toughness), (Some(2), Some(2)));
            assert!(object_has_effective_keyword_kind(
                runner.state(),
                copy,
                KeywordKind::Haste
            ));
        }

        runner.advance_to_end_step();
        runner.advance_until_stack_empty();
        for copy in copies {
            assert!(
                !runner.state().battlefield.contains(&copy),
                "exiled at end step"
            );
        }
        assert!(
            runner.state().battlefield.contains(&bear),
            "the original stays"
        );
    }

    /// Losing the first flip ends the run with no copies.
    #[test]
    fn mirror_march_makes_no_copy_when_the_first_flip_is_lost() {
        let (runner, bear, copies) = bear_under_mirror_march(&[false]);
        assert!(copies.is_empty());
        assert!(runner.state().battlefield.contains(&bear));
    }

    /// Cast the printed Crazed Firecat with the flips pinned; the enters trigger
    /// puts a counter on it per won flip.
    fn firecat_counters(pinned: &[bool]) -> u32 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, mana_units(ManaType::Red, 7));
        let firecat = scenario
            .add_creature_to_hand_from_oracle(P0, "Crazed Firecat", 4, 4, CRAZED_FIRECAT)
            .with_subtypes(vec!["Elemental", "Cat"])
            .with_mana_cost(printed_cost(&[ManaCostShard::Red, ManaCostShard::Red], 5))
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, pinned);
        let mut events = runner.cast(firecat).resolve().events().to_vec();
        events.extend(settle(&mut runner));
        // Reach guard: the enters trigger flipped exactly the pinned run.
        assert_eq!(coin_flips(&events), pinned);
        let object = &runner.state().objects[&firecat];
        object
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    }

    /// Win, win, win, lose: three +1/+1 counters.
    #[test]
    fn crazed_firecat_gets_a_counter_per_won_flip() {
        assert_eq!(firecat_counters(&[true, true, true, false]), 3);
    }

    /// Losing the first flip ends the run with no counters.
    #[test]
    fn crazed_firecat_gets_nothing_when_the_first_flip_is_lost() {
        assert_eq!(firecat_counters(&[false]), 0);
    }
}

mod twice_upon_a_time {
    use super::{mana_units, printed_cost};
    use engine::game::scenario::{GameRunner, GameScenario, P0};
    use engine::types::ability::CastingRestriction;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCostShard, ManaType};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;

    const BEVERLY_CRUSHER: &str = "Seek out — Whenever you draw your second card each turn, put a +1/+1 counter on each of up to three target creatures.";
    const CHEKOV_AND_MCCOY: &str = "Whenever you put one or more +1/+1 counters on Chekov and McCoy, you may draw that many cards. Do this only once each turn.";
    pub(super) const TWICE_UPON_A_TIME: &str = "Cast this spell only if you control two or more Doctors.\nTake an extra turn after this one. Exile Twice Upon a Time.";

    /// P0 with `doctors` of the two printed Doctors on the battlefield and the
    /// printed Twice Upon a Time ({4}{U}{U}) in hand with its mana in the pool.
    fn board(doctors: usize) -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        if doctors >= 1 {
            scenario
                .add_creature_from_oracle(
                    P0,
                    "Beverly Crusher, Ship's Doctor",
                    2,
                    2,
                    BEVERLY_CRUSHER,
                )
                .with_mana_cost(printed_cost(&[ManaCostShard::White], 2))
                .as_legendary()
                .with_subtypes(vec!["Human", "Doctor"]);
        }
        if doctors >= 2 {
            scenario
                .add_creature_from_oracle(P0, "Chekov and McCoy", 3, 3, CHEKOV_AND_MCCOY)
                .with_mana_cost(printed_cost(
                    &[ManaCostShard::Green, ManaCostShard::Blue],
                    3,
                ))
                .as_legendary()
                .with_subtypes(vec!["Human", "Pilot", "Doctor"]);
        }
        let twice = scenario
            .add_spell_to_hand_from_oracle(P0, "Twice Upon a Time", false, TWICE_UPON_A_TIME)
            .with_mana_cost(printed_cost(&[ManaCostShard::Blue, ManaCostShard::Blue], 4))
            .id();
        let mut pool = mana_units(ManaType::Blue, 2);
        pool.extend(mana_units(ManaType::Colorless, 4));
        scenario.with_mana_pool(P0, pool);
        (scenario.build(), twice)
    }

    /// With two Doctors the cast is legal, the extra turn is queued after this
    /// one, and "Exile Twice Upon a Time" puts the resolving spell in exile.
    #[test]
    fn grants_an_extra_turn_and_exiles_itself_with_two_doctors() {
        let (mut runner, twice) = board(2);
        let outcome = runner.cast(twice).resolve();

        assert_eq!(outcome.zone_of(twice), Zone::Exile);
        let extra_turns = &outcome.state().extra_turns;
        assert_eq!(extra_turns.len(), 1);
        assert_eq!(extra_turns[0].player, P0);
        assert_eq!(extra_turns[0].anchor, P0);
        assert!(outcome.state().stack.is_empty());
    }

    /// With one Doctor "Cast this spell only if you control two or more
    /// Doctors" refuses the cast: the card stays in hand and no mana is spent.
    #[test]
    fn cannot_be_cast_with_fewer_than_two_doctors() {
        let (mut runner, twice) = board(1);
        assert!(
            runner.state().objects[&twice]
                .casting_restrictions
                .iter()
                .any(|restriction| matches!(
                    restriction,
                    CastingRestriction::RequiresCondition { condition: Some(_) }
                )),
            "the restriction parsed"
        );
        assert!(runner.cast(twice).try_resolve().is_err());
        assert_eq!(runner.state().objects[&twice].zone, Zone::Hand);
        assert!(runner.state().extra_turns.is_empty());
        assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 6);
    }
}

/// The printed cards the witnesses above run raise no swallowed-clause warning:
/// the detector's verdict matches what the gameplay shows.
mod parse_warnings {
    use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
    use engine::types::ability::Effect;
    use engine::types::ability_visit::visit_ability_def;
    use std::ops::ControlFlow;

    /// True when any effect in the parse's abilities or trigger bodies satisfies
    /// `wanted`.
    fn has_effect(parsed: &ParsedAbilities, wanted: impl Fn(&Effect) -> bool) -> bool {
        let roots = parsed
            .abilities
            .iter()
            .chain(parsed.triggers.iter().filter_map(|t| t.execute.as_deref()));
        let mut roots = roots;
        roots.any(|root| {
            visit_ability_def(root, &mut |effect| {
                if wanted(effect) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
            .is_break()
        })
    }

    /// Parse a printed card and assert it raises no warning. Reach guards: the
    /// parse holds the carrier the clause is about, and no `Unimplemented`
    /// effect (the audit skips such a unit, so it would report nothing either).
    fn assert_clean(
        name: &str,
        text: &str,
        printed: (&[&str], &[&str], &[&str]),
        carrier: impl Fn(&Effect) -> bool,
    ) {
        let (types, subtypes, keywords) = printed;
        let owned = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let parsed = parse_oracle_text(
            text,
            name,
            &owned(keywords),
            &owned(types),
            &owned(subtypes),
        );
        assert!(
            has_effect(&parsed, carrier),
            "{name}: expected carrier missing"
        );
        assert!(
            !has_effect(&parsed, |effect| matches!(
                effect,
                Effect::Unimplemented { .. }
            )),
            "{name}: parsed with an Unimplemented effect"
        );
        assert!(
            parsed.parse_warnings.is_empty(),
            "{name}: {:?}",
            parsed.parse_warnings
        );
    }

    fn is_vote(effect: &Effect) -> bool {
        matches!(
            effect,
            Effect::Vote {
                choices: _,
                per_choice_effect: _,
                starting_with: _,
                voter_scope: _,
                tally_mode: _,
                subject: _,
                visibility: _,
            }
        )
    }

    fn is_flip_with_win_effect(effect: &Effect) -> bool {
        matches!(
            effect,
            Effect::FlipCoins {
                flips: _,
                win_effect: Some(_),
                lose_effect: _,
                flipper: _,
            }
        )
    }

    #[test]
    fn plea_for_power() {
        assert_clean(
            "Plea for Power",
            super::votes::PLEA_FOR_POWER,
            (&["Sorcery"], &[], &["Will of the council"]),
            is_vote,
        );
    }

    #[test]
    fn path_of_the_ghosthunter() {
        assert_clean(
            "Path of the Ghosthunter",
            super::votes::PATH_OF_THE_GHOSTHUNTER,
            (&["Sorcery"], &[], &["Will of the Planeswalkers"]),
            is_vote,
        );
    }

    #[test]
    fn goblin_traprunner() {
        assert_clean(
            "Goblin Traprunner",
            super::coin_flip_wins::GOBLIN_TRAPRUNNER,
            (&["Creature"], &["Goblin"], &[]),
            is_flip_with_win_effect,
        );
    }

    #[test]
    fn mirror_march() {
        assert_clean(
            "Mirror March",
            super::coin_flip_wins::MIRROR_MARCH,
            (&["Enchantment"], &[], &[]),
            is_flip_with_win_effect,
        );
    }

    #[test]
    fn crazed_firecat() {
        assert_clean(
            "Crazed Firecat",
            super::coin_flip_wins::CRAZED_FIRECAT,
            (&["Creature"], &["Elemental", "Cat"], &[]),
            is_flip_with_win_effect,
        );
    }

    #[test]
    fn twice_upon_a_time() {
        assert_clean(
            "Twice Upon a Time",
            super::twice_upon_a_time::TWICE_UPON_A_TIME,
            (&["Sorcery"], &[], &[]),
            |effect| {
                matches!(
                    effect,
                    Effect::ExtraTurn {
                        target: _,
                        count: _,
                    }
                )
            },
        );
    }
}
