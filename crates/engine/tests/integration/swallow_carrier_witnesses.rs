//! Runtime witnesses for the typed carriers the swallowed-clause audit now credits
//! (`crates/phase-parser/src/parser/swallow_check.rs`): Combat Celebrant's "if this creature
//! hasn't been exerted this turn" attack-cost gate, Double Major's "except it isn't legendary if
//! the spell is legendary" copy exception, Vraska, Betrayal's Sting's "if target player has fewer
//! than nine poison counters" difference, and Mizzix's Mastery's "for each card exiled this way,
//! copy it" cast. Each card is the printed card loaded through `add_real_card` and driven through
//! public `GameRunner` actions. Bitterblade Warrior can exert again in the same turn, and
//! Double Major preserves the name, power, toughness, and nonlegendary status of Grizzly Bears.
//! Vraska adds poison only to the announced player and uses that player's team total in Two-Headed
//! Giant, including at the zero floor. Mastery's empty overload publishes an empty set and leaves
//! an older set intact, and its decline-all case completes without casting any copies.

use engine::database::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::players::team_poison_total;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityCost, EffectKind};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef, TrackedSetId};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{PlayerId, Supertype};

use super::support::shared_card_db;

fn printed_cards() -> &'static CardDatabase {
    shared_card_db().expect("printed-card fixture is required")
}

fn mana(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn settled(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn exert(runner: &mut GameRunner, expected: ObjectId) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ExertChoice { player: P0, attacker, .. } if attacker == expected
    ));
    runner.act(GameAction::ChooseExert { exert: true }).unwrap();
    assert!(runner.state().exerted_this_turn.contains(&expected));
}

#[test]
fn celebrant_gates_the_second_attack_but_bitterblade_warrior_can_exert_again() {
    let db = printed_cards();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let celebrant = scenario.add_real_card(P0, "Combat Celebrant", Zone::Battlefield, db);
    let warrior = scenario.add_real_card(P0, "Bitterblade Warrior", Zone::Battlefield, db);
    let vitalize = scenario.add_real_card(P0, "Vitalize", Zone::Hand, db);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let incarnation = ObjectIncarnationRef::from_object(&runner.state().objects[&celebrant]);
    let turn = runner.state().turn_number;
    let attacks = [
        (celebrant, AttackTarget::Player(P1)),
        (warrior, AttackTarget::Player(P1)),
    ];
    runner.advance_to_combat();
    runner
        .declare_attackers(&attacks)
        .expect("first declaration");
    // CR 508.1g + CR 701.43d: both attackers can pay their linked optional costs.
    exert(&mut runner, celebrant);
    exert(&mut runner, warrior);
    runner.advance_until_stack_empty();
    settled(&runner);
    // CR 500.8: the linked payoff untaps the other attacker and earns a combat.
    assert_eq!(runner.state().extra_phases.len(), 1);
    assert!(runner.state().objects[&celebrant].tapped);
    assert!(!runner.state().objects[&warrior].tapped);
    runner
        .cast(vitalize)
        .resolve()
        .assert_zone(&[vitalize], Zone::Graveyard);
    assert!(!runner.state().objects[&celebrant].tapped);

    for _ in 0..40 {
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => break,
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass combat priority");
            }
            ref waiting => panic!("unexpected prompt between combats: {waiting:?}"),
        }
    }
    assert_eq!(runner.state().turn_number, turn);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ));
    assert!(runner.state().extra_phases.is_empty());
    assert_eq!(
        ObjectIncarnationRef::from_object(&runner.state().objects[&celebrant]),
        incarnation
    );
    assert!(runner.state().exerted_this_turn.contains(&celebrant));
    assert!(runner.state().exerted_this_turn.contains(&warrior));
    runner
        .declare_attackers(&attacks)
        .expect("second declaration");
    // CR 701.43b + CR 508.1g: prior exert alone does not bar another payment.
    exert(&mut runner, warrior);
    runner.advance_until_stack_empty();
    settled(&runner);
    assert!(runner
        .state()
        .creature_attacked_player_this_combat(celebrant, P1));
    assert!(runner.state().extra_phases.is_empty());
}

fn double_major(name: &str, legendary: bool, colors: &[ManaType]) {
    let db = printed_cards();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_real_card(P0, name, Zone::Hand, db);
    let major = scenario.add_real_card(P0, "Double Major", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(colors));
    let mut runner = scenario.build();
    let _ = runner.cast(creature).commit();
    runner.cast(major).target_object(creature).resolve();
    settled(&runner);
    // CR 707.9b + CR 707.10f: the copy retains the name and enters as a token
    // without the legendary supertype, so CR 704.5j leaves both on the field.
    let original = &runner.state().objects[&creature];
    assert_eq!(original.zone, Zone::Battlefield);
    assert_eq!(
        original
            .card_types
            .supertypes
            .contains(&Supertype::Legendary),
        legendary
    );
    let copies: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.zone == Zone::Battlefield && object.name == name && object.id != creature
        })
        .collect();
    assert_eq!(copies.len(), 1);
    assert!(copies[0].is_token);
    assert!(!copies[0]
        .card_types
        .supertypes
        .contains(&Supertype::Legendary));
    assert_eq!(
        (copies[0].power, copies[0].toughness),
        (original.power, original.toughness)
    );
}

#[test]
fn double_major_copies_a_legendary_creature_without_the_supertype() {
    double_major(
        "Isamaru, Hound of Konda",
        true,
        &[ManaType::White, ManaType::Green, ManaType::Blue],
    );
}

#[test]
fn double_major_copies_a_nonlegendary_creature_unchanged() {
    double_major(
        "Grizzly Bears",
        false,
        &[
            ManaType::White,
            ManaType::Green,
            ManaType::Green,
            ManaType::Blue,
        ],
    );
}

fn vraska(mut scenario: GameScenario, poison: &[u32], target: PlayerId) -> (u32, Vec<u32>) {
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_real_card(
        P0,
        "Vraska, Betrayal's Sting",
        Zone::Battlefield,
        printed_cards(),
    );
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&source).unwrap();
    object.loyalty = Some(10);
    object.counters.insert(CounterType::Loyalty, 10);
    let index = object
        .abilities
        .iter()
        .position(|ability| matches!(ability.cost, Some(AbilityCost::Loyalty { amount: -9 })))
        .expect("printed -9 ability");
    for (player, &count) in runner.state_mut().players.iter_mut().zip(poison) {
        player.poison_counters = count;
    }
    let target_team_poison = team_poison_total(runner.state(), target);
    runner
        .activate(source, index)
        .target_player(target)
        .resolve();
    settled(&runner);
    // CR 606.4 + CR 608.1: payment and an empty stack establish resolution reach.
    assert_eq!(
        runner.state().objects[&source].counters[&CounterType::Loyalty],
        1
    );
    let after = runner
        .state()
        .players
        .iter()
        .map(|player| player.poison_counters)
        .collect();
    (target_team_poison, after)
}

#[test]
fn vraska_reads_and_tops_up_the_announced_player() {
    let scenario = GameScenario::new_n_player(3, 42);
    // CR 608.2c: only the announced player supplies the count and receives counters.
    assert_eq!(vraska(scenario, &[7, 3, 5], P1), (3, vec![7, 9, 5]));
}

#[test]
fn vraska_clamps_at_zero_for_a_target_team_above_nine_poison() {
    let scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    // CR 810.10a + CR 810.8d: target the head with zero of a live ten-poison team.
    assert_eq!(
        vraska(scenario, &[0, 0, 0, 10], PlayerId(2)),
        (10, vec![0, 0, 0, 10])
    );
}

#[test]
fn vraska_adds_the_team_poison_difference_to_the_targeted_head() {
    let scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    // CR 810.10a: read the team's six counters and put the difference on the targeted head.
    assert_eq!(
        vraska(scenario, &[0, 0, 0, 6], PlayerId(2)),
        (6, vec![0, 0, 3, 6])
    );
}

struct MasteryBoard {
    runner: GameRunner,
    mastery: ObjectId,
    eligible: Vec<ObjectId>,
    opposing: ObjectId,
    stale: ObjectId,
    stale_set: TrackedSetId,
}

fn mastery_board(names: &[&str]) -> MasteryBoard {
    let db = printed_cards();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mastery = scenario.add_real_card(P0, "Mizzix's Mastery", Zone::Hand, db);
    let stale = scenario.add_real_card(P0, "Sacred Nectar", Zone::Exile, db);
    let opposing = scenario.add_real_card(P1, "Sacred Nectar", Zone::Graveyard, db);
    let eligible = names
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Graveyard, db))
        .collect();
    scenario.with_mana_pool(P0, mana(&[ManaType::Red; 8]));
    let mut runner = scenario.build();
    let stale_set = TrackedSetId(runner.state().next_tracked_set_id);
    runner.state_mut().next_tracked_set_id += 1;
    runner
        .state_mut()
        .tracked_object_sets
        .insert(stale_set, vec![stale]);
    MasteryBoard {
        runner,
        mastery,
        eligible,
        opposing,
        stale,
        stale_set,
    }
}

impl MasteryBoard {
    fn overload(&mut self) -> CastOutcome {
        self.runner
            .cast(self.mastery)
            .alternative_cast(AlternativeCastDecision::Alternative)
            .resolve()
    }

    fn assert_offered(&self, expected: &[ObjectId]) {
        let WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            up_to,
            source_id,
            ..
        } = &self.runner.state().waiting_for
        else {
            panic!(
                "expected one copy selection, got {:?}",
                self.runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, self.mastery);
        assert!(*up_to);
        assert_eq!(*count, expected.len());
        assert_eq!(cards.len(), expected.len());
        assert!(expected.iter().all(|id| cards.contains(id)));
        for id in expected {
            assert_eq!(self.runner.state().objects[id].zone, Zone::Exile);
        }
    }

    fn select(&mut self, cards: Vec<ObjectId>) {
        self.runner.act(GameAction::SelectCards { cards }).unwrap();
    }

    fn assert_finished(&self, life: i32, exiled: &[ObjectId]) {
        settled(&self.runner);
        assert_eq!(self.runner.life(P0), life);
        for id in exiled.iter().chain([&self.mastery, &self.stale]) {
            assert_eq!(self.runner.state().objects[id].zone, Zone::Exile);
        }
        assert_eq!(
            self.runner.state().objects[&self.opposing].zone,
            Zone::Graveyard
        );
    }
}

#[test]
fn mastery_ordinary_cast_copies_only_the_target() {
    let mut board = mastery_board(&["Sacred Nectar", "Whitesun's Passage"]);
    let target = board.eligible[0];
    board
        .runner
        .cast(board.mastery)
        .alternative_cast(AlternativeCastDecision::Normal)
        .target_object(target)
        .resolve();
    // CR 608.2c + CR 707.12a: copy only the original exiled by this resolution.
    board.assert_offered(&[target]);
    board.select(vec![target]);
    board.runner.advance_until_stack_empty();
    board.assert_finished(24, &[target]);
    assert_eq!(
        board.runner.state().objects[&board.eligible[1]].zone,
        Zone::Graveyard
    );
}

#[test]
fn mastery_empty_overload_never_reuses_an_older_exile_set() {
    let mut board = mastery_board(&[]);
    let fresh_set = TrackedSetId(board.runner.state().next_tracked_set_id);
    let outcome = board.overload();
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::CastCopyOfCard,
            source_id,
            subject: None,
        } if *source_id == board.mastery
    )));
    let state = board.runner.state();
    assert_eq!(state.next_tracked_set_id, fresh_set.0 + 1);
    assert!(state.tracked_object_sets[&fresh_set].is_empty());
    assert_eq!(state.tracked_object_sets[&board.stale_set], [board.stale]);
    // CR 702.96a + CR 608.2c: the empty exile instruction supplies no copies.
    board.assert_finished(20, &[]);
}

#[test]
fn mastery_overload_supports_declining_all_copies() {
    let mut board = mastery_board(&["Sacred Nectar", "Chaplain's Blessing"]);
    board.overload();
    board.assert_offered(&board.eligible);
    // CR 707.12a: declining every copy still completes the self-exile.
    board.select(vec![]);
    board.assert_finished(20, &board.eligible);
}

#[test]
fn mastery_overload_casts_the_selected_copies_in_order_and_resolves_in_reverse() {
    let mut board = mastery_board(&["Sacred Nectar", "Whitesun's Passage", "Chaplain's Blessing"]);
    board.overload();
    board.assert_offered(&board.eligible);
    let [nectar, passage, _] = board.eligible[..] else {
        unreachable!()
    };
    // CR 707.12a + CR 405.2: choose two in reverse fixture order.
    board.select(vec![passage, nectar]);
    let names: Vec<_> = board
        .runner
        .state()
        .stack
        .iter()
        .map(|entry| board.runner.state().objects[&entry.source_id].name.as_str())
        .collect();
    assert_eq!(names, ["Whitesun's Passage", "Sacred Nectar"]);
    // CR 608.1: the last cast copy resolves first.
    board.runner.pass_both_players();
    assert_eq!(board.runner.life(P0), 24);
    assert_eq!(board.runner.state().stack.len(), 1);
    board.runner.advance_until_stack_empty();
    board.assert_finished(29, &board.eligible);
}
