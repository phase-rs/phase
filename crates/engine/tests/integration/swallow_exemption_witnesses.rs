//! Runtime witnesses for printed cards the swallow detector used to flag even
//! though they resolve as printed: Plea for Power and the five Paths with the
//! "Will of the Planeswalkers" vote (Path of the Ghosthunter, Animist, Enigma,
//! Pyromancer and Schemer; threshold-vote conditionals) and Twice Upon a Time
//! (a dynamic word in the card's own name). Each card is loaded as printed from the committed
//! integration fixture (`add_real_card`, MTGJSON 5.3.0+20261115), so the witness
//! drives the parse the shipped card data carries, not text typed into the test.
//! The per-win coin-flip cards (Crazed Firecat, Mirror March, Goblin Traprunner)
//! are not here: their rewards still resolve per flip, so the detector keeps
//! reporting them (`per_win_coin_flip_quantifiers_stay_reported`).
//! The gameplay modules show that the printed cards resolve as printed; only
//! the `parse_warnings` module reads the detector's verdict.

use engine::database::CardDatabase;
use engine::game::scenario::GameRunner;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};

use crate::support::shared_card_db;

/// The committed integration fixture, which carries every printed card the
/// witnesses place. Failing loudly (not skipping) keeps a missing fixture from
/// turning the witnesses into vacuous passes.
fn printed_cards() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration fixture is present")
}

fn mana_units(color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
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
    use super::{mana_units, printed_cards, settle};
    use engine::game::planechase::active_plane;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::EffectKind;
    use engine::types::actions::GameAction;
    use engine::types::events::GameEvent;
    use engine::types::format::FormatConfig;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    pub(super) const PLEA_FOR_POWER: &str = "Will of the council — Starting with you, each player votes for time or knowledge. If time gets more votes, take an extra turn after this one. If knowledge gets more votes or the vote is tied, draw three cards.";
    pub(super) const PATH_OF_THE_GHOSTHUNTER: &str = "Create X 1/1 white Spirit creature tokens with flying.\nWill of the Planeswalkers — Starting with you, each player votes for planeswalk or chaos. If planeswalk gets more votes, planeswalk. If chaos gets more votes or the vote is tied, chaos ensues.";
    pub(super) const PATH_OF_THE_ANIMIST: &str = "Search your library for up to two basic land cards, put them onto the battlefield tapped, then shuffle.\nWill of the Planeswalkers — Starting with you, each player votes for planeswalk or chaos. If planeswalk gets more votes, planeswalk. If chaos gets more votes or the vote is tied, chaos ensues.";
    pub(super) const PATH_OF_THE_ENIGMA: &str = "Target player draws four cards.\nWill of the Planeswalkers — Starting with you, each player votes for planeswalk or chaos. If planeswalk gets more votes, planeswalk. If chaos gets more votes or the vote is tied, chaos ensues.";
    pub(super) const PATH_OF_THE_PYROMANCER: &str = "Discard all the cards in your hand. Add {R} for each card discarded this way, then draw that many cards plus one.\nWill of the Planeswalkers — Starting with you, each player votes for planeswalk or chaos. If planeswalk gets more votes, planeswalk. If chaos gets more votes or the vote is tied, chaos ensues.";
    pub(super) const PATH_OF_THE_SCHEMER: &str = "Each player mills two cards. Then you put a creature card from a graveyard onto the battlefield under your control. It's an artifact in addition to its other types.\nWill of the Planeswalkers — Starting with you, each player votes for planeswalk or chaos. If planeswalk gets more votes, planeswalk. If chaos gets more votes or the vote is tied, chaos ensues.";

    /// Cast `spell` and stop at the controller's vote prompt, checking that the
    /// printed ballot is offered and the other seat votes next (CR 701.38a).
    pub(super) fn cast_to_first_vote(
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
    pub(super) fn vote_and_resolve(
        runner: &mut GameRunner,
        p0_vote: &str,
        p1_vote: &str,
    ) -> Vec<GameEvent> {
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
        let plea = scenario.add_real_card(P0, "Plea for Power", Zone::Hand, printed_cards());
        for _ in 0..6 {
            scenario.add_land_to_library_top(P0, "Plains");
        }
        (scenario.build(), plea)
    }

    /// The printed planes the Path witnesses stage.
    pub(super) struct Planes {
        /// The Dark Barony, the face-up active plane.
        pub(super) active: ObjectId,
        /// Naktamun, on top of the planar deck.
        pub(super) next: ObjectId,
    }

    /// A two-seat Planechase game with P0's mana pool filled with
    /// `mana`, the printed `card` in P0's hand, the printed The Dark Barony as
    /// the plane to face up ("Whenever chaos ensues, each opponent discards a
    /// card"), the printed Naktamun for the top of the planar deck, and a
    /// printed Grizzly Bears in the opponent's hand for that discard. Seat the
    /// planes with `seat_planes` after `build`.
    pub(super) fn planechase_scenario(
        card: &str,
        mana: Vec<ManaUnit>,
    ) -> (GameScenario, ObjectId, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new_with_format(FormatConfig::planechase(), 2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, mana);
        let spell = scenario.add_real_card(P0, card, Zone::Hand, printed_cards());
        let active = scenario.add_real_card(P0, "The Dark Barony", Zone::Command, printed_cards());
        let next = scenario.add_real_card(P0, "Naktamun", Zone::Command, printed_cards());
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Hand, printed_cards());
        (scenario, spell, active, next)
    }

    /// Put The Dark Barony face up as the active plane and Naktamun face down
    /// on top of the planar deck, with P0 as the planar controller.
    pub(super) fn seat_planes(runner: &mut GameRunner, active: ObjectId, next: ObjectId) -> Planes {
        let state = runner.state_mut();
        state.command_zone.retain(|id| *id != next);
        state.planar_deck.push_back(next);
        state
            .objects
            .get_mut(&next)
            .expect("the staged plane exists")
            .face_down = true;
        state.planar_controller = Some(P0);
        // Reach guard: the face-up plane is The Dark Barony and Naktamun waits
        // on top of the planar deck.
        assert_eq!(active_plane(state), Some(active));
        assert_eq!(state.planar_deck.front(), Some(&next));
        Planes { active, next }
    }

    fn path_of_the_ghosthunter() -> (GameRunner, ObjectId, Planes) {
        let (scenario, path, active, next) =
            planechase_scenario("Path of the Ghosthunter", mana_units(ManaType::White, 4));
        let mut runner = scenario.build();
        let planes = seat_planes(&mut runner, active, next);
        (runner, path, planes)
    }

    pub(super) fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
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
    /// line: one Spirit token and a planeswalk from The Dark Barony to Naktamun
    /// (CR 701.31b): the departing plane goes to the bottom of the planar deck,
    /// Naktamun is the active plane, and chaos does not ensue.
    #[test]
    fn path_of_the_ghosthunter_planeswalk_wins() {
        let (mut runner, path, planes) = path_of_the_ghosthunter();
        cast_to_first_vote(&mut runner, path, Some(1), ["planeswalk", "chaos"]);
        let events = vote_and_resolve(&mut runner, "planeswalk", "planeswalk");

        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::Planeswalked {
                    player_id,
                    from: Some(from),
                    to: Some(to),
                } if *player_id == P0 && *from == planes.active && *to == planes.next
            )),
            "planeswalked from The Dark Barony to Naktamun: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::ChaosEnsued { .. })));
        assert_eq!(active_plane(runner.state()), Some(planes.next));
        assert_eq!(
            runner.state().planar_deck.back(),
            Some(&planes.active),
            "the departing plane goes to the bottom of the planar deck"
        );
        assert_eq!(hand_size(&runner, P1), 1, "chaos did not ensue: no discard");
        assert_eq!(runner.battlefield_count(P0), 1, "X = 1 makes one Spirit");
        assert_eq!(runner.state().objects[&path].zone, Zone::Graveyard);
    }

    /// "If chaos gets more votes or the vote is tied, chaos ensues." A 1-1 tie
    /// resolves the chaos branch for the active plane (CR 311.7): chaos ensues
    /// for The Dark Barony, whose chaos ability makes the opponent discard,
    /// and nobody planeswalks (the planeswalk test above shows this instrument
    /// sees a planeswalk).
    #[test]
    fn path_of_the_ghosthunter_tied_vote_chaos_ensues() {
        let (mut runner, path, planes) = path_of_the_ghosthunter();
        cast_to_first_vote(&mut runner, path, Some(0), ["planeswalk", "chaos"]);
        let events = vote_and_resolve(&mut runner, "planeswalk", "chaos");

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChaosEnsues,
                ..
            }
        )));
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::ChaosEnsued { plane_id } if *plane_id == planes.active
            )),
            "chaos ensued for the active plane: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::Planeswalked { .. })));
        assert_eq!(active_plane(runner.state()), Some(planes.active));
        assert_eq!(
            runner.state().planar_deck.front(),
            Some(&planes.next),
            "the planar deck is untouched"
        );
        assert_eq!(
            hand_size(&runner, P1),
            0,
            "The Dark Barony's chaos ability made the opponent discard"
        );
        assert_eq!(runner.battlefield_count(P0), 0, "X = 0 makes no Spirit");
        assert_eq!(runner.state().objects[&path].zone, Zone::Graveyard);
    }
}

mod twice_upon_a_time {
    use super::{mana_units, printed_cards};
    use engine::game::scenario::{GameRunner, GameScenario, P0};
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::CastingRestriction;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::ManaType;
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;

    pub(super) const TWICE_UPON_A_TIME: &str = "Cast this spell only if you control two or more Doctors.\nTake an extra turn after this one. Exile Twice Upon a Time.";

    /// P0 with `doctors` of the two printed Doctors on the battlefield and the
    /// printed Twice Upon a Time ({4}{U}{U}) in hand with its mana in the pool.
    fn board(doctors: usize) -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for doctor in ["Beverly Crusher, Ship's Doctor", "Chekov and McCoy"]
            .into_iter()
            .take(doctors)
        {
            scenario.add_real_card(P0, doctor, Zone::Battlefield, printed_cards());
        }
        let twice = scenario.add_real_card(P0, "Twice Upon a Time", Zone::Hand, printed_cards());
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
/// the detector's verdict matches what the gameplay shows. These parse the
/// printed text with the current parser (not the fixture's stored parse), so
/// they are the tests that fail when the detector change is reverted; the
/// gameplay modules above resolve the same cards and do not depend on it.
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
        let face = super::printed_cards()
            .get_face_by_name(name)
            .unwrap_or_else(|| panic!("{name} is in the integration fixture"));
        assert_eq!(
            face.oracle_text.as_deref(),
            Some(text),
            "{name}: the text parsed here is the printed text the gameplay modules load"
        );
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
    fn path_of_the_animist() {
        assert_clean(
            "Path of the Animist",
            super::votes::PATH_OF_THE_ANIMIST,
            (&["Sorcery"], &[], &["Will of the Planeswalkers"]),
            is_vote,
        );
    }

    #[test]
    fn path_of_the_enigma() {
        assert_clean(
            "Path of the Enigma",
            super::votes::PATH_OF_THE_ENIGMA,
            (&["Sorcery"], &[], &["Will of the Planeswalkers"]),
            is_vote,
        );
    }

    #[test]
    fn path_of_the_pyromancer() {
        assert_clean(
            "Path of the Pyromancer",
            super::votes::PATH_OF_THE_PYROMANCER,
            (&["Sorcery"], &[], &["Will of the Planeswalkers"]),
            is_vote,
        );
    }

    #[test]
    fn path_of_the_schemer() {
        assert_clean(
            "Path of the Schemer",
            super::votes::PATH_OF_THE_SCHEMER,
            (&["Sorcery"], &[], &["Mill", "Will of the Planeswalkers"]),
            is_vote,
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

// The four Paths below (Animist, Enigma, Pyromancer, Schemer) are loaded as
// printed from the integration fixture and staged in a two-seat Planechase
// game, so each card's first line and the shared "Will of the Planeswalkers"
// line run on both vote branches through GameRunner and public GameActions.

mod path_of_the_animist {
    use super::votes::{hand_size, planechase_scenario, seat_planes, vote_and_resolve, Planes};
    use super::{mana_units, printed_cards, settle};
    use engine::game::planechase::active_plane;
    use engine::game::scenario::{GameRunner, P0, P1};
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::EffectKind;
    use engine::types::actions::GameAction;
    use engine::types::events::{GameEvent, PlayerActionKind};
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::ManaType;
    use engine::types::zones::Zone;

    /// The staged board: P0's library holds four printed basic lands of two
    /// types, one printed nonbasic land (Adarkar Wastes) and the printed vanilla
    /// Grizzly Bears as a non-land filler.
    struct AnimistBoard {
        runner: GameRunner,
        path: ObjectId,
        planes: Planes,
        forest_a: ObjectId,
        forest_b: ObjectId,
        island_a: ObjectId,
        island_b: ObjectId,
        bears: ObjectId,
        /// A printed nonbasic land: "basic land cards" must not offer it.
        nonbasic: ObjectId,
    }

    fn animist_board() -> AnimistBoard {
        let (mut scenario, path, active, next) =
            planechase_scenario("Path of the Animist", mana_units(ManaType::Green, 4));
        let forest_a = scenario.add_real_card(P0, "Forest", Zone::Library, printed_cards());
        let forest_b = scenario.add_real_card(P0, "Forest", Zone::Library, printed_cards());
        let island_a = scenario.add_real_card(P0, "Island", Zone::Library, printed_cards());
        let island_b = scenario.add_real_card(P0, "Island", Zone::Library, printed_cards());
        let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, printed_cards());
        let nonbasic = scenario.add_real_card(P0, "Adarkar Wastes", Zone::Library, printed_cards());
        let mut runner = scenario.build();
        let planes = seat_planes(&mut runner, active, next);
        AnimistBoard {
            nonbasic,
            runner,
            path,
            planes,
            forest_a,
            forest_b,
            island_a,
            island_b,
            bears,
        }
    }

    impl AnimistBoard {
        /// Cast the printed Path of the Animist ({3}{G}) and drive its first
        /// line to the vote prompt: assert the search prompt's actor, exact
        /// option ids, and count, pick a Forest and an Island, assert both
        /// enter the battlefield tapped under P0 and the library was
        /// shuffled, then assert the vote prompt's actor, options, and
        /// remaining voters. Returns the events so far.
        fn cast_search_and_park_at_vote(&mut self) -> Vec<GameEvent> {
            let outcome = self.runner.cast(self.path).resolve();
            let WaitingFor::SearchChoice {
                player,
                cards,
                count,
                up_to,
                ..
            } = outcome.final_waiting_for().clone()
            else {
                panic!(
                    "Path of the Animist must park at its library search, got {:?}",
                    outcome.final_waiting_for()
                );
            };
            assert_eq!(
                self.runner.state().players[P0.0 as usize].mana_pool.total(),
                0,
                "the pool held exactly the four mana of {{3}}{{G}}, all spent"
            );
            // CR 701.23a + CR 701.23b: the controller searches their own library
            // and need not find every card ("up to two").
            assert_eq!(player, P0, "the controller searches their own library");
            assert_eq!(count, 2, "'up to two basic land cards'");
            assert!(up_to, "the searcher may find fewer than two");
            // Hostile controls: the exact option set is the four printed basic
            // lands; neither the printed non-land filler (Grizzly Bears) nor
            // the printed nonbasic land (Adarkar Wastes) is offered.
            let mut offered = cards;
            let mut expected = vec![self.forest_a, self.forest_b, self.island_a, self.island_b];
            offered.sort();
            expected.sort();
            assert_eq!(
                offered, expected,
                "only the library's printed basic lands are offered, not the Bears or the nonbasic land"
            );

            let events = self
                .runner
                .act(GameAction::SelectCards {
                    cards: vec![self.forest_a, self.island_a],
                })
                .expect("the search selection continues the resolution")
                .events;

            // First-line outcome: both chosen lands are on the battlefield
            // under P0 and tapped.
            for land in [self.forest_a, self.island_a] {
                assert_eq!(self.runner.state().objects[&land].zone, Zone::Battlefield);
                assert_eq!(
                    self.runner.state().objects[&land].controller,
                    P0,
                    "the found lands enter under the searcher's control"
                );
                assert!(
                    self.runner.state().objects[&land].tapped,
                    "the found lands enter the battlefield tapped"
                );
            }
            // CR 701.24a: "then shuffle" shuffled the controller's library.
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        player_id,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    } if *player_id == P0
                )),
                "'then shuffle' shuffled P0's library: {events:?}"
            );
            // Hostile controls: the filler and the two unchosen lands stay in
            // the library.
            for card in [self.bears, self.nonbasic, self.forest_b, self.island_b] {
                assert_eq!(self.runner.state().objects[&card].zone, Zone::Library);
            }

            // Second line: the vote prompt, with the controller voting first.
            let WaitingFor::VoteChoice {
                player,
                options,
                remaining_voters,
                ..
            } = self.runner.state().waiting_for.clone()
            else {
                panic!(
                    "the Will of the Planeswalkers vote must follow the search, got {:?}",
                    self.runner.state().waiting_for
                );
            };
            assert_eq!(
                player, P0,
                "'Starting with you': the controller votes first (CR 701.38a)"
            );
            assert_eq!(options, ["planeswalk", "chaos"].map(String::from).to_vec());
            let waiting: Vec<_> = remaining_voters.iter().map(|(seat, _)| *seat).collect();
            assert_eq!(waiting, vec![P1], "the other seat votes next");
            events
        }
    }

    /// "Search your library for up to two basic land cards, put them onto the
    /// battlefield tapped, then shuffle." + "If planeswalk gets more votes,
    /// planeswalk." Planeswalk wins 2-0: the search (CR 701.23a) offers only
    /// the library's four printed basic lands, the chosen Forest and Island
    /// enter the battlefield tapped under P0, the library is shuffled (CR
    /// 701.24a) with the Bears filler left inside, and the game planeswalks
    /// from The Dark Barony to Naktamun (CR 701.31b).
    #[test]
    fn path_of_the_animist_planeswalk_wins() {
        let mut board = animist_board();
        let mut events = board.cast_search_and_park_at_vote();
        events.extend(vote_and_resolve(
            &mut board.runner,
            "planeswalk",
            "planeswalk",
        ));

        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::Planeswalked {
                    player_id,
                    from: Some(from),
                    to: Some(to),
                } if *player_id == P0 && *from == board.planes.active && *to == board.planes.next
            )),
            "planeswalked from The Dark Barony to Naktamun: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::ChaosEnsued { .. })));
        assert_eq!(active_plane(board.runner.state()), Some(board.planes.next));
        assert_eq!(
            board.runner.state().planar_deck.back(),
            Some(&board.planes.active),
            "the departing plane goes to the bottom of the planar deck (CR 701.31b)"
        );
        assert_eq!(
            hand_size(&board.runner, P1),
            1,
            "chaos did not ensue: the opponent keeps the Bears"
        );
        assert_eq!(
            board.runner.battlefield_count(P0),
            2,
            "the two found lands are P0's only battlefield permanents"
        );
        assert_eq!(
            board.runner.state().objects[&board.path].zone,
            Zone::Graveyard
        );

        settle(&mut board.runner);
        assert!(board.runner.state().stack.is_empty());
        assert!(matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }

    /// "If chaos gets more votes or the vote is tied, chaos ensues." A 1-1 tie
    /// resolves the chaos branch for the active plane (CR 311.7): chaos
    /// ensues for The Dark Barony, whose chaos ability makes the opponent
    /// discard the Bears, and nobody planeswalks. The first line still
    /// resolved in the same test: both found lands entered the battlefield
    /// tapped and the library was shuffled with the filler inside.
    #[test]
    fn path_of_the_animist_tied_vote_chaos_ensues() {
        let mut board = animist_board();
        let mut events = board.cast_search_and_park_at_vote();
        events.extend(vote_and_resolve(&mut board.runner, "planeswalk", "chaos"));

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChaosEnsues,
                ..
            }
        )));
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::ChaosEnsued { plane_id } if *plane_id == board.planes.active
            )),
            "chaos ensued for the active plane: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::Planeswalked { .. })));
        assert_eq!(
            active_plane(board.runner.state()),
            Some(board.planes.active)
        );
        assert_eq!(
            board.runner.state().planar_deck.front(),
            Some(&board.planes.next),
            "the planar deck is untouched"
        );
        assert_eq!(
            hand_size(&board.runner, P1),
            0,
            "The Dark Barony's chaos ability made the opponent discard the Bears"
        );
        assert_eq!(
            board.runner.battlefield_count(P0),
            2,
            "the two found lands stay on the battlefield"
        );
        assert_eq!(
            board.runner.state().objects[&board.path].zone,
            Zone::Graveyard
        );

        settle(&mut board.runner);
        assert!(board.runner.state().stack.is_empty());
        assert!(matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

mod path_of_the_enigma {
    use super::votes::{hand_size, planechase_scenario, seat_planes, vote_and_resolve, Planes};
    use super::{mana_units, printed_cards, settle};
    use engine::game::planechase::active_plane;
    use engine::game::scenario::{GameRunner, P0, P1};
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::game::triggers::drain_order_triggers_with_identity;
    use engine::types::ability::{EffectKind, TargetRef};
    use engine::types::actions::GameAction;
    use engine::types::events::GameEvent;
    use engine::types::game_state::{CastPaymentMode, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::ManaType;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    /// The staged board: P1's library holds six printed Islands so the
    /// targeted four-card draw cannot deck them.
    struct EnigmaBoard {
        runner: GameRunner,
        enigma: ObjectId,
        planes: Planes,
        bears: ObjectId,
    }

    fn enigma_board() -> EnigmaBoard {
        let (mut scenario, enigma, active, next) =
            planechase_scenario("Path of the Enigma", mana_units(ManaType::Blue, 5));
        for _ in 0..6 {
            scenario.add_real_card(P1, "Island", Zone::Library, printed_cards());
        }
        let mut runner = scenario.build();
        let planes = seat_planes(&mut runner, active, next);
        let bears = p1_bears(&runner);
        assert!(
            p1_library_len(&runner) >= 5,
            "P1's library must hold at least five cards before the draw"
        );
        EnigmaBoard {
            runner,
            enigma,
            planes,
            bears,
        }
    }

    fn p1_bears(runner: &GameRunner) -> ObjectId {
        runner.state().players[P1.0 as usize]
            .hand
            .iter()
            .copied()
            .find(|id| runner.state().objects[id].name == "Grizzly Bears")
            .expect("the printed Grizzly Bears sits in P1's hand")
    }

    fn p1_library_len(runner: &GameRunner) -> usize {
        runner.state().players[P1.0 as usize].library.len()
    }

    /// CR 121.1: count this player's settled draw events.
    fn drawn_by(events: &[GameEvent], player: PlayerId) -> u32 {
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == player))
            .count() as u32
    }

    /// Drive the post-vote resolution to the empty stack, answering The Dark
    /// Barony's "Whenever chaos ensues, each opponent discards a card" chaos
    /// trigger (CR 311.7): the engine parks a DiscardChoice for P1's
    /// five-card hand, whose exact option set is the printed Bears plus the
    /// four drawn Islands, and P1 discards one drawn Island, keeping the
    /// Bears.
    fn settle_through_the_barony_discard(
        runner: &mut GameRunner,
        bears: ObjectId,
    ) -> Vec<GameEvent> {
        let mut events = Vec::new();
        for _ in 0..64 {
            let waiting = runner.state().waiting_for.clone();
            if matches!(waiting, WaitingFor::OrderTriggers { .. }) {
                drain_order_triggers_with_identity(runner.state_mut());
            } else if let WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } = waiting
            {
                assert_eq!(player, P1, "the Barony chaos makes the opponent discard");
                assert_eq!(count, 1, "each opponent discards one card");
                let mut offered = cards.clone();
                let mut hand: Vec<ObjectId> = runner.state().players[P1.0 as usize]
                    .hand
                    .iter()
                    .copied()
                    .collect();
                offered.sort();
                hand.sort();
                assert_eq!(hand.len(), 5, "the Bears plus the four drawn Islands");
                assert_eq!(offered, hand, "the exact option set is P1's whole hand");
                assert!(
                    cards.contains(&bears),
                    "the printed Grizzly Bears is among the offered options"
                );
                let island = cards
                    .iter()
                    .copied()
                    .find(|id| *id != bears)
                    .expect("P1 holds four drawn Islands besides the Bears");
                events.extend(
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![island],
                        })
                        .expect("P1's discard is accepted")
                        .events,
                );
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

    impl EnigmaBoard {
        /// Announce the printed Path of the Enigma ({4}{U}) through the
        /// production cast pipeline: assert the offered target set is exactly
        /// both players (CR 601.2c + CR 115.1), target the opponent, commit,
        /// let both players pass so the spell resolves, assert the opponent
        /// drew exactly four (CR 121.1), then assert the vote prompt's actor,
        /// options, and remaining voters. Returns the events so far.
        fn cast_targeting_opponent_and_park_at_vote(&mut self) -> Vec<GameEvent> {
            let card_id = self.runner.state().objects[&self.enigma].card_id;
            let mut events = self
                .runner
                .act(GameAction::CastSpell {
                    object_id: self.enigma,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("announcing Path of the Enigma starts the cast pipeline")
                .events;

            // CR 601.2c + CR 115.1: the sole cast-time target is "Target
            // player", and both seats are legal.
            let WaitingFor::TargetSelection {
                player,
                target_slots,
                ..
            } = self.runner.state().waiting_for.clone()
            else {
                panic!(
                    "Path of the Enigma must ask for its target player, got {:?}",
                    self.runner.state().waiting_for
                );
            };
            assert_eq!(player, P0, "the caster chooses the target");
            assert_eq!(
                target_slots.len(),
                1,
                "'Target player draws four cards' is one target slot"
            );
            let offered = &target_slots[0].legal_targets;
            assert_eq!(offered.len(), 2, "both players are offered: {offered:?}");
            assert!(
                offered.contains(&TargetRef::Player(P0))
                    && offered.contains(&TargetRef::Player(P1)),
                "the offered target set is exactly both players: {offered:?}"
            );

            events.extend(
                self.runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("targeting the opponent is accepted")
                    .events,
            );

            // CR 601.2g-h: the prefilled pool auto-pays {4}{U}; if the payment
            // window still opens, a pass finalizes from the pool.
            if matches!(
                self.runner.state().waiting_for,
                WaitingFor::ManaPayment { .. }
            ) {
                events.extend(
                    self.runner
                        .act(GameAction::PassPriority)
                        .expect("the pool-funded payment finalizes")
                        .events,
                );
            }
            let WaitingFor::Priority { player } = self.runner.state().waiting_for.clone() else {
                panic!(
                    "Path of the Enigma must commit to the stack, got {:?}",
                    self.runner.state().waiting_for
                );
            };
            assert_eq!(player, P0);
            assert_eq!(
                self.runner.state().stack.len(),
                1,
                "the sorcery is the only stack object"
            );

            // Both players pass: the spell resolves and the vote prompt parks.
            events.extend(
                self.runner
                    .act(GameAction::PassPriority)
                    .expect("P0 passes priority")
                    .events,
            );
            events.extend(
                self.runner
                    .act(GameAction::PassPriority)
                    .expect("P1 passes priority and the spell resolves")
                    .events,
            );

            // First-line outcome: the chosen target — the opponent — drew
            // exactly four (CR 121.1); the controller drew none.
            assert_eq!(
                drawn_by(&events, P1),
                4,
                "the targeted opponent drew exactly four"
            );
            assert_eq!(drawn_by(&events, P0), 0, "the controller drew none");

            // Second line: the vote prompt, with the controller voting first.
            let WaitingFor::VoteChoice {
                player,
                options,
                remaining_voters,
                ..
            } = self.runner.state().waiting_for.clone()
            else {
                panic!(
                    "the Will of the Planeswalkers vote must follow the draw, got {:?}",
                    self.runner.state().waiting_for
                );
            };
            assert_eq!(
                player, P0,
                "'Starting with you': the controller votes first (CR 701.38a)"
            );
            assert_eq!(options, ["planeswalk", "chaos"].map(String::from).to_vec());
            let waiting: Vec<_> = remaining_voters.iter().map(|(seat, _)| *seat).collect();
            assert_eq!(waiting, vec![P1], "the other seat votes next");
            events
        }
    }

    /// "Target player draws four cards." + "If planeswalk gets more votes,
    /// planeswalk." Cast at the opponent so the target choice matters: the
    /// offered target set is exactly both players (CR 601.2c + CR 115.1), P1
    /// draws exactly four and P0 none (CR 121.1), and with planeswalk winning
    /// 2-0 the game planeswalks from The Dark Barony to Naktamun (CR 701.31b).
    #[test]
    fn path_of_the_enigma_planeswalk_wins() {
        let mut board = enigma_board();
        let mut events = board.cast_targeting_opponent_and_park_at_vote();

        assert_eq!(
            hand_size(&board.runner, P1),
            5,
            "the Bears plus the four drawn cards"
        );
        assert_eq!(
            hand_size(&board.runner, P0),
            0,
            "the controller's hand is empty"
        );
        assert_eq!(
            p1_library_len(&board.runner),
            2,
            "six Islands minus the four drawn"
        );

        events.extend(vote_and_resolve(
            &mut board.runner,
            "planeswalk",
            "planeswalk",
        ));

        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::Planeswalked {
                    player_id,
                    from: Some(from),
                    to: Some(to),
                } if *player_id == P0 && *from == board.planes.active && *to == board.planes.next
            )),
            "planeswalked from The Dark Barony to Naktamun: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::ChaosEnsued { .. })));
        assert_eq!(active_plane(board.runner.state()), Some(board.planes.next));
        assert_eq!(
            board.runner.state().planar_deck.back(),
            Some(&board.planes.active),
            "the departing plane goes to the bottom of the planar deck (CR 701.31b)"
        );
        assert_eq!(
            hand_size(&board.runner, P1),
            5,
            "chaos did not ensue: no discard, P1 keeps all five cards"
        );
        assert_eq!(
            board.runner.state().objects[&board.enigma].zone,
            Zone::Graveyard
        );

        settle(&mut board.runner);
        assert!(board.runner.state().stack.is_empty());
        assert!(matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }

    /// "If chaos gets more votes or the vote is tied, chaos ensues." A 1-1 tie
    /// resolves the chaos branch for the active plane (CR 311.7): chaos ensues
    /// for The Dark Barony, whose chaos trigger makes the opponent discard
    /// one of the drawn Islands (keeping the printed Bears), and nobody
    /// planeswalks. The first line still resolved in the same test: the
    /// targeted opponent drew exactly four and P0 none.
    #[test]
    fn path_of_the_enigma_tied_vote_chaos_ensues() {
        let mut board = enigma_board();
        let mut events = board.cast_targeting_opponent_and_park_at_vote();

        assert_eq!(
            hand_size(&board.runner, P1),
            5,
            "the Bears plus the four drawn cards"
        );
        assert_eq!(
            p1_library_len(&board.runner),
            2,
            "six Islands minus the four drawn"
        );

        // Submit the 1-1 tie through the production vote action.
        // `vote_and_resolve` cannot be reused here: its settle loop answers
        // only OrderTriggers/Priority, and the Barony chaos parks a
        // DiscardChoice for P1's five-card hand.
        for (voter, choice) in [(P0, "planeswalk"), (P1, "chaos")] {
            let WaitingFor::VoteChoice { player, .. } = board.runner.state().waiting_for.clone()
            else {
                panic!(
                    "expected {voter:?}'s vote, got {:?}",
                    board.runner.state().waiting_for
                );
            };
            assert_eq!(player, voter, "votes proceed in turn order (CR 701.38a)");
            events.extend(
                board
                    .runner
                    .act(GameAction::ChooseOption {
                        choice: choice.to_string(),
                    })
                    .expect("the vote is accepted")
                    .events,
            );
        }

        events.extend(settle_through_the_barony_discard(
            &mut board.runner,
            board.bears,
        ));

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChaosEnsues,
                ..
            }
        )));
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::ChaosEnsued { plane_id } if *plane_id == board.planes.active
            )),
            "chaos ensued for The Dark Barony: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::Planeswalked { .. })));
        assert_eq!(
            active_plane(board.runner.state()),
            Some(board.planes.active)
        );
        assert_eq!(
            board.runner.state().planar_deck.front(),
            Some(&board.planes.next),
            "the planar deck is untouched"
        );
        // First-line outcome still exact after the chaos discard: P1 drew four
        // and discarded exactly one drawn Island, keeping the printed Bears.
        assert_eq!(
            drawn_by(&events, P1),
            4,
            "the targeted opponent drew exactly four"
        );
        assert_eq!(drawn_by(&events, P0), 0, "the controller drew none");
        assert_eq!(
            hand_size(&board.runner, P1),
            4,
            "four drawn cards minus the one discarded"
        );
        assert!(
            board.runner.state().players[P1.0 as usize]
                .hand
                .iter()
                .any(|id| *id == board.bears),
            "P1 keeps the printed Grizzly Bears"
        );
        assert_eq!(
            board.runner.state().objects[&board.enigma].zone,
            Zone::Graveyard
        );

        settle(&mut board.runner);
        assert!(board.runner.state().stack.is_empty());
        assert!(matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

mod path_of_the_pyromancer {
    use super::votes::{
        cast_to_first_vote, hand_size, planechase_scenario, seat_planes, vote_and_resolve, Planes,
    };
    use super::{mana_units, printed_cards, settle};
    use engine::game::planechase::active_plane;
    use engine::game::scenario::{GameRunner, P0, P1};
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::EffectKind;
    use engine::types::actions::GameAction;
    use engine::types::events::GameEvent;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::ManaType;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    /// The seven printed library cards, top first after staging: the first four
    /// are the four cards "that many cards plus one" draws, the last three stay.
    /// Distinct names so a wrong draw count is visible in the membership.
    const LIBRARY: [&str; 7] = [
        "Forest",
        "Island",
        "Mountain",
        "Plains",
        "Swamp",
        "Grizzly Bears",
        "Raging Goblin",
    ];

    struct Staged {
        spell: ObjectId,
        planes: Planes,
        /// The printed cards that start in hand besides the spell; all of them
        /// must be discarded by the first line.
        discarded: Vec<ObjectId>,
        /// `LIBRARY`, top first.
        library: Vec<ObjectId>,
    }

    fn library_ids(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
        runner.state().players[player.0 as usize]
            .library
            .iter()
            .copied()
            .collect()
    }

    fn hand_ids(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
        runner.state().players[player.0 as usize]
            .hand
            .iter()
            .copied()
            .collect()
    }

    fn graveyard_ids(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
        runner.state().players[player.0 as usize]
            .graveyard
            .iter()
            .copied()
            .collect()
    }

    fn sorted_ids(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
        ids.sort_unstable_by_key(|id| id.0);
        ids
    }

    /// A two-seat Planechase game with P0's five mana of the {4}{R} cost in the
    /// pool, the printed Path of the Pyromancer in hand, `hand` beside it, and
    /// [`LIBRARY`] on top of P0's library.
    fn pyromancer(hand: &[&str]) -> (GameRunner, Staged) {
        let (mut scenario, spell, active, next) =
            planechase_scenario("Path of the Pyromancer", mana_units(ManaType::Red, 5));
        let discarded: Vec<ObjectId> = hand
            .iter()
            .map(|name| scenario.add_real_card(P0, name, Zone::Hand, printed_cards()))
            .collect();
        let library: Vec<ObjectId> = LIBRARY
            .iter()
            .map(|name| scenario.add_real_card(P0, name, Zone::Library, printed_cards()))
            .collect();
        let mut runner = scenario.build();
        let planes = seat_planes(&mut runner, active, next);
        // Reach guard: `add_real_card` appends, so the first card added is the
        // library top the mill/draw instructions read.
        assert_eq!(
            library_ids(&runner, P0),
            library,
            "staging: the first library card added is the top card"
        );
        assert_eq!(
            runner.state().players[P0.0 as usize].mana_pool.total(),
            5,
            "staging: the pool holds exactly the five mana of the cost"
        );
        (
            runner,
            Staged {
                spell,
                planes,
                discarded,
                library,
            },
        )
    }

    /// The first line has resolved once the vote prompt is up: the whole hand is
    /// discarded, one {R} per discarded card is in the pool, and that many plus
    /// one cards are drawn. `drawn` is `discarded.len() + 1`.
    fn assert_first_line(runner: &GameRunner, staged: &Staged, drawn: usize) {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::VoteChoice { .. }),
            "the first line resolved before the vote: {:?}",
            runner.state().waiting_for
        );
        // The card leaves the stack only as the final part of its resolution
        // (CR 608.2n): while the vote prompt is up the spell is still in
        // `Zone::Stack`.
        assert_eq!(
            runner.state().objects[&staged.spell].zone,
            Zone::Stack,
            "the spell is still resolving while its first line has run"
        );
        assert_eq!(
            sorted_ids(graveyard_ids(runner, P0)),
            sorted_ids(staged.discarded.clone()),
            "every card that was in hand was discarded"
        );
        for card in &staged.discarded {
            assert_eq!(
                runner.state().objects[card].zone,
                Zone::Graveyard,
                "the discarded card is in the graveyard"
            );
        }
        assert!(
            graveyard_ids(runner, P1).is_empty(),
            "'Discard all the cards in your hand' names only the caster"
        );
        assert_eq!(
            hand_ids(runner, P0),
            staged.library[..drawn].to_vec(),
            "drew exactly {drawn} cards (that many plus one)"
        );
        assert_eq!(
            library_ids(runner, P0),
            staged.library[drawn..].to_vec(),
            "the library lost exactly the drawn cards"
        );
        let pool: Vec<ManaType> = runner.state().players[P0.0 as usize]
            .mana_pool
            .units()
            .map(|unit| unit.color)
            .collect();
        assert_eq!(
            pool,
            vec![ManaType::Red; staged.discarded.len()],
            "one {{R}} per card discarded this way, with the five of the cost spent"
        );
    }

    /// "If planeswalk gets more votes, planeswalk." Planeswalk wins 2-0 and the
    /// active plane is left for Naktamun; chaos does not ensue. The first line
    /// "Discard all the cards in your hand. Add {R} for each card discarded this
    /// way, then draw that many cards plus one." discarded all three, added
    /// {R}{R}{R} (CR 106.4), and drew four.
    #[test]
    fn path_of_the_pyromancer_planeswalk_wins() {
        let (mut runner, staged) = pyromancer(&["Grizzly Bears", "Grizzly Bears", "Grizzly Bears"]);
        cast_to_first_vote(&mut runner, staged.spell, None, ["planeswalk", "chaos"]);
        assert_first_line(&runner, &staged, 4);

        let events = vote_and_resolve(&mut runner, "planeswalk", "planeswalk");

        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::Planeswalked {
                    player_id,
                    from: Some(from),
                    to: Some(to),
                } if *player_id == P0 && *from == staged.planes.active && *to == staged.planes.next
            )),
            "planeswalked from The Dark Barony to Naktamun: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::ChaosEnsued { .. })));
        assert_eq!(active_plane(runner.state()), Some(staged.planes.next));
        assert_eq!(
            runner.state().planar_deck.back(),
            Some(&staged.planes.active),
            "the departing plane goes to the bottom of the planar deck"
        );
        assert_eq!(hand_size(&runner, P1), 1, "chaos did not ensue: no discard");

        assert_eq!(
            hand_ids(&runner, P0),
            staged.library[..4].to_vec(),
            "the four drawn cards are still in hand"
        );
        assert_eq!(
            sorted_ids(graveyard_ids(&runner, P0)),
            sorted_ids(
                staged
                    .discarded
                    .iter()
                    .copied()
                    .chain(std::iter::once(staged.spell))
                    .collect(),
            ),
            "the three discarded cards and the resolved spell are in the graveyard"
        );
        assert!(runner.state().stack.is_empty(), "the stack is empty");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "resolution settled at priority: {:?}",
            runner.state().waiting_for
        );
    }

    /// "If chaos gets more votes or the vote is tied, chaos ensues." A 1-1 tie
    /// runs The Dark Barony's chaos ability (the opponent discards a card) and
    /// nobody planeswalks; the first line's discard/mana/draw outcome is
    /// unchanged.
    #[test]
    fn path_of_the_pyromancer_tied_vote_chaos_ensues() {
        let (mut runner, staged) = pyromancer(&["Grizzly Bears", "Grizzly Bears", "Grizzly Bears"]);
        cast_to_first_vote(&mut runner, staged.spell, None, ["planeswalk", "chaos"]);
        assert_first_line(&runner, &staged, 4);

        let events = vote_and_resolve(&mut runner, "planeswalk", "chaos");

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChaosEnsues,
                ..
            }
        )));
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::ChaosEnsued { plane_id } if *plane_id == staged.planes.active
            )),
            "chaos ensued for the active plane: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::Planeswalked { .. })));
        assert_eq!(active_plane(runner.state()), Some(staged.planes.active));
        assert_eq!(
            runner.state().planar_deck.front(),
            Some(&staged.planes.next),
            "the planar deck is untouched"
        );
        assert_eq!(
            hand_size(&runner, P1),
            0,
            "The Dark Barony's chaos ability made the opponent discard"
        );

        assert_eq!(
            hand_ids(&runner, P0),
            staged.library[..4].to_vec(),
            "the four drawn cards are still in hand"
        );
        assert_eq!(
            sorted_ids(graveyard_ids(&runner, P0)),
            sorted_ids(
                staged
                    .discarded
                    .iter()
                    .copied()
                    .chain(std::iter::once(staged.spell))
                    .collect(),
            ),
            "the three discarded cards and the resolved spell are in the graveyard"
        );
        assert!(runner.state().stack.is_empty(), "the stack is empty");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "resolution settled at priority: {:?}",
            runner.state().waiting_for
        );
    }

    /// With no other cards in hand, "Discard all the cards in your hand" discards
    /// zero, "Add {R} for each card discarded this way" adds zero (CR 106.4),
    /// and "draw that many cards plus one" draws exactly one.
    #[test]
    fn path_of_the_pyromancer_empty_hand_draws_one() {
        let (mut runner, staged) = pyromancer(&[]);
        cast_to_first_vote(&mut runner, staged.spell, None, ["planeswalk", "chaos"]);
        assert_first_line(&runner, &staged, 1);

        for (voter, choice) in [(P0, "planeswalk"), (P1, "planeswalk")] {
            let WaitingFor::VoteChoice { player, .. } = runner.state().waiting_for.clone() else {
                panic!(
                    "expected {voter:?}'s vote, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(player, voter, "votes proceed in turn order");
            runner
                .act(GameAction::ChooseOption {
                    choice: choice.to_string(),
                })
                .expect("the vote is accepted");
        }
        let _events = settle(&mut runner);

        assert_eq!(active_plane(runner.state()), Some(staged.planes.next));
        assert!(runner.state().stack.is_empty(), "the stack is empty");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "resolution settled at priority: {:?}",
            runner.state().waiting_for
        );
    }
}

mod path_of_the_schemer {
    use super::votes::{hand_size, planechase_scenario, seat_planes, vote_and_resolve, Planes};
    use super::{mana_units, printed_cards};
    use engine::game::planechase::active_plane;
    use engine::game::scenario::{GameRunner, P0, P1};
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::EffectKind;
    use engine::types::actions::GameAction;
    use engine::types::card_type::CoreType;
    use engine::types::events::GameEvent;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::ManaType;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    struct Staged {
        spell: ObjectId,
        planes: Planes,
        /// A printed creature already in P0's graveyard before the spell.
        p0_creature: ObjectId,
        /// A different printed creature already in P1's graveyard.
        p1_creature: ObjectId,
        /// P0's library, top first: Forest and Island are milled.
        p0_library: Vec<ObjectId>,
        /// P1's library, top first: Raging Goblin and Mountain are milled.
        p1_library: Vec<ObjectId>,
    }

    fn library_ids(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
        runner.state().players[player.0 as usize]
            .library
            .iter()
            .copied()
            .collect()
    }

    fn graveyard_ids(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
        runner.state().players[player.0 as usize]
            .graveyard
            .iter()
            .copied()
            .collect()
    }

    fn battlefield_ids(runner: &GameRunner) -> Vec<ObjectId> {
        runner.state().battlefield.iter().copied().collect()
    }

    fn sorted_ids(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
        ids.sort_unstable_by_key(|id| id.0);
        ids
    }

    /// A two-seat Planechase game with P0's five mana of the {4}{B} cost in the
    /// pool, the printed Path of the Schemer in hand, one printed creature in
    /// each graveyard, and four-card libraries whose top two are known.
    fn schemer() -> (GameRunner, Staged) {
        let (mut scenario, spell, active, next) =
            planechase_scenario("Path of the Schemer", mana_units(ManaType::Black, 5));
        let p0_creature =
            scenario.add_real_card(P0, "Savannah Lions", Zone::Graveyard, printed_cards());
        let p1_creature = scenario.add_real_card(P1, "Gray Ogre", Zone::Graveyard, printed_cards());
        let p0_library: Vec<ObjectId> = ["Forest", "Island", "Plains", "Plains"]
            .iter()
            .map(|name| scenario.add_real_card(P0, name, Zone::Library, printed_cards()))
            .collect();
        let p1_library: Vec<ObjectId> = ["Raging Goblin", "Mountain", "Island", "Island"]
            .iter()
            .map(|name| scenario.add_real_card(P1, name, Zone::Library, printed_cards()))
            .collect();
        let mut runner = scenario.build();
        let planes = seat_planes(&mut runner, active, next);
        // Reach guard: `add_real_card` appends, so the first card added is the
        // library top that "Each player mills two cards" takes.
        assert_eq!(
            library_ids(&runner, P0),
            p0_library,
            "staging: the first P0 library card added is the top card"
        );
        assert_eq!(
            library_ids(&runner, P1),
            p1_library,
            "staging: the first P1 library card added is the top card"
        );
        (
            runner,
            Staged {
                spell,
                planes,
                p0_creature,
                p1_creature,
                p0_library,
                p1_library,
            },
        )
    }

    /// Cast the spell and stop at the resolution-time graveyard pick, returning
    /// the offered cards. "Then you put a creature card from a graveyard onto the
    /// battlefield under your control."
    fn cast_to_graveyard_pick(runner: &mut GameRunner, staged: &Staged) -> Vec<ObjectId> {
        let outcome = runner.cast(staged.spell).resolve();
        let WaitingFor::EffectZoneChoice { player, cards, .. } =
            outcome.final_waiting_for().clone()
        else {
            panic!(
                "the first line must offer a graveyard creature, got {:?}",
                outcome.final_waiting_for()
            );
        };
        assert_eq!(
            player, P0,
            "'you put a creature card from a graveyard' — the controller chooses"
        );
        cards
    }

    /// "Each player mills two cards" (CR 701.17a): each library is exactly two
    /// cards shorter and each graveyard holds exactly the two milled cards
    /// beside the creature that was already there.
    fn assert_mills(runner: &GameRunner, staged: &Staged) {
        assert_eq!(
            library_ids(runner, P0),
            staged.p0_library[2..].to_vec(),
            "P0 milled exactly Forest and Island"
        );
        assert_eq!(
            library_ids(runner, P1),
            staged.p1_library[2..].to_vec(),
            "P1 milled exactly Raging Goblin and Mountain"
        );
        assert_eq!(
            sorted_ids(graveyard_ids(runner, P0)),
            sorted_ids(vec![
                staged.p0_creature,
                staged.p0_library[0],
                staged.p0_library[1],
            ]),
            "Savannah Lions plus the two milled cards"
        );
        assert_eq!(
            sorted_ids(graveyard_ids(runner, P1)),
            sorted_ids(vec![
                staged.p1_creature,
                staged.p1_library[0],
                staged.p1_library[1],
            ]),
            "Gray Ogre plus the two milled cards"
        );
    }

    /// Exactly ONE creature card left a graveyard — not one per player: the
    /// milled Raging Goblin is on the battlefield under P0's control, and both
    /// other graveyard creatures (and the milled land) are untouched.
    fn assert_single_creature_moved(runner: &GameRunner, staged: &Staged) {
        let moved = staged.p1_library[0];
        assert_eq!(
            battlefield_ids(runner),
            vec![moved],
            "exactly one permanent entered the battlefield"
        );
        let object = &runner.state().objects[&moved];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(
            object.controller, P0,
            "onto the battlefield under your control"
        );
        assert_eq!(object.owner, P1, "it is the card milled from P1's library");
        assert_eq!(
            sorted_ids(graveyard_ids(runner, P0)),
            sorted_ids(vec![
                staged.p0_creature,
                staged.p0_library[0],
                staged.p0_library[1],
            ]),
            "P0's graveyard creature and both milled cards are untouched"
        );
        assert_eq!(
            sorted_ids(graveyard_ids(runner, P1)),
            sorted_ids(vec![staged.p1_creature, staged.p1_library[1]]),
            "P1 keeps the unchosen creature and the milled land"
        );
    }

    /// "It's an artifact in addition to its other types" — a type change that
    /// retains the prior types (CR 205.1b), applied in layer 4 (CR 613.1d); read
    /// from the engine's own evaluated characteristics.
    fn assert_artifact_creature(runner: &GameRunner, staged: &Staged) {
        let moved = staged.p1_library[0];
        let core_types = &runner.state().objects[&moved].card_types.core_types;
        assert!(
            core_types.contains(&CoreType::Creature),
            "it is still a creature: {core_types:?}"
        );
        assert!(
            core_types.contains(&CoreType::Artifact),
            "it is an artifact in addition to its other types: {core_types:?}"
        );
    }

    /// "If planeswalk gets more votes, planeswalk." Planeswalk wins 2-0 and the
    /// active plane is left for Naktamun; the first line mills two per player and
    /// puts exactly one creature card onto the battlefield as an artifact.
    #[test]
    fn path_of_the_schemer_planeswalk_wins() {
        let (mut runner, staged) = schemer();
        let offered = cast_to_graveyard_pick(&mut runner, &staged);
        assert_eq!(
            sorted_ids(offered),
            sorted_ids(vec![
                staged.p0_creature,
                staged.p1_creature,
                staged.p1_library[0],
            ]),
            "every creature card in both graveyards, including the milled Raging Goblin"
        );
        assert_mills(&runner, &staged);

        runner
            .act(GameAction::SelectCards {
                cards: vec![staged.p1_library[0]],
            })
            .expect("the graveyard pick is accepted");
        assert_single_creature_moved(&runner, &staged);

        let WaitingFor::VoteChoice {
            player,
            options,
            remaining_voters,
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "the first line resolved to the vote, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(
            player, P0,
            "'Starting with you': the controller votes first"
        );
        assert_eq!(options, vec!["planeswalk".to_string(), "chaos".to_string()]);
        assert_eq!(
            remaining_voters
                .iter()
                .map(|(seat, _)| *seat)
                .collect::<Vec<_>>(),
            vec![P1]
        );

        let events = vote_and_resolve(&mut runner, "planeswalk", "planeswalk");

        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::Planeswalked {
                    player_id,
                    from: Some(from),
                    to: Some(to),
                } if *player_id == P0 && *from == staged.planes.active && *to == staged.planes.next
            )),
            "planeswalked from The Dark Barony to Naktamun: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::ChaosEnsued { .. })));
        assert_eq!(active_plane(runner.state()), Some(staged.planes.next));
        assert_eq!(
            runner.state().planar_deck.back(),
            Some(&staged.planes.active),
            "the departing plane goes to the bottom of the planar deck"
        );
        assert_eq!(hand_size(&runner, P1), 1, "chaos did not ensue: no discard");

        assert_artifact_creature(&runner, &staged);
        assert_eq!(
            battlefield_ids(&runner),
            vec![staged.p1_library[0]],
            "the chosen creature is still the only permanent"
        );
        assert!(runner.state().stack.is_empty(), "the stack is empty");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "resolution settled at priority: {:?}",
            runner.state().waiting_for
        );
    }

    /// "If chaos gets more votes or the vote is tied, chaos ensues." A 1-1 tie
    /// runs The Dark Barony's chaos ability (the opponent discards) and nobody
    /// planeswalks; the first line's mills and single creature move are
    /// unchanged.
    #[test]
    fn path_of_the_schemer_tied_vote_chaos_ensues() {
        let (mut runner, staged) = schemer();
        let offered = cast_to_graveyard_pick(&mut runner, &staged);
        assert_eq!(
            sorted_ids(offered),
            sorted_ids(vec![
                staged.p0_creature,
                staged.p1_creature,
                staged.p1_library[0],
            ]),
            "every creature card in both graveyards, including the milled Raging Goblin"
        );
        assert_mills(&runner, &staged);

        runner
            .act(GameAction::SelectCards {
                cards: vec![staged.p1_library[0]],
            })
            .expect("the graveyard pick is accepted");
        assert_single_creature_moved(&runner, &staged);

        let WaitingFor::VoteChoice { player, .. } = runner.state().waiting_for.clone() else {
            panic!(
                "the first line resolved to the vote, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(
            player, P0,
            "'Starting with you': the controller votes first"
        );

        let events = vote_and_resolve(&mut runner, "planeswalk", "chaos");

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChaosEnsues,
                ..
            }
        )));
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::ChaosEnsued { plane_id } if *plane_id == staged.planes.active
            )),
            "chaos ensued for the active plane: {events:?}"
        );
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::Planeswalked { .. })));
        assert_eq!(active_plane(runner.state()), Some(staged.planes.active));
        assert_eq!(
            runner.state().planar_deck.front(),
            Some(&staged.planes.next),
            "the planar deck is untouched"
        );
        assert_eq!(
            hand_size(&runner, P1),
            0,
            "The Dark Barony's chaos ability made the opponent discard"
        );

        assert_artifact_creature(&runner, &staged);
        assert_eq!(
            battlefield_ids(&runner),
            vec![staged.p1_library[0]],
            "exactly one creature card moved, not one per player"
        );
        assert!(runner.state().stack.is_empty(), "the stack is empty");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "resolution settled at priority: {:?}",
            runner.state().waiting_for
        );
    }
}
