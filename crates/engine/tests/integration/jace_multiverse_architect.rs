//! Jace, Multiverse Architect — "At the beginning of combat on each opponent's
//! turn, they may pay {2}. If they don't, creatures they control can't attack
//! Jaces you control this turn."
//!
//! The trailing "creatures they control can't attack [Subtype]s you control
//! this turn" was a parser `Effect::Unimplemented` leaf. This suite locks in:
//!   1. PARSE (row 1): the trigger chain lowers to `PayCost { payer:
//!      TriggeringPlayer }` (parent) with an `AddRestriction { ScopedPlayer,
//!      EndOfTurn, Attack { Planeswalker { subtype: Some(Jace) } } }` leaf,
//!      zero `Unimplemented` in the trigger chain.
//!   2. RUNTIME (rows 2–6): decline bans attacks on Jace planeswalkers only,
//!      paid {2} suppresses the ban, the prompt addresses the triggering
//!      opponent, and per-opponent instances are independent (row 4).
//!   3. HOSTILE/TEMPORAL (rows 2b/2c/2d): fire-time controller latch,
//!      late/animated attackers, live-controller defended set.
//!   4. EXPIRY (row 3): the "this turn" ban prunes at cleanup (CR 514.2).
//!   5. PAYER FAMILY (row 5c): sibling "they may pay" reflexive triggers keep
//!      the triggering-player payer; declared-target payers are untouched.
//!   6. FALLBACK (row 7): unbound `ScopedPlayer` pins the controller.
//!   7. HONESTY (row 8): non-planeswalker defended subtypes stay
//!      `Unimplemented` (fail-closed, red).
//!
//! CR 508.1c: attack restrictions checked at declare-attackers. CR 508.5: the
//! defended planeswalker compares on controller (CR 109.4). CR 205.3j:
//! planeswalker subtypes. CR 514.2: end-of-turn expiry. CR 118.12:
//! the named "they" both chooses and pays. (CR numbers inherited from the
//! in-code authorities cited in the implementation; docs/MagicCompRules.txt is
//! absent in this checkout.)

use engine::game::combat::{attack_tax_is_affordable, declare_attackers, AttackTarget};
use engine::game::effects::add_restriction;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::game::turns::execute_cleanup;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, ChosenAttribute, Effect, GameRestriction,
    ProhibitedActivity, ResolvedAbility, RestrictionExpiry, RestrictionPlayerScope,
    StaticCondition, TargetFilter, UnlessPayScaling,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::{AttackTargetFilter, TriggerMode};
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text (Scryfall `Jace, Multiverse Architect`).
const JACE_ORACLE: &str = "At the beginning of combat on each opponent's turn, they may pay {2}. If they don't, creatures they control can't attack Jaces you control this turn.\n+1: Draw two cards, then put a card from your hand on the bottom of your library.\n-3: Exile another target planeswalker or creature you control. Reveal cards from the top of your library until you reveal a creature or planeswalker card. Put that card onto the battlefield and the rest on the bottom of your library in a random order.\nJace, Multiverse Architect can be your commander.";

fn floating_mana(units: &[ManaType]) -> Vec<ManaUnit> {
    units
        .iter()
        .map(|ty| ManaUnit::new(*ty, ObjectId(0), false, vec![]))
        .collect()
}

fn mana_total(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool
        .total()
}

/// Walk an execute chain root-to-leaf.
fn chain_nodes(mut ability: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut nodes = vec![ability];
    while let Some(sub) = ability.sub_ability.as_deref() {
        nodes.push(sub);
        ability = sub;
    }
    nodes
}

/// Find Jace's beginning-of-combat trigger in a full-card parse.
fn find_jace_trigger(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> &engine::types::ability::TriggerDefinition {
    parsed
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::Phase) && t.phase == Some(Phase::BeginCombat))
        .expect("Jace has a beginning-of-combat phase trigger")
}

/// Build a 2-player board: P0 controls Jace (verbatim Oracle text), P1 has an
/// attacker. Active player is P1 (the opponent's turn), at beginning of combat.
fn jace_scenario() -> (GameRunner, ObjectId, ObjectId) {
    jace_scenario_with_pool(&[])
}

fn jace_scenario_with_pool(p1_pool: &[ManaType]) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jace = scenario
        .add_planeswalker_from_oracle(P0, "Jace, Multiverse Architect", "Jace", 4, JACE_ORACLE)
        .id();
    let attacker = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    scenario.with_mana_pool(P1, floating_mana(p1_pool));
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().phase = Phase::BeginCombat;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, jace, attacker)
}

/// Imperative planeswalker (sibling fixtures whose Oracle text is not under
/// test — mirrors the Willie suite's `pw` fixture).
fn add_planeswalker(
    state: &mut GameState,
    controller: PlayerId,
    name: &str,
    subtype: &str,
) -> ObjectId {
    let card = CardId(state.next_object_id);
    let id = create_object(state, card, controller, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Planeswalker];
    obj.card_types.subtypes = vec![subtype.to_string()];
    obj.base_card_types = obj.card_types.clone();
    id
}

fn add_battle(
    state: &mut GameState,
    controller: PlayerId,
    protector: PlayerId,
    name: &str,
) -> ObjectId {
    let card = CardId(state.next_object_id);
    let id = create_object(state, card, controller, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    // A real Siege (CR 310.9b/310.9d/310.12a): battles without defense
    // counters and a protector are not attackable at all, so a bare
    // Battle-typed object would fail the legality check for reasons unrelated
    // to the ban. The protector must be an opponent of the controller (CR
    // 310.12a) — the SBA reassigns an illegal protector choice, measured —
    // so callers must choose a legal pair.
    obj.card_types.core_types = vec![CoreType::Battle];
    obj.card_types.subtypes = vec!["Siege".to_string()];
    obj.base_card_types = obj.card_types.clone();
    obj.defense = Some(5);
    obj.base_defense = Some(5);
    obj.counters.insert(CounterType::Defense, 5);
    obj.chosen_attributes
        .push(ChosenAttribute::Player(protector));
    id
}

/// Imperative 2/2 bear under `controller` — attack fixtures for moved-Jace
/// checks, where the attacker's controller must differ from the Jace's (a
/// self-attack is illegal by target validity regardless of any ban, so it can
/// neither prove nor disprove the defended set).
fn add_bear(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
    let card = CardId(state.next_object_id);
    let id = create_object(state, card, controller, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.summoning_sick = false;
    id
}

/// Fire Jace's trigger for the current active player (production phase event).
fn fire_begin_combat(runner: &mut GameRunner) {
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::BeginCombat,
        }],
    );
}

/// Pass priority until the {2} optional-payment prompt surfaces (or the stack
/// drains, which fails loudly at the call site).
fn drive_to_optional_prompt(runner: &mut GameRunner) {
    for _ in 0..100 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            return;
        }
        if runner.state().stack.is_empty() {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            return;
        }
    }
}

/// Drain to an idle empty stack. A further `OptionalEffectChoice` here is
/// UNEXPECTED and fails loudly: every decision under test was already made by
/// the caller.
fn drive_to_idle(runner: &mut GameRunner) {
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::OptionalEffectChoice { .. } => {
                panic!(
                    "unexpected optional-effect prompt during drive_to_idle: {:?} — \
                     every decision this test exercises must already be settled by now",
                    runner.state().waiting_for
                );
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
        }
    }
    panic!("resolution did not settle within the budget");
}

/// Assert the {2} prompt addresses `player`, answer it, and drain.
fn answer_optional(runner: &mut GameRunner, player: PlayerId, accept: bool) {
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice {
            player: prompted, ..
        } => {
            assert_eq!(
                prompted, player,
                "the {{2}} payment choice must be offered to the triggering opponent"
            );
        }
        other => panic!("expected the {{2}} optional payment prompt, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("answering the {2} payment succeeds");
    drive_to_idle(runner);
    assert!(
        runner.state().stack.is_empty(),
        "resolution must settle with an empty stack"
    );
}

fn attack_is_legal(
    runner: &GameRunner,
    controller: PlayerId,
    attacker: ObjectId,
    target: AttackTarget,
) -> bool {
    let mut state = runner.state().clone();
    state.active_player = controller;
    let mut events = Vec::new();
    declare_attackers(&mut state, &[(attacker, target)], &mut events).is_ok()
}

fn has_attack_ban(runner: &GameRunner) -> bool {
    runner.state().restrictions.iter().any(|r| {
        matches!(
            r,
            GameRestriction::ProhibitActivity {
                activity: ProhibitedActivity::Attack { .. },
                ..
            }
        )
    })
}

/// Row 1 — RED-first parse probe on the verbatim Jace clause: the trigger
/// chain lowers to an optional parent with a subtype-defended `ScopedPlayer`
/// restriction leaf, zero `Unimplemented`.
///
/// Payer assertion (CR 118.12): the root `{2}` payment is offered to (and
/// paid by) the triggering opponent. Fixed in round 2 via the
/// `lower_trigger_effect_chain` post-pass re-stamp (the r5
/// `ReflexiveParent::MayPay`-arm seam was proven unreachable for "they may"
/// shapes and reverted); revert-failing — disabling the post-pass flips the
/// payer back to `Controller` (observed).
#[test]
fn jace_trigger_clause_parses_to_subtype_defended_restriction() {
    let parsed = parse_oracle_text(
        JACE_ORACLE,
        "Jace, Multiverse Architect",
        &[],
        &["Planeswalker".to_string()],
        &["Jace".to_string()],
    );
    let trigger = find_jace_trigger(&parsed);
    let parent = trigger
        .execute
        .as_deref()
        .expect("trigger has an execute chain");

    // Parent: optional {2} payment offered to (and paid by) the triggering
    // opponent — CR 118.12, the named "they" both chooses and pays.
    match parent.effect.as_ref() {
        Effect::PayCost { payer, .. } => {
            assert_eq!(
                payer,
                &TargetFilter::TriggeringPlayer,
                "the opponent pays {{2}}, not Jace's controller"
            );
        }
        other => panic!("expected PayCost parent, got {other:?}"),
    }
    assert!(parent.optional, "reach-guard: the parent carries the may");
    assert_eq!(
        parent.optional_player,
        Some(TargetFilter::TriggeringPlayer),
        "the choice is offered to the triggering opponent"
    );

    // Reflexive sub-chain: "If they don't" gates the ban.
    let sub = parent
        .sub_ability
        .as_deref()
        .expect("the ban remains chained under the payment");
    assert_eq!(
        sub.condition,
        Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed())
        }),
        "the ban is gated on the opponent having declined payment"
    );

    // Leaf: player-scoped this-turn ban defending Jace planeswalkers.
    let nodes = chain_nodes(parent);
    let leaf = nodes.last().unwrap();
    assert!(
        !nodes
            .iter()
            .any(|n| matches!(n.effect.as_ref(), Effect::Unimplemented { .. })),
        "zero Unimplemented in the trigger chain: {:?}",
        nodes
            .iter()
            .map(|n| format!("{:?}", n.effect))
            .collect::<Vec<_>>()
    );
    match leaf.effect.as_ref() {
        Effect::AddRestriction {
            restriction:
                GameRestriction::ProhibitActivity {
                    affected_players,
                    expiry,
                    activity:
                        ProhibitedActivity::Attack {
                            defended,
                            protected_player,
                        },
                    ..
                },
        } => {
            // HOSTILE (Willie legacy anaphor): "that player"/"they" with no
            // creatures-subject binds `ParentTargetedPlayer` — Jace's
            // creatures-subject must bind `ScopedPlayer` instead.
            assert_eq!(
                *affected_players,
                RestrictionPlayerScope::ScopedPlayer,
                "'creatures they control' binds the phase's active player"
            );
            assert_eq!(
                *expiry,
                RestrictionExpiry::EndOfTurn,
                "this-turn cleanup owns expiry (CR 514.2)"
            );
            assert_eq!(
                *defended,
                AttackTargetFilter::Planeswalker {
                    subtype: Some("Jace".to_string())
                },
                "the defended scope names the Jace subtype"
            );
            assert_eq!(
                *protected_player, None,
                "protection snapshots at resolution, not at parse"
            );
        }
        other => panic!("expected AddRestriction(Attack) leaf, got {other:?}"),
    }
    // HOSTILE (Advokist bounded form): the "until your next turn" shape stamps
    // `UntilNextTurnOf` — the "this turn" shape must carry NO duration.
    assert_eq!(
        leaf.duration, None,
        "the Willie/Sandswirl this-turn pattern owns cleanup via expiry, not duration"
    );
}

/// Row 2 + scope matrix — declining bans attacks on Jace planeswalkers ONLY:
/// the player, a non-Jace planeswalker, and a battle stay attackable.
///
/// REVERT-FAIL (matcher arm): the Jace attack goes through (ban matches
/// nothing). The three legal attacks are the paired positive reach-guards —
/// they prove the pipeline reached declaration rather than short-circuiting.
#[test]
fn jace_decline_bans_jace_planeswalker_attacks_only() {
    let (mut runner, jace, attacker) = jace_scenario();
    let chandra = add_planeswalker(runner.state_mut(), P0, "Chandra", "Chandra");
    // Siege controlled by P1 and protected by P0: the only legal 2-player
    // assignment (CR 310.12a forbids controller-as-protector; the SBA
    // reassigns it, measured), and CR 310.9b lets P1 attack their own battle
    // since its protector is a different player. The ban's affected player is
    // P1 and the arm never matches Battle, so the verdict is
    // controller-independent — this exercises the Battle-pairing row with the
    // affected attacker.
    let battle = add_battle(runner.state_mut(), P1, P0, "Invasion");

    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    answer_optional(&mut runner, P1, false);

    assert!(
        has_attack_ban(&runner),
        "declining installs the attack prohibition"
    );
    // REVERT-FAIL: without the matcher arm this attack is legal.
    assert!(
        !attack_is_legal(&runner, P1, attacker, AttackTarget::Planeswalker(jace)),
        "CR 508.1c: P1 can't attack a Jace P0 controls"
    );
    // Scope-matrix negatives — same attacker, sibling targets stay legal.
    assert!(
        attack_is_legal(&runner, P1, attacker, AttackTarget::Player(P0)),
        "the ban defends Jaces, not their controller"
    );
    assert!(
        attack_is_legal(&runner, P1, attacker, AttackTarget::Planeswalker(chandra)),
        "a non-Jace planeswalker stays attackable"
    );
    assert!(
        attack_is_legal(&runner, P1, attacker, AttackTarget::Battle(battle)),
        "CR 310.5: the subtype scope never matches a battle"
    );
}

/// Row 2b — fire-time latch: Jace changes controller AFTER the trigger fires
/// but BEFORE the decline resolves. The ban defends the OLD controller's
/// Jaces (protection snapshots `original_controller` at creation; triggered
/// abilities are not re-stamped at resolution).
///
/// REVERT-FAIL: a live-controller snapshot at creation defends the NEW
/// controller's Jaces instead (stolen Jace banned, remaining Jace free).
#[test]
fn jace_ban_latches_fire_time_controller() {
    let (mut runner, jace_a, attacker) = jace_scenario();
    let jace_b = add_planeswalker(runner.state_mut(), P0, "Jace B", "Jace");

    fire_begin_combat(&mut runner);
    // Steal Jace A after the trigger fired, before the decline resolves.
    runner
        .state_mut()
        .objects
        .get_mut(&jace_a)
        .unwrap()
        .controller = P1;
    // Enemy attacker for the moved Jace (P1 attacking its own Jace would be
    // a self-attack — illegal regardless of the ban).
    let p0_bear = add_bear(runner.state_mut(), P0, "P0 Bear");

    drive_to_optional_prompt(&mut runner);
    answer_optional(&mut runner, P1, false);

    // The stolen Jace now belongs to the restricted player — attackable by
    // P0's creature.
    assert!(
        attack_is_legal(&runner, P0, p0_bear, AttackTarget::Planeswalker(jace_a)),
        "the ban defends P0's Jaces; the P1-controlled Jace is attackable"
    );
    // P0's remaining Jace stays defended.
    assert!(
        !attack_is_legal(&runner, P1, attacker, AttackTarget::Planeswalker(jace_b)),
        "P0's remaining Jace stays defended after the steal"
    );
}

/// Row 2c — affected-temporal cases: the ban is player-scoped, not a creature
/// filter. A creature P1 steals, a creature entering under P1 later, and a
/// newly animated attacker are all still banned.
///
/// REVERT-FAIL: a creature-filter implementation lets each of these through.
#[test]
fn jace_ban_covers_stolen_late_and_animated_attackers() {
    let (mut runner, jace, attacker) = jace_scenario();

    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    answer_optional(&mut runner, P1, false);
    assert!(
        !attack_is_legal(&runner, P1, attacker, AttackTarget::Planeswalker(jace)),
        "pre-change reach-guard: the initial board is banned"
    );

    // (a) P1 steals a P0 creature post-resolution — still banned.
    let stolen_card = CardId(runner.state().next_object_id);
    let stolen = create_object(
        runner.state_mut(),
        stolen_card,
        P0,
        "Stolen Bear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&stolen).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.summoning_sick = false;
        obj.controller = P1;
    }
    assert!(
        !attack_is_legal(&runner, P1, stolen, AttackTarget::Planeswalker(jace)),
        "a creature stolen after resolution is still banned"
    );

    // (b) A creature entering under P1 later that turn — still banned.
    let late_card = CardId(runner.state().next_object_id);
    let late = create_object(
        runner.state_mut(),
        late_card,
        P1,
        "Late Bear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&late).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.summoning_sick = false;
    }
    assert!(
        !attack_is_legal(&runner, P1, late, AttackTarget::Planeswalker(jace)),
        "a creature entering under P1 later that turn is still banned"
    );

    // (c) A noncreature animated under P1 mid-turn — still banned.
    let vehicle_card = CardId(runner.state().next_object_id);
    let vehicle = create_object(
        runner.state_mut(),
        vehicle_card,
        P1,
        "Crew Vehicle".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&vehicle).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.base_card_types = obj.card_types.clone();
        // Animation outcome: now a creature (power/toughness set, ready).
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.summoning_sick = false;
    }
    assert!(
        !attack_is_legal(&runner, P1, vehicle, AttackTarget::Planeswalker(jace)),
        "an animated attacker under P1 is still banned"
    );
}

/// Row 2d — defended live-vs-snapshot: AFTER the ban resolves, Jace moves
/// from P0 to P2. Legality follows the NEW controller (live comparison).
/// CR 611.2c: the ban modifies the rules of the game, so "Jaces you control"
/// is read live — not snapshotted at resolution.
///
/// The attacker is the RESTRICTED player (P1) throughout: the
/// declare-attackers gate skips the defended-scope check for an unaffected
/// attacker, so only P1's attacks can prove or disprove the defended set.
///
/// REVERT-FAIL (owner comparison): the moved Jace's owner is still P0, so an
/// owner-anchored comparison keeps it defended and the first attack stays
/// illegal. REVERT-FAIL (snapshot): a resolution-time snapshot likewise keeps
/// the moved Jace banned.
#[test]
fn jace_defended_set_follows_live_controller() {
    let (mut runner, jace, amy_attacker, _) = jace_three_player_scenario();
    let jace_b = add_planeswalker(runner.state_mut(), P0, "Jace B", "Jace");

    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    answer_optional(&mut runner, P1, false);
    assert!(
        !attack_is_legal(&runner, P1, amy_attacker, AttackTarget::Planeswalker(jace)),
        "pre-change reach-guard: Jace is banned before the move"
    );
    assert!(
        !attack_is_legal(
            &runner,
            P1,
            amy_attacker,
            AttackTarget::Planeswalker(jace_b)
        ),
        "pre-change reach-guard: Jace B is banned before the move"
    );

    // Donate Jace to P2: controller P2, owner still P0.
    runner
        .state_mut()
        .objects
        .get_mut(&jace)
        .unwrap()
        .controller = P2;
    assert!(
        attack_is_legal(&runner, P1, amy_attacker, AttackTarget::Planeswalker(jace)),
        "the moved Jace is no longer defended (defense follows the live controller, not the owner or a snapshot)"
    );
    assert!(
        !attack_is_legal(
            &runner,
            P1,
            amy_attacker,
            AttackTarget::Planeswalker(jace_b)
        ),
        "P0's remaining Jace stays defended"
    );
}

/// Row 3 — "this turn" expires at cleanup (CR 514.2): after
/// `execute_cleanup` the restriction is gone and Jace is attackable again.
///
/// REVERT-FAIL (no-expiry): the ban persists past cleanup.
#[test]
fn jace_ban_expires_at_cleanup() {
    let (mut runner, jace, attacker) = jace_scenario();

    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    answer_optional(&mut runner, P1, false);
    assert!(
        !attack_is_legal(&runner, P1, attacker, AttackTarget::Planeswalker(jace)),
        "pre-expiry reach-guard: the ban is active the same turn"
    );

    execute_cleanup(runner.state_mut(), &mut Vec::new());
    assert!(
        !has_attack_ban(&runner),
        "CR 514.2: the EndOfTurn restriction prunes at cleanup"
    );
    assert!(
        attack_is_legal(&runner, P1, attacker, AttackTarget::Planeswalker(jace)),
        "Jace is attackable again next turn"
    );
}

/// 3-player board: P0 controls Jace, P1 (Amy) and P2 (Bob) each have an
/// attacker. Active player is set per phase by the caller.
fn jace_three_player_scenario() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let jace = scenario
        .add_planeswalker_from_oracle(P0, "Jace, Multiverse Architect", "Jace", 4, JACE_ORACLE)
        .id();
    let amy_attacker = scenario.add_creature(P1, "Amy Bear", 2, 2).id();
    let bob_attacker = scenario.add_creature(P2, "Bob Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().phase = Phase::BeginCombat;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, jace, amy_attacker, bob_attacker)
}

/// Row 4 — multiplayer reach: Amy declines on her combat (Amy banned, Bob
/// free); after cleanup prunes Amy's ban, Bob declines on his combat (Bob
/// banned, Amy free). Each firing binds its own active player —
///
/// REVERT-FAIL (`OpponentsOfSourceController` affected): Bob is already banned
/// in phase 1. The Amy-banned assertion is the paired positive reach-guard.
#[test]
fn jace_multiplayer_ban_tracks_each_opponent_separately() {
    let (mut runner, jace, amy_attacker, bob_attacker) = jace_three_player_scenario();

    // Phase 1: Amy's combat. Amy declines.
    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    answer_optional(&mut runner, P1, false);
    assert!(
        !attack_is_legal(&runner, P1, amy_attacker, AttackTarget::Planeswalker(jace)),
        "Amy is banned after declining on her combat"
    );
    assert!(
        attack_is_legal(&runner, P2, bob_attacker, AttackTarget::Planeswalker(jace)),
        "Bob attacks Jaces freely while Amy is banned"
    );

    // Between turns: cleanup prunes Amy's EndOfTurn ban (CR 514.2), then Bob's
    // turn begins (turn order P0 -> P1 -> P2).
    execute_cleanup(runner.state_mut(), &mut Vec::new());
    runner.state_mut().active_player = P2;
    runner.state_mut().priority_player = P2;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P2 };

    // Phase 2: Bob's combat. Bob declines.
    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    answer_optional(&mut runner, P2, false);
    assert!(
        !attack_is_legal(&runner, P2, bob_attacker, AttackTarget::Planeswalker(jace)),
        "Bob is banned after declining on his combat"
    );
    assert!(
        attack_is_legal(&runner, P1, amy_attacker, AttackTarget::Planeswalker(jace)),
        "Amy is free on Bob's turn (her ban pruned at cleanup)"
    );
}

/// Row 5 — payer correctness: the opponent pays {2} from their own pool.
/// Accepting installs NO restriction; P1's pool drops by exactly {2} and
/// P0's pool is untouched.
///
/// REVERT-FAIL (`payer: Controller`): the controller is charged instead — with
/// P0's pool empty the payment fails, the ban installs, and both the
/// no-restriction and mana assertions fail.
#[test]
fn jace_accepted_payment_comes_from_the_opponent() {
    let (mut runner, jace, attacker) =
        jace_scenario_with_pool(&[ManaType::Colorless, ManaType::Colorless]);

    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    let p1_before = mana_total(&runner, P1);
    assert_eq!(p1_before, 2, "reach-guard: P1 holds {{2}} before choosing");
    answer_optional(&mut runner, P1, true);

    assert!(
        !has_attack_ban(&runner),
        "paying {{2}} suppresses the prohibition"
    );
    assert_eq!(
        mana_total(&runner, P1),
        0,
        "P1 paid {{2}} from their own pool"
    );
    assert_eq!(
        mana_total(&runner, P0),
        0,
        "Jace's controller is never charged"
    );
    assert!(
        attack_is_legal(&runner, P1, attacker, AttackTarget::Planeswalker(jace)),
        "reach-guard: payment leaves Jace attackable (pipeline reached declaration)"
    );
}

/// Row 5b — can't-pay path: a tapped-out opponent is still banned. The choice
/// is offered (or auto-skipped if that is the engine's unpayable-optional
/// behavior — pinned below); either way the restriction installs and Jace is
/// unattackable.
///
/// REVERT-FAIL (ban-on-decline removed): Jace stays attackable.
#[test]
fn jace_tapped_out_opponent_is_still_banned() {
    let (mut runner, jace, attacker) = jace_scenario();
    assert_eq!(mana_total(&runner, P1), 0, "reach-guard: P1 is tapped out");

    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    let prompted = matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    );
    if prompted {
        // Offered but unpayable: declining installs the ban without spending.
        answer_optional(&mut runner, P1, false);
    } else {
        drive_to_idle(&mut runner);
    }
    assert!(
        prompted,
        "pin: the tapped-out opponent is still offered the choice (decline path)"
    );
    assert!(
        has_attack_ban(&runner),
        "the ban installs when the opponent cannot pay"
    );
    assert_eq!(mana_total(&runner, P1), 0, "nothing was spent");
    assert!(
        !attack_is_legal(&runner, P1, attacker, AttackTarget::Planeswalker(jace)),
        "Jace is unattackable after the can't-pay path"
    );
}

/// Row 6 — the choice is offered to the triggering opponent (P1), not Jace's
/// controller (P0) or a bystander (P2); P1's decline links to the ban.
///
/// REVERT-FAIL (`optional_player` → controller): P0 is prompted instead.
#[test]
fn jace_choice_addresses_the_triggering_opponent() {
    let (mut runner, jace, attacker, _) = jace_three_player_scenario();

    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice {
            player: prompted, ..
        } => {
            assert_eq!(
                prompted, P1,
                "the {{2}} choice addresses the active opponent, not P0/P2"
            );
        }
        other => panic!("expected the {{2}} optional payment prompt, got {other:?}"),
    }
    answer_optional(&mut runner, P1, false);
    assert!(
        !attack_is_legal(&runner, P1, attacker, AttackTarget::Planeswalker(jace)),
        "chain linkage: P1's decline installs the ban"
    );
}

/// Row 5c — payer regression across the "may pay" family: sibling
/// "they may pay" triggers pay through the triggering player; the "you may
/// pay" sibling keeps the controller default; declared-target payers
/// (`unless` taxes) are untouched.
///
/// Horn/Wandering Archaic already stamp `TriggeringPlayer` via the clause
/// subject→payer rebind, so those pins are behavior-preserving (the post-pass
/// no-ops on them by its second conjunct). Jace and the Meathook opponent
/// trigger ("they" subject outside the rebind list) are fixed by the
/// post-pass re-stamp per CR 118.12; Meathook-you keeps the `Controller`
/// default (reflexive "you may" head never sets `modifiers.optional_player`).
#[test]
fn jace_may_pay_family_keeps_triggering_player_payer() {
    let pay_parent_payer = |text: &str, name: &str| {
        let parsed = parse_oracle_text(text, name, &[], &["Creature".to_string()], &[]);
        let trigger = parsed
            .triggers
            .iter()
            .find_map(|t| t.execute.as_deref())
            .unwrap_or_else(|| panic!("{name}: has a trigger with an execute chain"));
        match trigger.effect.as_ref() {
            Effect::PayCost { payer, .. } => payer.clone(),
            other => panic!("{name}: expected PayCost parent, got {other:?}"),
        }
    };

    // Horn of Plenty + Wandering Archaic: "they may pay" — the triggering
    // player pays (reach-guard: each also stamps optional_player).
    for (name, text) in [
        (
            "Horn of Plenty",
            "Whenever a player casts a spell, they may pay {1}. If the player does, they draw a card at the beginning of the next end step.",
        ),
        (
            "Wandering Archaic",
            "Whenever an opponent casts an instant or sorcery spell, they may pay {2}. If they don't, you may copy that spell. You may choose new targets for the copy.",
        ),
    ] {
        let parsed = parse_oracle_text(text, name, &[], &["Creature".to_string()], &[]);
        let node = parsed
            .triggers
            .iter()
            .find_map(|t| t.execute.as_deref())
            .unwrap_or_else(|| panic!("{name}: has a trigger with an execute chain"));
        assert_eq!(
            node.optional_player,
            Some(TargetFilter::TriggeringPlayer),
            "{name}: reach-guard: the choice is offered to the triggering player"
        );
        assert_eq!(
            pay_parent_payer(text, name),
            TargetFilter::TriggeringPlayer,
            "{name}: the triggering player pays"
        );
    }

    // Meathook Massacre II, opponent trigger: "they may pay 3 life".
    let meathook = parse_oracle_text(
        "When Meathook Massacre II enters, each player sacrifices X creatures of their choice.\nWhenever a creature you control dies, you may pay 3 life. If you do, return that card under your control with a finality counter on it.\nWhenever a creature an opponent controls dies, they may pay 3 life. If they don't, return that card under your control with a finality counter on it.",
        "Meathook Massacre II",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    // Meathook sibling: "you may pay 3 life" — controller-named, keeps the
    // controller default.
    let you_trigger = meathook
        .triggers
        .iter()
        .find(|t| {
            matches!(
                &t.valid_card,
                Some(TargetFilter::Typed(tf))
                    if tf.controller == Some(engine::types::ability::ControllerRef::You)
            ) && t
                .execute
                .as_deref()
                .is_some_and(|e| matches!(&*e.effect, Effect::PayCost { .. }))
        })
        .and_then(|t| t.execute.as_deref())
        .expect("Meathook: the you-creature-dies trigger");
    match you_trigger.effect.as_ref() {
        Effect::PayCost { payer, .. } => {
            assert_eq!(
                payer,
                &TargetFilter::Controller,
                "Meathook 'you may pay 3 life': the controller still pays"
            );
        }
        other => panic!("expected PayCost, got {other:?}"),
    }

    // Mana Leak: declared-target payer on the `unless` path — untouched by
    // any trigger-parent payer fix (Blazing Salvo's named-card UnlessPay pin
    // in `oracle_effect/tests.rs` covers the sibling shape).
    let leak = parse_oracle_text(
        "Counter target spell unless its controller pays {3}.",
        "Mana Leak",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let spell = leak
        .abilities
        .iter()
        .find(|a| a.unless_pay.is_some())
        .expect("Mana Leak carries an unless_pay modifier");
    assert_eq!(
        spell.unless_pay.as_ref().unwrap().payer,
        TargetFilter::ParentTargetController,
        "Mana Leak still charges the countered spell's controller"
    );

    // Meathook opponent trigger ("they may pay 3 life"): same "they"-in-body
    // shape as Jace, fixed by the same post-pass re-stamp (runs last so the
    // behavior-preserving pins above all execute first).
    let opponent_trigger = meathook
        .triggers
        .iter()
        .find(|t| {
            matches!(
                &t.valid_card,
                Some(TargetFilter::Typed(tf))
                    if tf.controller == Some(engine::types::ability::ControllerRef::Opponent)
            ) && t
                .execute
                .as_deref()
                .is_some_and(|e| matches!(&*e.effect, Effect::PayCost { .. }))
        })
        .and_then(|t| t.execute.as_deref())
        .expect("Meathook: the opponent-creature-dies trigger");
    match opponent_trigger.effect.as_ref() {
        Effect::PayCost { payer, .. } => {
            assert_eq!(
                payer,
                &TargetFilter::TriggeringPlayer,
                "Meathook 'they may pay 3 life': the triggering player pays"
            );
        }
        other => panic!("expected PayCost, got {other:?}"),
    }
}

/// Row 7 — `ScopedPlayer` fallback: an unbound scope pins the ability's
/// controller (`fill_runtime_fields`). Bound case is the paired reach-guard.
///
/// REVERT-FAIL (fallback removed): the unbound scope resolves to no one and
/// the attack goes through.
#[test]
fn jace_unbound_scoped_player_falls_back_to_controller() {
    let mut state = GameState::new(engine::types::format::FormatConfig::standard(), 2, 42);
    state.active_player = P1;
    let source_card = CardId(state.next_object_id);
    let source = create_object(
        &mut state,
        source_card,
        P0,
        "Jace, Multiverse Architect".to_string(),
        Zone::Battlefield,
    );
    let attacker_card = CardId(state.next_object_id);
    let attacker = create_object(
        &mut state,
        attacker_card,
        P1,
        "P1 Bear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&attacker).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.summoning_sick = false;
    }
    let jace = add_planeswalker(&mut state, P0, "Jace", "Jace");

    let restriction_effect = || Effect::AddRestriction {
        restriction: GameRestriction::ProhibitActivity {
            source: ObjectId(0),
            affected_players: RestrictionPlayerScope::ScopedPlayer,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::Attack {
                defended: AttackTargetFilter::Planeswalker {
                    subtype: Some("Jace".to_string()),
                },
                protected_player: None,
            },
        },
    };

    // Unbound scope + fallback present: pins the controller — production
    // cannot reach this path because the phase trigger stamps unconditionally,
    // so this documents the inherited contract.
    let mut ability = ResolvedAbility::new(restriction_effect(), vec![], source, P0);
    assert_eq!(
        ability.scoped_player, None,
        "reach-guard: the scope is genuinely unbound"
    );
    let mut events = Vec::new();
    add_restriction::resolve(&mut state, &ability, &mut events).unwrap();
    // Unbound lowered value: pins the controller. (No legality assertion
    // here: with a controller-pinned scope AND controller-defended subtype,
    // the only matching attack is a self-attack — illegal by game rules
    // regardless of any ban, so a ban verdict would pass vacuously.)
    // REVERT-FAIL (fallback removed, by construction): lowers to no one.
    let lowered_unbound = state
        .restrictions
        .last()
        .and_then(|r| match r {
            GameRestriction::ProhibitActivity {
                affected_players, ..
            } => Some(affected_players.clone()),
            _ => None,
        })
        .expect("the restriction is installed");
    assert_eq!(
        lowered_unbound,
        RestrictionPlayerScope::SpecificPlayer(P0),
        "unbound ScopedPlayer falls back to the controller"
    );
    // Paired negative: the unbound scope does not ban anyone else — P1's
    // genuine enemy attack on P0's Jace stays legal. Active player is set to
    // the attacker's controller so the verdict comes from the ban, not from
    // turn structure.
    let mut free_state = state.clone();
    free_state.active_player = P1;
    let mut free_events = Vec::new();
    assert!(
        declare_attackers(
            &mut free_state,
            &[(attacker, AttackTarget::Planeswalker(jace))],
            &mut free_events
        )
        .is_ok(),
        "the fallback pins the controller only: P1 stays free"
    );

    // Bound case: an explicitly scoped player lowers to that player, and the
    // ban genuinely bites — P1's enemy attack on P0's Jace is illegal.
    // REVERT-FAIL (matcher arm reverted): this attack goes through.
    // Reads the newest restriction — the unbound resolve above already
    // installed the P0-scoped one.
    ability.scoped_player = Some(P1);
    let mut events = Vec::new();
    add_restriction::resolve(&mut state, &ability, &mut events).unwrap();
    let lowered = state
        .restrictions
        .last()
        .and_then(|r| match r {
            GameRestriction::ProhibitActivity {
                affected_players, ..
            } => Some(affected_players.clone()),
            _ => None,
        })
        .expect("the restriction is installed");
    assert_eq!(
        lowered,
        RestrictionPlayerScope::SpecificPlayer(P1),
        "a bound scope lowers to that specific player"
    );
    let mut bound_state = state.clone();
    bound_state.active_player = P1;
    let mut bound_events = Vec::new();
    assert!(
        declare_attackers(
            &mut bound_state,
            &[(attacker, AttackTarget::Planeswalker(jace))],
            &mut bound_events
        )
        .is_err(),
        "bound scope + subtype-defended Jace: P1 cannot attack P0's Jace"
    );
}

/// Row 8 — coverage honesty: a non-planeswalker defended subtype ("Goblins")
/// fails closed to `Unimplemented` — honest red, no silent acceptance.
/// (Row 1 in this file is the paired positive: "Jaces" lowers cleanly.)
///
/// REVERT-FAIL (gate removed): the Goblin scope lowers to a supported
/// restriction.
#[test]
fn jace_non_planeswalker_subtype_scope_stays_unsupported() {
    let parsed = parse_oracle_text(
        "At the beginning of combat on each opponent's turn, they may pay {2}. If they don't, creatures they control can't attack Goblins you control this turn.",
        "Jace Goblin Test",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::Phase))
        .expect("the trigger frame still lowers");
    let parent = trigger.execute.as_deref().expect("trigger has execute");
    let nodes = chain_nodes(parent);
    assert!(
        nodes
            .iter()
            .any(|n| matches!(n.effect.as_ref(), Effect::Unimplemented { .. })),
        "a non-planeswalker defended subtype fails closed to Unimplemented"
    );
    assert!(
        !nodes.iter().any(|n| matches!(
            n.effect.as_ref(),
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    activity: ProhibitedActivity::Attack { .. },
                    ..
                },
                ..
            }
        )),
        "no attack restriction lowers for the unsupported scope"
    );
}

/// Rows 9/9b — subtype-defended static TAX, scoped end to end (fix round 2,
/// decision (a)): the subject-led shape lowers SUPPORTED (`CantAttack` +
/// `attack_defended` + `UnlessPay{2, Flat, defended}`) because the
/// `:2709` attach stamps `UnlessPay.defended` from `attack_defended`, and the
/// runtime tax (`compute_combat_tax`) filters by it — unpaid Jace attacks are
/// taxed (illegal unfunded), paid Jace attacks commit, and unpaid non-Jace /
/// controller attacks are untaxed (legal with an empty pool).
///
/// Probe text re-derived at implementation from the real Ghostly Prison
/// sentence ("Creatures can't attack you unless their controller pays {2}
/// for each creature they control that's attacking you."), with the defended
/// "you" replaced by "Jaces you control". The probe keeps the "for each"
/// tail, which lowers to `Flat` scaling; the runtime legs use a single
/// attacker, for which `Flat` and per-attacker scaling are equivalent —
/// multi-attacker scaling of the dropped tail is unpinned.
const JACE_TAX_ORACLE: &str = "Creatures can't attack Jaces you control unless their controller pays {2} for each creature they control that's attacking you.";

/// Ghostly-Prison-with-Jaces combat-tax static, carried by a 2/2 creature
/// shell so the fixture has a live permanent (mirrors the Molten Tide
/// suite's `add_ghostly_prison`).
fn add_jace_tax(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let def =
        parse_static_line(JACE_TAX_ORACLE).expect("Ghostly-Prison-with-Jaces tax should parse");
    let mut builder = scenario.add_creature(player, "Ghostly Prison Jaces", 2, 2);
    builder.with_static_definition(def);
    builder.id()
}

/// Fresh tax board: P0 controls the Ghostly-Prison-with-Jaces static, a Jace
/// planeswalker, and a non-Jace planeswalker; P1 has one ready attacker and
/// an empty pool. Active player is P1 at pre-combat main with priority.
fn jace_tax_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_jace_tax(&mut scenario, P0);
    let attacker = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let mut runner = scenario.build();
    let jace = add_planeswalker(runner.state_mut(), P0, "Jace", "Jace");
    let chandra = add_planeswalker(runner.state_mut(), P0, "Chandra", "Chandra");
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, attacker, jace, chandra)
}

/// Deposit exactly {2} generic into P1's pool for the paid-tax leg.
fn fund_two_generic(runner: &mut GameRunner) {
    for ty in [ManaType::Colorless, ManaType::Colorless] {
        runner
            .state_mut()
            .add_mana_to_pool(P1, ManaUnit::new(ty, ObjectId(0), false, vec![]))
            .expect("funding P1's pool succeeds");
    }
}

#[test]
fn jace_subtype_defended_tax_shape_and_runtime() {
    // Parse shape: CantAttack + subtype defended + UnlessPay {2}.
    // REVERT-FAIL (combinator arm removed): `parse_static_line` yields no
    // subtype-defended tax (None defended or unparsed).
    let def =
        parse_static_line(JACE_TAX_ORACLE).expect("Ghostly-Prison-with-Jaces tax should parse");
    assert_eq!(
        def.mode,
        StaticMode::CantAttack,
        "the tax lowers as a CantAttack static, got {:?}",
        def.mode
    );
    assert_eq!(
        def.attack_defended,
        Some(AttackTargetFilter::Planeswalker {
            subtype: Some("Jace".to_string())
        }),
        "the shared combinator scopes the tax to Jaces you control"
    );
    // Reach-guard: `attack_defended` proves the line reached the subject-led
    // path (not an upstream decline) — the stamp below keys off exactly it.
    // RED-first on the stamp (fix reverted): `UnlessPay.defended` stays
    // `None` while `attack_defended` stays `Some` — the refuted premise.
    match &def.condition {
        Some(StaticCondition::UnlessPay {
            cost,
            scaling,
            defended,
        }) => {
            assert_eq!(cost.mana_value(), 2, "the tax costs {{2}}, got {cost:?}");
            assert_eq!(
                *scaling,
                UnlessPayScaling::Flat,
                "the subject-led route carries no per-affected tail, got {scaling:?}"
            );
            assert_eq!(
                *defended,
                Some(AttackTargetFilter::Planeswalker {
                    subtype: Some("Jace".to_string())
                }),
                "the :2709 stamp scopes the tax payload to Jaces you control"
            );
        }
        other => panic!("expected an UnlessPay tax condition, got {other:?}"),
    }

    // Runtime board: P0 controls the tax source, a Jace, and a non-Jace
    // planeswalker; P1 has one attacker. No Jace trigger on board — only the
    // static tax is under test, so no optional prompt can interfere.
    let (mut runner, attacker, jace, chandra) = jace_tax_board();

    // Unpaid Jace attack: the preview reports the tax unaffordable, the
    // declaration still opens the tax prompt, and accepting fails without
    // committing — the attack is illegal unpaid.
    // REVERT-FAIL (matcher arm reverted): the preview sees no tax and the
    // unpaid attack commits — the same revert breaks rows 2 and 9 together
    // (shared-enforcement proof).
    let jace_attacks = vec![(attacker, AttackTarget::Planeswalker(jace))];
    assert!(
        !attack_tax_is_affordable(runner.state(), &jace_attacks),
        "an unfunded Jace attack cannot cover the {{2}} tax"
    );
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: jace_attacks,
            bands: vec![],
        })
        .expect("a human taxed declaration still opens the tax prompt");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CombatTaxPayment { .. }
        ),
        "the unpaid Jace declaration must ask for the tax"
    );
    let error = runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect_err("accepting an unaffordable Jace tax must fail");
    assert!(
        format!("{error:?}").contains("Cannot pay mana cost"),
        "unexpected rejection: {error:?}"
    );
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_none_or(|combat| combat.attackers.is_empty()),
        "a failed tax payment must not commit the Jace attack"
    );

    // Paid Jace attack: funding {2} commits the attack and spends the pool.
    // Paired positive proving the block above is a tax, not a ban. Stand
    // the pending (failed) tax prompt down first — a failed payment leaves
    // `CombatTaxPayment` intact by design.
    runner
        .act(GameAction::PayCombatTax { accept: false })
        .expect("declining the unaffordable tax stands the prompt down");
    fund_two_generic(&mut runner);
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Planeswalker(jace))],
            bands: vec![],
        })
        .expect("the funded Jace attack must open the tax prompt");
    runner
        .act(GameAction::PayCombatTax { accept: true })
        .expect("paying the {{2}} Jace tax must succeed");
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == attacker)),
        "the paid Jace attack commits"
    );
    assert_eq!(mana_total(&runner, P1), 0, "the {{2}} tax left P1's pool");

    // Row 9b continued — SCOPED legs (fix round 2): the stamped
    // `UnlessPay.defended` means the tax fires ONLY on Jace-subtype targets.
    // REVERT-FAIL (stamp reverted → `defended: None`): the preview below
    // reports unaffordable and the declaration opens a tax prompt — the
    // over-broad-tax direction (unpaid non-Jace attacks illegally taxed).
    for (target, label) in [
        (AttackTarget::Planeswalker(chandra), "Chandra"),
        (AttackTarget::Player(P0), "Jace's controller"),
    ] {
        let (mut fresh, fresh_attacker, _, _) = jace_tax_board();
        let attacks = vec![(fresh_attacker, target)];
        assert!(
            attack_tax_is_affordable(fresh.state(), &attacks),
            "an unfunded {label} attack must see no tax (empty pool is affordable)"
        );
        fresh.pass_both_players();
        fresh
            .act(GameAction::DeclareAttackers {
                attacks,
                bands: vec![],
            })
            .unwrap_or_else(|e| panic!("the untaxed {label} attack must declare directly: {e:?}"));
        assert!(
            !matches!(
                fresh.state().waiting_for,
                WaitingFor::CombatTaxPayment { .. }
            ),
            "no tax prompt may open for the untaxed {label} attack"
        );
        assert!(
            fresh.state().combat.as_ref().is_some_and(|combat| combat
                .attackers
                .iter()
                .any(|a| a.object_id == fresh_attacker)),
            "the untaxed {label} attack commits"
        );
    }
}

// Row 9 Goblins-tax shape — STOP-ITEM (returned, not pinned): the plan's
// "Goblins … unless … pays → None" premise is refuted by measurement.
// `parse_static_line("Creatures can't attack Goblins you control unless
// their controller pays {2}.")` yields `Some(CantAttack,
// affected=Typed(Creature), attack_defended=None, UnlessPay{{2}, Flat,
// defended=None})` — an UNSCOPED {2} tax (observed). The predicate's
// defended arm fails closed correctly (subtype gate rejects "Goblin"), but
// the subject path then accepts the leftover scope text as an untaxed
// trailing-`unless` and attaches an unscoped tax. Pinning that would
// enshrine an over-broad tax (fix-round-1 precedent: never pin measured
// over-broad behavior). Smallest fix site: decline at the `:2709` attach
// when the predicate consumed no defended scope yet trailing text precedes
// the `unless` — a NEW decline behavior, outside this round's stamp-only
// scope, returned for a design decision.

/// Row 10 — event delivery through a REAL phase transition: from P1's
/// pre-combat main with priority, advance into beginning of combat via the
/// runner (production `turns::auto_advance` + priority passes — no injected
/// `PhaseChanged`, no teleported phase) and prove Jace's trigger fires: the
/// {2} choice reaches P1, declining installs the ban, and P1's Bear cannot
/// attack the Jace. Guards the registration + delivery path the
/// injected-event rows bypass (review R3 note).
#[test]
fn jace_trigger_fires_through_real_begin_combat_transition() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jace = scenario
        .add_planeswalker_from_oracle(P0, "Jace, Multiverse Architect", "Jace", 4, JACE_ORACLE)
        .id();
    let attacker = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    // NOTE: phase stays PreCombatMain — the advance below is the real thing.

    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "reach-guard: the runner must reach beginning of combat, else the \
         trigger below never fires"
    );

    drive_to_optional_prompt(&mut runner);
    answer_optional(&mut runner, P1, false);

    assert!(
        has_attack_ban(&runner),
        "declining installs the attack prohibition"
    );
    // REVERT-FAIL: without event delivery the trigger never fires, no ban
    // exists, and this attack stays legal.
    assert!(
        !attack_is_legal(&runner, P1, attacker, AttackTarget::Planeswalker(jace)),
        "CR 508.1c: P1 can't attack a Jace P0 controls"
    );
}
