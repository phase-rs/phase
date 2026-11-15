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
use engine::types::ability::{DelayedTriggerCondition, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
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
