//! A possessive-player subject keeps its POSSESSOR in scope as the antecedent
//! of the bare object pronoun in its own predicate.
//!
//! CR 608.2c: "read the whole text and apply the rules of English to the text."
//! In "that creature's controller sacrifices it at end of combat" the subject
//! names a player only by way of a creature that player controls, so "it" is
//! that creature — not the ability's source. CR 608.2k lets that untargeted
//! reference keep naming the same object once the delayed ability resolves.
//! CR 701.21a is why the two must agree: "To sacrifice a permanent, its
//! controller moves it from the battlefield directly to its owner's graveyard.
//! A player can't sacrifice something that isn't a permanent, or something
//! that's a permanent they don't control" — so the actor and the subject bind
//! to the same side, and a source-self reading authorizes nobody to act.
//!
//! CR 509.3d: "An ability that reads 'Whenever [a creature] becomes blocked by
//! a creature, . . .' triggers once for each creature that blocks the specified
//! creature" — so each blocker gets its own delayed instruction, bound to that
//! blocker and that blocker's controller.
//!
//! CR 111.2: "The player who creates a token is its owner. The token enters the
//! battlefield under that player's control" — the Wall belongs to the player
//! who sacrificed, not to the Golem's controller.
//!
//! Cards (verbatim Oracle text, read from MTGJSON `data/mtgjson/AtomicCards.json`):
//! - Basalt Golem — "This creature can't be blocked by artifact creatures.
//!   Whenever this creature becomes blocked by a creature, that creature's
//!   controller sacrifices it at end of combat. If the player does, they create
//!   a 0/2 colorless Wall artifact creature token with defender."
//!   Basalt Golem is the one card in the printed class whose lowering this
//!   change moves, so it carries the discriminating runtime rows.
//! - Cerulean Sphinx — "Flying\n{U}: This creature's owner shuffles it into
//!   their library." A source-possessive sibling, held here as a no-regression
//!   row — see its own doc for what it does and does not pin.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{DelayedTriggerCondition, Effect, EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const BASALT_GOLEM: &str = "This creature can't be blocked by artifact creatures.\nWhenever this creature becomes blocked by a creature, that creature's controller sacrifices it at end of combat. If the player does, they create a 0/2 colorless Wall artifact creature token with defender.";

const CERULEAN_SPHINX: &str = "Flying\n{U}: This creature's owner shuffles it into their library.";

/// Drive the game forward through every prompt combat opens on the way to the
/// end-of-combat step, until `done` holds. A 2-power attacker blocked by two
/// creatures opens a CR 510.1c assignment prompt, which is answered by piling
/// all of the damage onto the first listed blocker.
fn drive_until(runner: &mut GameRunner, done: impl Fn(&GameState) -> bool) {
    for _ in 0..80 {
        if done(runner.state()) {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::AssignCombatDamage {
                total_damage,
                blockers,
                ..
            } => {
                let first = blockers
                    .first()
                    .map(|slot| slot.blocker_id)
                    .expect("an assignment prompt lists at least one blocker");
                GameAction::AssignCombatDamage {
                    mode: Default::default(),
                    assignments: vec![(first, *total_damage)],
                    trample_damage: 0,
                    controller_damage: 0,
                }
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the prompt is answerable");
    }
    panic!("the awaited state was not reached");
}

/// Every delayed end-of-combat sacrifice currently scheduled.
fn scheduled_end_of_combat_sacrifices(state: &GameState) -> usize {
    state
        .delayed_triggers
        .iter()
        .filter(|dt| {
            matches!(
                dt.condition,
                DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::EndCombat
                }
            ) && matches!(&dt.ability.effect, Effect::Sacrifice { .. })
        })
        .count()
}

/// The 0/2 Wall artifact creature tokens with defender that `player` controls.
fn wall_tokens_of(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    let mut walls: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj.controller == player
                    && obj.name == "Wall"
                    && obj.power == Some(0)
                    && obj.toughness == Some(2)
                    && obj.has_keyword(&Keyword::Defender)
            })
        })
        .collect();
    walls.sort_unstable();
    walls
}

/// A Basalt Golem P0 controls, attacking P1, blocked by every creature in
/// `blockers`. Returns the runner with the blocks declared and the resulting
/// `BecomesBlocked` triggers resolved.
fn golem_blocked_by(scenario: GameScenario, golem: ObjectId, blockers: &[ObjectId]) -> GameRunner {
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(golem, AttackTarget::Player(P1))])
        .expect("Basalt Golem can attack");
    drive_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    let assignments: Vec<(ObjectId, ObjectId)> =
        blockers.iter().map(|blocker| (*blocker, golem)).collect();
    runner
        .declare_blockers(&assignments)
        .expect("the blockers can block");
    runner.advance_until_stack_empty();
    runner
}

/// CR 608.2c + CR 608.2k + CR 701.21a + CR 111.2: the blocker — not Basalt
/// Golem — is sacrificed at end of combat, by the blocker's own controller, and
/// that player gets the Wall.
///
/// This is the discriminating runtime row for the fix. Reverting the parser
/// change restores `Sacrifice { target: SelfRef }`: the delayed ability still
/// holds the blocker in its snapshotted targets, so the resolver's inherited
/// pool still names the blocker, but `authorized_sacrificers` for a non-anaphor
/// filter is `[ability.controller]` (P0) while the blocker's controller is P1 —
/// so nothing is sacrificed at all and no Wall is created. Measured on the
/// reverted parser: this row fails first at the graveyard assertion with
/// `Some(Battlefield)`, and the Wall assertions below it flip for the same
/// reason.
#[test]
fn a_blocker_is_sacrificed_by_its_own_controller_at_end_of_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let golem = scenario
        .add_creature_from_oracle(P0, "Basalt Golem", 2, 4, BASALT_GOLEM)
        .id();
    let blocker = scenario.add_creature(P1, "Stone Blocker", 0, 5).id();
    let mut runner = golem_blocked_by(scenario, golem, &[blocker]);

    // Reach guard: the BecomesBlocked trigger resolved and scheduled exactly
    // one end-of-combat sacrifice (CR 509.3d: once per blocker), and neither
    // creature has died in combat yet.
    assert_eq!(
        scheduled_end_of_combat_sacrifices(runner.state()),
        1,
        "reach guard: the becomes-blocked trigger scheduled one end-of-combat sacrifice; \
         delayed_triggers = {:?}",
        runner.state().delayed_triggers
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Battlefield,
        "reach guard: the blocker is still on the battlefield before end of combat"
    );
    assert_eq!(
        runner.state().objects[&blocker].controller,
        P1,
        "reach guard: the blocker is controlled by P0's opponent"
    );

    drive_until(&mut runner, |state| {
        state.phase == Phase::EndCombat && state.stack.is_empty()
    });
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects.get(&blocker).map(|obj| obj.zone),
        Some(Zone::Graveyard),
        "CR 701.21a: the blocking creature's controller sacrifices IT at end of combat"
    );
    assert_eq!(
        runner.state().objects.get(&golem).map(|obj| obj.zone),
        Some(Zone::Battlefield),
        "Basalt Golem must survive — the card sacrifices the creature that blocked it, \
         never itself"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P1).len(),
        1,
        "CR 111.2: the player who sacrificed creates the Wall, so it belongs to the \
         blocker's controller"
    );
    assert_eq!(
        runner.state().objects[&wall_tokens_of(runner.state(), P1)[0]].owner,
        P1,
        "CR 111.2: the creating player is the token's owner"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P0),
        Vec::new(),
        "the Golem's controller never creates the Wall"
    );
}

/// CR 603.7c + CR 701.21a: when the blocker has left the battlefield before the
/// delayed instruction resolves, nothing is sacrificed and the gated Wall is
/// never created — the "If the player does" rider is tied to the sacrifice
/// actually happening, not to the trigger having fired.
#[test]
fn a_blocker_that_already_left_sacrifices_nothing_and_makes_no_wall() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let golem = scenario
        .add_creature_from_oracle(P0, "Basalt Golem", 2, 4, BASALT_GOLEM)
        .id();
    // A 1-toughness blocker dies to the Golem's 2 combat damage (CR 704.5g)
    // before the end-of-combat step.
    let blocker = scenario.add_creature(P1, "Fragile Blocker", 0, 1).id();
    let mut runner = golem_blocked_by(scenario, golem, &[blocker]);

    assert_eq!(
        scheduled_end_of_combat_sacrifices(runner.state()),
        1,
        "reach guard: the becomes-blocked trigger scheduled its end-of-combat sacrifice"
    );

    drive_until(&mut runner, |state| {
        state.phase == Phase::EndCombat && state.stack.is_empty()
    });
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects.get(&blocker).map(|obj| obj.zone),
        Some(Zone::Graveyard),
        "reach guard: the blocker died to combat damage, so the delayed sacrifice found \
         nothing to sacrifice"
    );
    assert_eq!(
        scheduled_end_of_combat_sacrifices(runner.state()),
        0,
        "reach guard: the delayed instruction actually fired and was consumed, so the \
         absences below are measured after it resolved — not before it ran"
    );
    assert_eq!(
        runner.state().objects.get(&golem).map(|obj| obj.zone),
        Some(Zone::Battlefield),
        "Basalt Golem is not a fallback subject for a sacrifice it never authorized"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P1),
        Vec::new(),
        "no sacrifice happened, so the gated Wall is not created"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P0),
        Vec::new(),
        "and certainly not for the Golem's controller"
    );
}

/// CR 400.7 + CR 603.7c: the delayed instruction names the blocker it
/// snapshotted, and a blocker that left and returned under the same storage id
/// is a NEW object the instruction may not touch.
///
/// This is the currency guard on the referent the fix introduces. A
/// `ParentTarget` an ability PINNED is checked against its recorded
/// `target_incarnations`: this delayed instruction pins exactly one object, so
/// `effects/sacrifice.rs`'s `pinned_object_targets_all_stale` early return
/// fires first and `live_object_targets` is never reached on this path, though
/// both apply the same `target_pin_is_current`. Either way the blinked-back
/// creature is dropped, nothing is sacrificed, and the gated Wall is not
/// created. (The unpinned fallback rung of `targeting::resolved_targets`
/// carries no such check, which is why
/// `subject::subject_possessive_object_antecedent` refuses to pin a subject
/// whose runtime referent would degrade to it.)
#[test]
fn a_blocker_that_blinked_back_is_a_new_object_and_is_not_sacrificed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let golem = scenario
        .add_creature_from_oracle(P0, "Basalt Golem", 2, 4, BASALT_GOLEM)
        .id();
    let blocker = scenario.add_creature(P1, "Blinking Blocker", 0, 5).id();
    let mut runner = golem_blocked_by(scenario, golem, &[blocker]);

    assert_eq!(
        scheduled_end_of_combat_sacrifices(runner.state()),
        1,
        "reach guard: the becomes-blocked trigger scheduled its end-of-combat sacrifice"
    );

    // CR 400.7: battlefield -> exile -> battlefield returns a new object at the
    // same `ObjectId`.
    let before = runner.state().objects[&blocker].incarnation;
    {
        let mut events = Vec::new();
        engine::game::zones::move_to_zone(runner.state_mut(), blocker, Zone::Exile, &mut events);
        engine::game::zones::move_to_zone(
            runner.state_mut(),
            blocker,
            Zone::Battlefield,
            &mut events,
        );
    }
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Battlefield,
        "reach guard: it came back"
    );
    assert!(
        runner.state().objects[&blocker].incarnation > before,
        "reach guard: a blink is a new incarnation at the same id"
    );

    drive_until(&mut runner, |state| {
        state.phase == Phase::EndCombat && state.stack.is_empty()
    });
    runner.advance_until_stack_empty();

    assert_eq!(
        scheduled_end_of_combat_sacrifices(runner.state()),
        0,
        "reach guard: the delayed instruction fired and was consumed, so the absences \
         below are measured after it resolved"
    );
    assert_eq!(
        runner.state().objects.get(&blocker).map(|obj| obj.zone),
        Some(Zone::Battlefield),
        "CR 400.7: the returned creature is a new object and is not sacrificed"
    );
    assert_eq!(
        runner.state().objects.get(&golem).map(|obj| obj.zone),
        Some(Zone::Battlefield),
        "and Basalt Golem is not a fallback subject for a sacrifice that found nothing"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P1),
        Vec::new(),
        "no sacrifice happened, so the gated Wall is not created"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P0),
        Vec::new(),
        "and none for the Golem's controller"
    );
}

/// CR 509.3d: the trigger fires once per blocking creature, so two blockers
/// produce two independent delayed instructions — each bound to its own blocker
/// and its own blocker's controller, with one Wall per successful sacrifice.
#[test]
fn two_blockers_each_get_their_own_delayed_sacrifice_and_wall() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let golem = scenario
        .add_creature_from_oracle(P0, "Basalt Golem", 2, 4, BASALT_GOLEM)
        .id();
    let first = scenario.add_creature(P1, "First Blocker", 0, 5).id();
    let second = scenario.add_creature(P1, "Second Blocker", 0, 5).id();
    let mut runner = golem_blocked_by(scenario, golem, &[first, second]);

    assert_eq!(
        scheduled_end_of_combat_sacrifices(runner.state()),
        2,
        "CR 509.3d: one delayed sacrifice per blocking creature; delayed_triggers = {:?}",
        runner.state().delayed_triggers
    );

    drive_until(&mut runner, |state| {
        state.phase == Phase::EndCombat && state.stack.is_empty()
    });
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects.get(&first).map(|obj| obj.zone),
        Some(Zone::Graveyard),
        "the first blocker is sacrificed"
    );
    assert_eq!(
        runner.state().objects.get(&second).map(|obj| obj.zone),
        Some(Zone::Graveyard),
        "the second blocker is sacrificed — neither firing collapsed onto the other's subject"
    );
    assert_eq!(
        runner.state().objects.get(&golem).map(|obj| obj.zone),
        Some(Zone::Battlefield),
        "Basalt Golem survives both firings"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P1).len(),
        2,
        "CR 111.2: one Wall per successful sacrifice, both for the blockers' controller"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P0),
        Vec::new(),
        "no Wall for the Golem's controller"
    );
}

/// CR 608.2h + CR 701.21a: "that creature's controller" is read when the delayed
/// ability resolves, so two blockers controlled by DIFFERENT players at that
/// moment are each sacrificed by their own controller, and each of those two
/// players gets their own Wall.
///
/// Blocks are declared by one defending player (CR 509.1a), so the divergence is
/// introduced the only way a single attacker's blockers can diverge: control of
/// one blocker changes (CR 613.1b, layer 2) after blockers are declared and
/// before the end-of-combat step. The delayed instruction already holds that
/// object (CR 603.7c), and CR 701.21a makes its CURRENT controller the only
/// player who may sacrifice it.
#[test]
fn two_blockers_with_different_controllers_each_sacrifice_their_own() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let golem = scenario
        .add_creature_from_oracle(P0, "Basalt Golem", 2, 4, BASALT_GOLEM)
        .id();
    let stays = scenario.add_creature(P1, "P1's Blocker", 0, 5).id();
    let handed_over = scenario.add_creature(P1, "Handed-Over Blocker", 0, 5).id();
    let mut runner = golem_blocked_by(scenario, golem, &[stays, handed_over]);

    assert_eq!(
        scheduled_end_of_combat_sacrifices(runner.state()),
        2,
        "reach guard: CR 509.3d scheduled one delayed sacrifice per blocker"
    );

    // CR 613.1b: hand one blocker to a third player. This rewrites the layer-2
    // BASELINE rather than installing a control-changing effect the way
    // production does (`effects::gain_control` adds a transient continuous
    // `ChangeController` modification and leaves `base_controller` alone).
    // Equivalent for every assertion below, because `layers.rs`'s per-object
    // reset recomputes `controller` from `base_controller` and both routes
    // therefore leave `obj.controller == P2` after the layer pass — which is
    // the only input `resolve_sacrifice_scope` and `parent_target_controller`
    // read.
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&handed_over)
            .expect("the blocker whose control changes");
        obj.base_controller = Some(P2);
        obj.controller = P2;
    }
    assert_eq!(
        runner.state().objects[&handed_over].controller,
        P2,
        "reach guard: one blocker is now controlled by a third player"
    );
    assert_eq!(
        runner.state().objects[&stays].controller,
        P1,
        "reach guard: the other blocker is still P1's"
    );

    drive_until(&mut runner, |state| {
        state.phase == Phase::EndCombat && state.stack.is_empty()
    });
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects.get(&stays).map(|obj| obj.zone),
        Some(Zone::Graveyard),
        "P1 sacrifices the blocker P1 controls"
    );
    assert_eq!(
        runner.state().objects.get(&handed_over).map(|obj| obj.zone),
        Some(Zone::Graveyard),
        "CR 701.21a: the blocker's CURRENT controller sacrifices it"
    );
    assert_eq!(
        runner.state().objects.get(&golem).map(|obj| obj.zone),
        Some(Zone::Battlefield),
        "Basalt Golem survives"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P1).len(),
        1,
        "CR 111.2: P1 created exactly one Wall"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P2).len(),
        1,
        "CR 111.2: the third player created the other Wall — the actor is read per firing"
    );
    assert_eq!(
        wall_tokens_of(runner.state(), P0),
        Vec::new(),
        "no Wall for the Golem's controller"
    );
}

/// Sibling no-regression: Cerulean Sphinx's "This creature's owner shuffles it
/// into their library" must still move the Sphinx.
///
/// What this row pins is narrow, and deliberately so. The clause is claimed
/// whole by `oracle_effect`'s owner-of-target recognizer, which derives the
/// moved object from the possessor text and never reaches
/// `lower_subject_predicate_ast` — so this row does NOT witness the pin's
/// source-possessive exclusion (that is pinned at the unit level, in
/// `oracle_effect::subject::tests::a_parent_target_possessive_subject_names_its_possessor`,
/// which asserts `TargetFilter::SourceController` declines). It witnesses that
/// the pin did not disturb a whole-clause recognizer standing between the same
/// printed grammar and the same runtime outcome, which is the regression this
/// change could plausibly have caused there.
#[test]
fn a_source_possessive_subject_still_names_its_own_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P1, "Decoy Creature", 2, 2).id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    let sphinx = scenario
        .add_creature_from_oracle(P0, "Cerulean Sphinx", 5, 5, CERULEAN_SPHINX)
        .id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: sphinx,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the Sphinx's ability can be activated");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects.get(&sphinx).map(|obj| obj.zone),
        Some(Zone::Library),
        "the Sphinx itself is shuffled into its owner's library"
    );
    assert_eq!(
        runner.state().objects.get(&decoy).map(|obj| obj.zone),
        Some(Zone::Battlefield),
        "no other permanent is moved — the pronoun names the source, not a parent target"
    );
}

/// CR 108.3 + CR 400.3 + CR 121.1: the source's owner shuffles and draws,
/// even when the activating player controls a permanent owned by another seat.
#[test]
fn gandalf_wandering_wizard_owner_shuffles_and_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gandalf = scenario
        .add_creature_from_oracle(
            P1, "Gandalf, Wandering Wizard", 4, 5,
            "Ward {3} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {3}.)\n{6}: Gandalf's owner shuffles him into their library and draws three cards.",
        )
        .controlled_by(P0)
        .id();
    for _ in 0..12 {
        scenario.add_card_to_library_top(P1, "Island");
    }
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let ability_index = runner.state().objects[&gandalf]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("Gandalf's activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: gandalf,
            ability_index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Gandalf's six-mana activation");
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == gandalf));
    let mut events = Vec::new();
    while !runner.state().stack.is_empty() {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("resolve activation")
                .events,
        );
    }
    assert_eq!(runner.state().players[1].hand.len(), 3);
    assert!(runner.state().players[0].hand.is_empty());
    assert_eq!(runner.state().objects[&gandalf].zone, Zone::Library);
    assert!(runner.state().players[1].library.contains(&gandalf));
    let shuffles: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert!(!shuffles.is_empty());
    assert!(shuffles.iter().all(|player| *player == P1), "{shuffles:?}");
    assert_eq!(events.iter().filter(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::Shuffle, source_id, .. } if *source_id == gandalf
    )).count(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().resolution_stack.is_empty());
}

/// CR 701.24c: the explicit shuffle must execute, not merely the zone-move tail.
#[test]
fn beacon_of_destruction_shuffles_into_its_owners_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let beacon = scenario.add_spell_to_hand_from_oracle(
        P0, "Beacon of Destruction", true,
        "Beacon of Destruction deals 5 damage to any target. Shuffle Beacon of Destruction into its owner's library.",
    ).with_mana_cost(parse_mtgjson_mana_cost("{3}{R}{R}")).id();
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let events = runner
        .cast(beacon)
        .target_object(victim)
        .resolve()
        .events()
        .to_vec();
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert!(runner.state().players[0].library.contains(&beacon));
    let shuffles: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    // FU-2: the existing automatic zone-move shuffle duplicates the explicit
    // instruction. This row deliberately does not depend on that duplicate.
    assert!(!shuffles.is_empty() && shuffles.iter().all(|player| *player == P0));
    assert_eq!(events.iter().filter(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::Shuffle, source_id, .. } if *source_id == beacon
    )).count(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

/// CR 108.3 + CR 400.3: a replacement body's source keeps its owner without
/// an incarnation stamp. Diabolic Edict avoids indestructible and protection.
#[test]
fn darksteel_colossus_replacement_shuffles_owners_library() {
    assert_edict_replacement_owner(
        "Darksteel Colossus", 11,
        "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)\nIndestructible (Damage and effects that say \"destroy\" don't destroy this creature.)\nIf Darksteel Colossus would be put into a graveyard from anywhere, reveal Darksteel Colossus and shuffle it into its owner's library instead.",
    );
}

#[test]
fn progenitus_replacement_shuffles_owners_library() {
    assert_edict_replacement_owner(
        "Progenitus", 10,
        "Protection from everything\nIf Progenitus would be put into a graveyard from anywhere, reveal Progenitus and shuffle it into its owner's library instead.",
    );
}

fn assert_edict_replacement_owner(name: &str, size: i32, oracle: &str) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let body = scenario
        .add_creature_from_oracle(P1, name, size, size, oracle)
        .controlled_by(P0)
        .id();
    let edict = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Diabolic Edict",
            true,
            "Target player sacrifices a creature of their choice.",
        )
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{B}"))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&body].controller, P0);
    assert_eq!(runner.state().objects[&body].owner, P1);
    let events = runner
        .cast(edict)
        .target_player(P0)
        .resolve()
        .events()
        .to_vec();
    assert_eq!(runner.state().objects[&body].zone, Zone::Library);
    assert!(runner.state().players[1].library.contains(&body));
    let shuffles: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert!(!shuffles.is_empty(), "replacement shuffle reached");
    assert!(shuffles.iter().all(|player| *player == P1), "{shuffles:?}");
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::Shuffle, source_id, .. } if *source_id == body
    )), "explicit replacement Shuffle reached");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

/// CR 608.2c + CR 400.3: the source owner, not the preceding exile target's
/// owner or the trigger controller, shuffles Etrata.
///
/// Etrata's printed "That player loses the game if they own three or more
/// exiled cards with hit counters on them" currently loses its condition in a
/// pre-existing misparse (FU-4). This owner-binding row passes with or without
/// that loss; if it occurs, the targeted exile must precede it.
#[test]
fn etrata_three_player_shuffles_into_owner_library() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let etrata = scenario.add_creature_from_oracle(
        P2, "Etrata, the Silencer", 3, 5,
        "Etrata can't be blocked.\nWhenever Etrata deals combat damage to a player, exile target creature that player controls and put a hit counter on that card. That player loses the game if they own three or more exiled cards with hit counters on them. Etrata's owner shuffles Etrata into their library.",
    ).controlled_by(P0).as_legendary().id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let other = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(etrata, AttackTarget::Player(P1))])
        .unwrap();
    drive_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. })
    });
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        unreachable!();
    };
    assert_eq!(*player, P0);
    assert_eq!(target_slots.len(), 1);
    assert_eq!(target_slots[0].legal_targets.len(), 2);
    for target in [victim, other] {
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(target)));
    }
    let mut events = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .unwrap()
        .events;
    while !runner.state().stack.is_empty() {
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    let exile = events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Exile,
                    ..
                } if *object_id == victim
            )
        })
        .expect("the targeted exile, not elimination, moves the victim");
    if let Some(loss) = events.iter().position(|event| {
        matches!(
            event,
            GameEvent::PlayerLost { player_id, .. } | GameEvent::PlayerEliminated { player_id }
                if *player_id == P1
        )
    }) {
        assert!(
            exile < loss,
            "the printed exile precedes the unrelated FU-4 loss"
        );
    }
    assert_eq!(runner.state().objects[&etrata].zone, Zone::Library);
    assert!(runner.state().players[2].library.contains(&etrata));
    assert!(!runner.state().players[0].library.contains(&etrata));
    assert!(!runner.state().players[1].library.contains(&etrata));
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::Shuffle, source_id, .. }
                if *source_id == etrata
        )),
        "the production trigger reaches its explicit Shuffle"
    );
    let shuffles: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert!(!shuffles.is_empty(), "the printed shuffle executes");
    assert!(shuffles.iter().all(|player| *player == P2), "{shuffles:?}");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

/// CR 400.7e + CR 603.6: a dies trigger finds its source's graveyard successor.
/// CR 115.10a + CR 400.3: that reference is untargeted and uses its owner's library.
#[test]
fn alabaster_dragon_death_trigger_shuffles_owners_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature_from_oracle(
            P1,
            "Alabaster Dragon",
            4,
            4,
            "Flying\nWhen this creature dies, shuffle it into its owner's library.",
        )
        .controlled_by(P0)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{B}{B}"))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(murder).target_object(dragon).commit();
    let mut events = Vec::new();
    while !runner.state().stack.is_empty() {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "the untargeted dies trigger must not ask for a target: {:?}",
            runner.state().waiting_for
        );
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert_eq!(runner.state().objects[&dragon].zone, Zone::Library);
    assert!(runner.state().players[1].library.contains(&dragon));
    let shuffles: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert!(!shuffles.is_empty());
    assert!(shuffles.iter().all(|player| *player == P1), "{shuffles:?}");
    assert_eq!(events.iter().filter(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::Shuffle, source_id, .. } if *source_id == dragon
    )).count(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

/// CR 115.10a + CR 400.3: the untargeted activation moves the Aura's host,
/// not its source. CR 701.67a: ordinary mana can pay the full waterbend cost.
#[test]
fn watery_grasp_activation_shuffles_enchanted_creature_into_owners_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .controlled_by(P0)
        .id();
    let aura = scenario.add_spell_to_hand(P0, "Watery Grasp", false)
        .as_enchantment().with_subtypes(vec!["Aura"])
        .with_mana_cost(parse_mtgjson_mana_cost("{U}"))
        .from_oracle_text_with_keywords(&["enchant"],
        "Enchant creature\nEnchanted creature doesn't untap during its controller's untap step.\nWaterbend {5}: Enchanted creature's owner shuffles it into their library. (While paying a waterbend cost, you can tap your artifacts and creatures to help. Each one pays for {1}.)",
    ).id();
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(aura).target_object(host).resolve();
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(host))
    );
    let ability_index = runner.state().objects[&aura]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("waterbend activation");
    runner
        .act(GameAction::ActivateAbility {
            source_id: aura,
            ability_index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner
        .act(GameAction::PassPriority)
        .expect("pay waterbend from the mana pool");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the host reference is not targeted: {:?}",
        runner.state().waiting_for
    );
    let mut events = Vec::new();
    while !runner.state().stack.is_empty() {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert_eq!(runner.state().objects[&host].zone, Zone::Library);
    assert!(runner.state().players[1].library.contains(&host));
    assert_eq!(runner.state().objects[&aura].zone, Zone::Graveyard);
    assert!(!runner.state().players[1].library.contains(&aura));
    let shuffles: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert!(!shuffles.is_empty());
    assert!(shuffles.iter().all(|player| *player == P1), "{shuffles:?}");
    assert_eq!(events.iter().filter(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::Shuffle, source_id, .. } if *source_id == aura
    )).count(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

const LEMBAS: &str = "When this artifact enters, scry 1, then draw a card.\n{2}, {T}, Sacrifice this artifact: You gain 3 life.\nWhen this artifact is put into a graveyard from the battlefield, its owner shuffles it into their library.";
const INFERNO_HELLION: &str = "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)\nAt the beginning of each end step, if this creature attacked or blocked this turn, its owner shuffles it into their library.";
const GRUESOME_ENCORE: &str = "Put target creature card from an opponent's graveyard onto the battlefield under your control. It gains haste. Exile it at the beginning of the next end step. If that creature would leave the battlefield, exile it instead of putting it anywhere else.";

fn finish_owner_shuffle(runner: &mut GameRunner, source: ObjectId, owner: PlayerId) {
    let mut events = Vec::new();
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority")
                .events,
        );
    }
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_owner_shuffle(&events, source, owner);
}

fn assert_owner_shuffle(events: &[GameEvent], source: ObjectId, owner: PlayerId) {
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::EffectResolved { kind: EffectKind::Shuffle, source_id, .. } if *source_id == source
    )));
    let shufflers: std::collections::BTreeSet<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    // CR 701.24a: the instruction names the source's owner as the shuffler.
    assert_eq!(shufflers, [owner].into());
}

#[test]
fn lembas_departure_shuffles_by_owner_after_controller_sacrifices() {
    use engine::types::game_state::CastPaymentMode;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lembas = scenario
        .add_artifact_from_oracle(P0, "Lembas", LEMBAS)
        .controlled_by(P1)
        .id();
    let island = scenario.add_card_to_library_top(P0, "Island");
    let forest = scenario.add_card_to_library_top(P1, "Forest");
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 2],
    );
    let mut runner = scenario.build();
    runner
        .act(GameAction::PassPriority)
        .expect("give P1 priority");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    let life_before = runner.state().players[1].life;
    let events = runner
        .act(GameAction::ActivateAbility {
            source_id: lembas,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 pays Lembas's activation costs")
        .events;
    // CR 701.21a: P1 sacrifices the Lembas they control as a cost.
    assert!(events
        .iter()
        .any(|event| matches!(event, GameEvent::ZoneChanged {
        object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, record,
    } if *object_id == lembas && record.controller == P1)));
    assert!(runner.state().players[0].graveyard.contains(&lembas));
    // CR 603.3a + CR 603.6c + CR 603.10a: the departure looks back to P1's control.
    assert_eq!(runner.state().stack.len(), 2);
    let departure = &runner.state().stack[1];
    assert_eq!(departure.source_id, lembas);
    assert!(matches!(
        departure.kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    assert_eq!(departure.controller, P1);

    finish_owner_shuffle(&mut runner, lembas, P0);
    assert_eq!(runner.state().players[1].life, life_before + 3);
    // CR 400.7e + CR 400.3: the exact graveyard successor goes to P0's library.
    assert!(runner.state().players[0].library.contains(&lembas));
    assert!(runner.state().players[0].library.contains(&island));
    assert_eq!(
        runner.state().players[1]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [forest]
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HellionCombat {
    Attacks,
    Blocks,
    SitsOut,
}

#[test]
fn inferno_hellion_owner_performs_move_redirected_to_exile_and_still_shuffles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hellion = scenario
        .add_creature_to_graveyard(P1, "Inferno Hellion", 7, 3)
        .from_oracle_text(INFERNO_HELLION)
        .id();
    let encore = scenario
        .add_spell_to_hand_from_oracle(P0, "Gruesome Encore", false, GRUESOME_ENCORE)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );
    scenario.add_creature(P1, "Available Blocker", 0, 8);
    scenario.add_card_to_library_top(P0, "Island");
    let forest = scenario.add_card_to_library_top(P1, "Forest");
    let mut runner = scenario.build();
    let outcome = runner.cast(encore).target_object(hellion).resolve();
    outcome.assert_zone(&[hellion], Zone::Battlefield);
    assert_eq!(outcome.state().objects[&hellion].controller, P0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));

    runner.advance_to_combat();
    // CR 508.1a: record Hellion's attack through the public declaration.
    runner
        .declare_attackers(&[(hellion, AttackTarget::Player(P1))])
        .unwrap();
    drive_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    runner.declare_blockers(&[]).unwrap();
    drive_until(&mut runner, |state| state.phase == Phase::End);

    // CR 603.3b: P0 puts Encore's delayed trigger below Hellion's trigger.
    let WaitingFor::OrderTriggers {
        player: P0,
        triggers,
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected P0 to order both end-step triggers: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(triggers.len(), 2);
    let hellion_index = triggers
        .iter()
        .position(|trigger| trigger.source_id == hellion)
        .unwrap();
    let encore_index = triggers
        .iter()
        .position(|trigger| trigger.source_id == encore)
        .unwrap();
    runner
        .act(GameAction::OrderTriggers {
            order: vec![encore_index, hellion_index],
        })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(runner.state().stack[0].source_id, encore);
    assert_eq!(runner.state().stack[1].source_id, hellion);
    assert_eq!(runner.state().stack[1].controller, P0);

    let mut events = Vec::new();
    for expected_player in [P0, P1] {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == expected_player
        ));
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, encore);
    assert!(runner.state().resolution_stack.is_empty());
    // CR 701.24c: the owner shuffles even when Encore redirects the move.
    assert_owner_shuffle(&events, hellion, P1);
    let exiled = &runner.state().objects[&hellion];
    // CR 614.1a: Encore replaces the destination of the owner's move.
    assert_eq!(exiled.zone, Zone::Exile);
    assert_eq!(
        exiled.exiled_by,
        Some(P1),
        "Hellion's owner performs the redirected move"
    );
    assert!(runner.state().players[1]
        .library
        .iter()
        .copied()
        .eq([forest]));
    drive_until(&mut runner, |state| state.stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().resolution_stack.is_empty());
}

#[test]
fn inferno_hellion_end_step_shuffles_by_owner_after_its_own_combat() {
    for (combat, bounce) in [
        (HellionCombat::Attacks, false),
        (HellionCombat::Blocks, false),
        (HellionCombat::Attacks, true),
        (HellionCombat::SitsOut, false),
    ] {
        let active_player = if combat == HellionCombat::Blocks {
            P1
        } else {
            P0
        };
        let defender = if active_player == P0 { P1 } else { P0 };
        let mut scenario = GameScenario::new();
        scenario.at_phase(if combat == HellionCombat::Blocks {
            Phase::PostCombatMain
        } else {
            Phase::PreCombatMain
        });
        let hellion = scenario
            .add_creature_from_oracle(P1, "Inferno Hellion", 7, 3, INFERNO_HELLION)
            .controlled_by(P0)
            .id();
        let other = scenario
            .add_creature(active_player, "Other Attacker", 1, 1)
            .id();
        if active_player == P0 {
            scenario.add_creature(P1, "Available Blocker", 0, 8);
        }
        let island = scenario.add_card_to_library_top(P0, "Island");
        let forest = scenario.add_card_to_library_top(P1, "Forest");
        if active_player == P1 {
            scenario.add_card_to_library_top(P1, "Plains");
        }
        let unsummon = if bounce {
            let spell = scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Unsummon",
                    true,
                    "Return target creature to its owner's hand.",
                )
                .with_mana_cost(parse_mtgjson_mana_cost("{U}"))
                .id();
            scenario.add_basic_land(P0, ManaColor::Blue);
            Some(spell)
        } else {
            None
        };
        let mut runner = scenario.build();
        drive_until(&mut runner, |state| {
            state.active_player == active_player && state.phase == Phase::PreCombatMain
        });
        runner.advance_to_combat();
        let attacker = if combat == HellionCombat::Attacks {
            hellion
        } else {
            other
        };
        // CR 508.1a + CR 509.1a: record participation through public declarations.
        runner
            .declare_attackers(&[(attacker, AttackTarget::Player(defender))])
            .expect("declare attacker");
        drive_until(&mut runner, |state| {
            matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. })
        });
        let blocks = [(hellion, attacker)];
        runner
            .declare_blockers(if combat == HellionCombat::Blocks {
                &blocks
            } else {
                &[]
            })
            .expect("declare blockers");
        drive_until(&mut runner, |state| state.phase == Phase::End);

        // CR 513.1 + CR 603.4: only the Hellion that participated queues a trigger.
        if combat == HellionCombat::SitsOut {
            assert!(runner.state().creatures_attacked_this_turn.contains(&other));
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert_eq!(runner.state().objects[&hellion].zone, Zone::Battlefield);
            continue;
        }
        assert_eq!(runner.state().stack.len(), 1, "{combat:?}");
        let trigger = runner.state().stack[0].clone();
        assert_eq!(trigger.source_id, hellion);
        assert!(matches!(
            trigger.kind,
            StackEntryKind::TriggeredAbility { .. }
        ));
        assert_eq!(trigger.controller, P0);
        let incarnation = runner.state().objects[&hellion].incarnation;

        if let Some(unsummon) = unsummon {
            let mut cast = runner.cast(unsummon).target_object(hellion).commit();
            assert_eq!(cast.state().stack.len(), 2);
            let mut events = Vec::new();
            for _ in 0..8 {
                if cast.state().stack.len() == 1 {
                    break;
                }
                events.extend(
                    cast.act(GameAction::PassPriority)
                        .expect("resolve Unsummon")
                        .events,
                );
            }
            assert!(events
                .iter()
                .any(|event| matches!(event, GameEvent::ZoneChanged {
                object_id, from: Some(Zone::Battlefield), to: Zone::Hand, ..
            } if *object_id == hellion)));
            assert_eq!(cast.state().stack.len(), 1);
            assert_eq!(cast.state().stack[0].id, trigger.id);
            assert_eq!(cast.state().stack[0].controller, P0);
            assert!(cast.state().players[1].hand.contains(&hellion));
            assert_ne!(cast.state().objects[&hellion].incarnation, incarnation);
        }
        let before_resolution = runner.state().objects[&hellion].incarnation;
        finish_owner_shuffle(&mut runner, hellion, P1);
        // CR 400.7 + CR 701.24c: the bounced incarnation stays in hand, and
        // the owner still shuffles. The original incarnation can move otherwise.
        if bounce {
            assert_eq!(
                runner.state().objects[&hellion].incarnation,
                before_resolution
            );
            assert!(runner.state().players[1].hand.contains(&hellion));
        } else {
            assert!(runner.state().players[1].library.contains(&hellion));
        }
        assert!(runner.state().players[0].library.contains(&island));
        assert!(runner.state().players[1].library.contains(&forest));
        assert!(!runner.state().players[0].library.contains(&hellion));
    }
}

#[test]
fn inferno_hellion_end_step_shuffles_by_owner_after_becoming_a_land() {
    const MOON_ORACLE: &str = "Enchant creature, land, or planeswalker\nEnchanted permanent is a colorless land with \"{T}: Add {C}\" and loses all other card types and abilities.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hellion = scenario
        .add_creature_from_oracle(P1, "Inferno Hellion", 7, 3, INFERNO_HELLION)
        .controlled_by(P0)
        .id();
    scenario.add_artifact_from_oracle(
        P0,
        "Vedalken Orrery",
        "You may cast spells as though they had flash.",
    );
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let moon = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Imprisoned in the Moon", MOON_ORACLE)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{U}"))
        .from_oracle_text_with_keywords(&["enchant"], MOON_ORACLE)
        .id();
    let forest = scenario.add_card_to_library_top(P1, "Forest");
    let plains = scenario.add_card_to_library_top(P1, "Plains");
    scenario.add_creature(P1, "Available Blocker", 0, 8);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(hellion, AttackTarget::Player(P1))])
        .expect("declare Hellion as attacker");
    drive_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    runner.declare_blockers(&[]).expect("declare no blockers");
    drive_until(&mut runner, |state| state.phase == Phase::End);

    // CR 603.4: The attack satisfies the intervening-if at trigger admission.
    assert_eq!(runner.state().stack.len(), 1);
    let trigger = &runner.state().stack[0];
    assert_eq!(trigger.source_id, hellion);
    assert_eq!(trigger.controller, P0);
    assert!(matches!(
        trigger.kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    let trigger_id = trigger.id;
    let incarnation = runner.state().objects[&hellion].incarnation;

    // CR 609.4 + CR 702.8a: Orrery lets P0 cast the Aura as though it had flash.
    let mut cast = runner.cast(moon).target_object(hellion).commit();
    assert_eq!(cast.state().stack.len(), 2);
    assert_eq!(cast.state().stack[1].id, moon);
    for _ in 0..8 {
        if cast.state().stack.len() == 1 {
            break;
        }
        cast.act(GameAction::PassPriority)
            .expect("resolve the Aura");
    }
    // CR 113.7a + CR 613.1d: The queued trigger survives the type change.
    assert_eq!(cast.state().stack.len(), 1);
    assert_eq!(cast.state().stack[0].id, trigger_id);
    assert_eq!(
        cast.state().objects[&moon].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(hellion))
    );
    assert_eq!(
        cast.state().objects[&hellion].card_types.core_types,
        [engine::types::card_type::CoreType::Land]
    );
    assert_eq!(cast.state().objects[&hellion].incarnation, incarnation);

    // CR 700.7 + CR 603.4: The recheck uses that permanent's combat history.
    finish_owner_shuffle(&mut runner, hellion, P1);
    assert!(runner.state().players[1].library.contains(&hellion));
    assert!(runner.state().players[1].library.contains(&forest));
    assert!(runner.state().players[1].library.contains(&plains));
}
