//! BB-FU7 — a spelled-out "attacks each combat if able and attacks a player
//! other than you if able" effect is NOT the goad keyword action.
//!
//! Two cards print the goad *effect* without the goad *keyword*:
//!   - Kardur, Doomscourge: "When Kardur enters, until your next turn, creatures
//!     your opponents control attack each combat if able and attack a player
//!     other than you if able."
//!   - Maximum Carnage chapter I: "Until your next turn, each creature attacks
//!     each combat if able and attacks a player other than you if able."
//!
//! (Both verbatim from MTGJSON `AtomicCards.json`.)
//!
//! CR 701.15a: only a spell or ability that *goads* a creature makes it goaded.
//! CR 701.15b: "Goaded is a designation a permanent can have" — the designation,
//! not the pair of requirements, is what "goaded creature" effects read.
//! Official Maximum Carnage ruling (2025-09-19, Scryfall/MTGJSON): "Although the
//! effects of the first chapter ability are the same as the goad keyword action,
//! that ability doesn't cause any creatures to become goaded. Effects that refer
//! to 'goaded creatures' won't apply."
//!
//! The engine reads that designation from `GameObject::goaded_by`
//! (`game/filter.rs`: `FilterProp::Goaded => !obj.goaded_by.is_empty()`), which
//! is exactly what these tests assert on.
//!
//! Second divergence, same rulings: the effect is a *continuous* effect with a
//! dynamic affected set. Kardur ruling (2021-02-05): "Kardur's first ability
//! affects all creatures your opponents control, including any that enter the
//! battlefield after the ability resolves." Maximum Carnage ruling (2025-09-19):
//! "affects all creatures until your next turn, regardless of whether or not they
//! were on the battlefield at the time the ability resolved." A one-shot goad mark
//! only touches the creatures present at resolution.

use std::sync::Arc;

use engine::game::combat::{
    attacker_constraints_for_active_player, get_valid_attacker_ids, CombatRequirement,
};
use engine::game::derived::derive_display_state;
use engine::game::functioning_abilities::active_static_definitions;
use engine::game::layers::evaluate_layers;
use engine::game::public_state::mark_public_state_all_dirty;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::turns::{execute_cleanup, execute_untap};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Effect, StaticDefinition, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// Kardur's ETB effect sentence, verbatim minus the "When Kardur enters," trigger
/// prefix so it can be cast as a sorcery (the effect text is what is under test).
const KARDUR_EFFECT: &str = "Until your next turn, creatures your opponents control attack each combat if able and attack a player other than you if able.";

/// Kardur, Doomscourge's full Oracle text (MTGJSON verbatim).
const KARDUR_ORACLE: &str = "When Kardur enters, until your next turn, creatures your opponents control attack each combat if able and attack a player other than you if able.\nWhenever an attacking creature dies, each opponent loses 1 life and you gain 1 life.";

/// Maximum Carnage chapter I (MTGJSON verbatim).
const MAXIMUM_CARNAGE_CHAPTER_I: &str =
    "Until your next turn, each creature attacks each combat if able and attacks a player other than you if able.";

/// Maximum Carnage's FULL verbatim MTGJSON Oracle text — including the Saga
/// reminder line and the `I —`/`II —`/`III —` chapter prefixes. The bare chapter
/// body parses down a DIFFERENT path (measured: `Effect::Unimplemented`
/// `{static_structure}` under `["Enchantment"]/["Saga"]`), so it is not a reach
/// guard; only the full text exercises the Saga chapter lowering.
const MAXIMUM_CARNAGE_ORACLE: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — Until your next turn, each creature attacks each combat if able and attacks a player other than you if able.\nII — Add {R}{R}{R}.\nIII — This Saga deals 5 damage to each opponent.";

/// Disrupt Decorum's FULL verbatim MTGJSON Oracle text, reminder parenthetical
/// INCLUDED — that parenthetical is a word-for-word instance of the compound
/// this change's recognizer matches, so a truncated fixture cannot guard the
/// interaction. Reminder text is stripped in `parser/oracle.rs`
/// (`strip_reminder_text` inside `prepare_spell_resolution_line`), so today's
/// risk is low (measured: the full text yields abilities:1, triggers:0,
/// statics:0) — which is exactly why the verbatim string is free and strictly
/// stronger.
const DISRUPT_DECORUM_ORACLE: &str = "Goad all creatures you don't control. (Until your next turn, those creatures attack each combat if able and attack a player other than you if able.)";

/// CR 508.1d + CR 701.15b: true when the spelled-out requirement is grafted onto
/// `id` as a functioning static. This is the reach guard every "no designation"
/// / "no longer bound" assertion in this file is paired with — an empty
/// `goaded_by` is satisfied vacuously by a parse that produced nothing.
fn bound_by_requirement(state: &GameState, id: ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|obj| {
        active_static_definitions(state, obj)
            .any(|sd| sd.mode == StaticMode::MustAttackAwayFromSource)
    })
}

/// P0 casts the spelled-out requirement in their precombat main; P1 has one
/// creature on board. Returns the post-resolution runner and P1's creature.
fn cast_requirement_spell(text: &str) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Spelled-Out Requirement", false, text)
        .id();
    let opp_creature = scenario.add_creature(P1, "Ornery Ox", 2, 2).id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    (runner, opp_creature)
}

/// Put the game in P1's declare-attackers step with `attacker` as the only
/// possible attacker and all three players as possible defenders.
fn arm_declare_attackers(runner: &mut GameRunner, attacker: ObjectId) {
    arm_declare_attackers_against(runner, &[attacker], &[P0, P2]);
}

/// Generalized form of [`arm_declare_attackers`]: P1 declares attackers with
/// `attackers` as the only eligible creatures and `defenders` as the only
/// attackable players (CR 508.1b).
fn arm_declare_attackers_against(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    defenders: &[PlayerId],
) {
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.phase = Phase::DeclareAttackers;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: P1,
        valid_attacker_ids: attackers.to_vec(),
        valid_attack_targets: defenders
            .iter()
            .copied()
            .map(AttackTarget::Player)
            .collect(),
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
}

/// Install a functioning intrinsic `CantAttackOrBlock` static (the Pacifism
/// restriction, CR 508.1c), mirroring `goaded_creature_under_pacifism_visible`.
fn pacify(runner: &mut GameRunner, creature: ObjectId) {
    let def = StaticDefinition::new(StaticMode::CantAttackOrBlock)
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::CantAttackOrBlock,
        }]);
    let obj = runner.state_mut().objects.get_mut(&creature).unwrap();
    obj.static_definitions = vec![def.clone()].into();
    obj.base_static_definitions = Arc::new(vec![def]);
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

/// FLIP LEG — CR 701.15a/b + the Maximum Carnage ruling: the affected creature
/// must NOT carry the goaded designation. Today the parser lowers the sentence to
/// `Effect::GoadAll`, whose resolver stamps `goaded_by`, so this fails.
#[test]
fn spelled_out_requirement_does_not_goad_the_creature() {
    let (runner, opp_creature) = cast_requirement_spell(KARDUR_EFFECT);
    // Reach guard: the requirement actually landed, so the empty `goaded_by`
    // below cannot pass because the sentence failed to parse or resolve.
    assert!(
        bound_by_requirement(runner.state(), opp_creature),
        "reach: the opponent's creature must carry the spelled-out requirement"
    );
    let goaded_by = &runner.state().objects[&opp_creature].goaded_by;
    assert!(
        goaded_by.is_empty(),
        "CR 701.15a/b + official Maximum Carnage ruling: a spelled-out \
         'attacks ... if able and attacks a player other than you if able' effect \
         creates combat requirements but NO goaded designation; \
         `goaded_by` must stay empty, got {goaded_by:?}"
    );
}

/// FLIP LEG — same for the "each creature" (Maximum Carnage) population, which
/// includes the effect controller's own creatures.
#[test]
fn maximum_carnage_chapter_one_does_not_goad_any_creature() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Maximum Carnage", false, MAXIMUM_CARNAGE_CHAPTER_I)
        .id();
    let own = scenario.add_creature(P0, "Bear", 2, 2).id();
    let opp = scenario.add_creature(P1, "Wolf", 2, 2).id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();

    for (id, label) in [
        (own, "controller's own creature"),
        (opp, "opponent's creature"),
    ] {
        // Reach guard: "each creature" binds BOTH populations, so an empty
        // `goaded_by` below is a real negative and not a parse failure.
        assert!(
            bound_by_requirement(runner.state(), id),
            "reach: {label} must carry the spelled-out requirement"
        );
        let goaded_by = &runner.state().objects[&id].goaded_by;
        assert!(
            goaded_by.is_empty(),
            "Maximum Carnage ruling (2025-09-19): chapter I 'doesn't cause any \
             creatures to become goaded' — {label} must have an empty \
             `goaded_by`, got {goaded_by:?}"
        );
    }
}

/// FLIP LEG — Kardur ruling (2021-02-05) / Maximum Carnage ruling (2025-09-19):
/// the effect binds creatures that enter AFTER it resolves. A one-shot goad mark
/// cannot, so this fails today.
#[test]
fn requirement_binds_a_creature_that_enters_after_resolution() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Spelled-Out Requirement", false, KARDUR_EFFECT)
        .id();
    // No opponent creature exists at resolution: the late entrant below is the
    // ONLY creature the requirement could bind, so the assertion cannot be
    // satisfied by some other creature's requirement (measured: with a
    // pre-existing goaded creature on board this test passed vacuously).
    let mut runner = scenario.build();
    runner.cast(spell).resolve();

    // The late entrant: enters the battlefield AFTER the effect resolved (still
    // during P0's turn 1), so on P1's turn 2 it is not summoning-sick (CR 302.6)
    // and is able to attack.
    let late = {
        let state = runner.state_mut();
        let card_id = engine::types::identifiers::CardId(state.next_object_id);
        let id = engine::game::zones::create_object(
            state,
            card_id,
            P1,
            "Late Bloomer".to_string(),
            engine::types::Zone::Battlefield,
        );
        let ts = state.next_timestamp();
        let entered_turn = state.turn_number;
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.entered_battlefield_turn = Some(entered_turn);
        obj.summoning_sick = false;
        obj.timestamp = ts;
        // `zones::create_object` is raw test scaffolding and does NOT mark the
        // layer system dirty (the real ETB pipeline does). Mark the incremental
        // "entered" state exactly as a real entrant would, so the CR 611.2c
        // dynamic-set machinery — `active_effects_force_incremental_escalation`
        // → `entered_object_perturbs_affected_filter` → the intact TCE filter —
        // actually runs for this object.
        state.layers_dirty.mark_entered(id);
        id
    };

    // `arm_declare_attackers` writes `waiting_for` directly, bypassing the phase
    // machinery that normally flushes layers, so run the layer pass explicitly
    // (same `refresh` staging as `goaded_creature_under_pacifism_visible`).
    evaluate_layers(runner.state_mut());
    // Reach guard: the requirement really did graft onto the LATE entrant via
    // the intact filter — the `is_err()` below is not some unrelated illegality.
    assert!(
        bound_by_requirement(runner.state(), late),
        "CR 611.2c: the intact affected filter must bind a creature that entered \
         after the effect resolved"
    );

    arm_declare_attackers(&mut runner, late);
    let res = runner.act(GameAction::DeclareAttackers {
        attacks: vec![],
        bands: vec![],
    });
    assert!(
        res.is_err(),
        "CR 508.1d + Kardur ruling: the requirement applies to creatures that \
         enter after the effect resolves, so declaring no attackers with an able \
         creature must be illegal"
    );
}

/// GUARD LEG (passes today, must keep passing) — the two combat requirements are
/// actually enforced: an able creature must attack (CR 508.1d), and it must
/// attack a player other than the effect's controller.
#[test]
fn spelled_out_requirement_forces_attack_away_from_the_controller() {
    // Declaring no attackers is illegal.
    let (mut runner, ox) = cast_requirement_spell(KARDUR_EFFECT);
    arm_declare_attackers(&mut runner, ox);
    let none = runner.act(GameAction::DeclareAttackers {
        attacks: vec![],
        bands: vec![],
    });
    assert!(
        none.is_err(),
        "CR 508.1d: an able creature under an 'attacks each combat if able' \
         requirement must attack"
    );

    // Attacking the effect's controller (P0) disobeys the "a player other than
    // you" requirement while P2 is an available legal defender.
    let (mut runner, ox) = cast_requirement_spell(KARDUR_EFFECT);
    arm_declare_attackers(&mut runner, ox);
    let at_controller = runner.act(GameAction::DeclareAttackers {
        attacks: vec![(ox, AttackTarget::Player(P0))],
        bands: vec![],
    });
    assert!(
        at_controller.is_err(),
        "the 'attacks a player other than you if able' requirement must forbid \
         attacking the effect's controller when another player is attackable"
    );

    // Attacking the third player obeys both requirements.
    let (mut runner, ox) = cast_requirement_spell(KARDUR_EFFECT);
    arm_declare_attackers(&mut runner, ox);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(ox, AttackTarget::Player(P2))],
            bands: vec![],
        })
        .expect("attacking a player other than the effect's controller obeys both requirements");
    assert!(
        runner.state().combat.is_some(),
        "the legal declaration must commit combat"
    );
}

/// PARSE LEG (SHAPE) — neither real card's FULL Oracle text may lower to the
/// goad mechanic, and each must produce exactly one requirement grant.
///
/// The negative alone would pass vacuously on a card that failed to parse (the
/// prior revision of this test walked the bare chapter body, which measures to
/// `Effect::Unimplemented{static_structure}` — `has_goad` is false there both
/// before and after the fix). Assertions 1 and 3 close that hole: the parse must
/// yield triggers, and exactly one walked effect must be the positive shape.
#[test]
fn neither_card_lowers_to_goad() {
    fn has_goad(effect: &Effect) -> bool {
        matches!(effect, Effect::GoadAll { .. } | Effect::Goad { .. })
    }

    /// CR 701.15b attaches exactly two combat requirements; both must ride ONE
    /// `StaticDefinition` so `register_transient_effect` keeps the affected
    /// filter intact for both (CR 611.2c).
    fn is_requirement_grant(effect: &Effect) -> bool {
        let Effect::GenericEffect {
            static_abilities, ..
        } = effect
        else {
            return false;
        };
        static_abilities.len() == 1
            && static_abilities[0].modifications
                == vec![
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustAttack,
                    },
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustAttackAwayFromSource,
                    },
                ]
    }

    /// Every effect the card's parse produces: standalone abilities, trigger
    /// executes, and their `sub_ability` chains.
    fn walk(parsed: &engine::parser::oracle::ParsedAbilities) -> Vec<Effect> {
        fn push_chain(def: &engine::types::ability::AbilityDefinition, out: &mut Vec<Effect>) {
            out.push(def.effect.as_ref().clone());
            if let Some(sub) = def.sub_ability.as_deref() {
                push_chain(sub, out);
            }
        }
        let mut out = Vec::new();
        for ability in &parsed.abilities {
            push_chain(ability, &mut out);
        }
        for trigger in &parsed.triggers {
            if let Some(execute) = &trigger.execute {
                push_chain(execute, &mut out);
            }
        }
        out
    }

    for (label, oracle, core_types, subtypes) in [
        (
            "Kardur, Doomscourge",
            KARDUR_ORACLE,
            &["Creature"][..],
            &["Demon"][..],
        ),
        (
            "Maximum Carnage",
            MAXIMUM_CARNAGE_ORACLE,
            &["Enchantment"][..],
            &["Saga"][..],
        ),
    ] {
        let parsed = parse_oracle_text(
            oracle,
            label,
            &[],
            &core_types.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &subtypes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );

        // 1. Non-empty collection: a parse that produced nothing must not pass.
        assert!(
            !parsed.triggers.is_empty(),
            "{label}: the full Oracle text must produce triggers (Kardur ETB / \
             Maximum Carnage Saga chapters), got none"
        );

        let effects = walk(&parsed);

        // 2. Negative — CR 701.15a + the 2025-09-19 ruling.
        for effect in &effects {
            assert!(
                !has_goad(effect),
                "{label} must not lower to the goad mechanic, got {effect:?}"
            );
        }

        // 3. Positive reach — subsumes "not Effect::Unimplemented" and makes (2)
        //    impossible to satisfy vacuously.
        let grants = effects.iter().filter(|e| is_requirement_grant(e)).count();
        assert_eq!(
            grants, 1,
            "{label} must produce exactly one MustAttack + \
             MustAttackAwayFromSource grant, got {grants} in {effects:?}"
        );
    }
}

/// T6 — MULTI-AUTHORITY (CR 701.15c analogue): two different players each resolve
/// the effect over P1's creature, so it carries two grafted definitions with
/// distinct `source_controller` anchors and must avoid BOTH.
///
/// REVERT PROBE: collapse the `layers.rs` graft dedup from full-definition
/// equality (`sd == &def`) to mode-only ⇒ only one anchor survives ⇒ one of
/// P0/P2 becomes a legal defender ⇒ one of the two `is_err()` legs fails.
#[test]
fn two_controllers_each_add_their_own_avoided_player() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p0_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Spelled-Out Requirement", false, KARDUR_EFFECT)
        .id();
    let p2_spell = scenario
        .add_spell_to_hand_from_oracle(P2, "Spelled-Out Requirement", false, KARDUR_EFFECT)
        .id();
    let ox = scenario.add_creature(P1, "Ornery Ox", 2, 2).id();
    let mut runner = scenario.build();

    runner.cast(p0_spell).resolve();
    {
        // Hand the turn to P2 so it can cast the same sorcery-speed effect.
        let state = runner.state_mut();
        state.active_player = P2;
        state.priority_player = P2;
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player: P2 };
    }
    runner.cast(p2_spell).resolve();

    // Reach: BOTH anchors are present on the one creature (CR 701.15c — each
    // distinct avoided player is an additional requirement).
    let anchors: Vec<PlayerId> = {
        let state = runner.state();
        let obj = &state.objects[&ox];
        let mut a: Vec<PlayerId> = active_static_definitions(state, obj)
            .filter(|sd| sd.mode == StaticMode::MustAttackAwayFromSource)
            .filter_map(|sd| sd.source_controller)
            .collect();
        a.sort_unstable_by_key(|p| p.0);
        a
    };
    assert_eq!(
        anchors,
        vec![P0, P2],
        "CR 701.15c: two installing players must leave two distinct anchors"
    );

    for avoided in [P0, P2] {
        // A rejected declaration is validated before combat is committed, so the
        // state is re-armable for the next leg.
        arm_declare_attackers_against(&mut runner, &[ox], &[P0, P2, P3]);
        let res = runner.act(GameAction::DeclareAttackers {
            attacks: vec![(ox, AttackTarget::Player(avoided))],
            bands: vec![],
        });
        assert!(
            res.is_err(),
            "CR 508.1d: attacking {avoided:?} disobeys that player's own \
             away-from requirement while P3 is attackable"
        );
    }

    arm_declare_attackers_against(&mut runner, &[ox], &[P0, P2, P3]);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(ox, AttackTarget::Player(P3))],
            bands: vec![],
        })
        .expect("attacking the one player neither installer named obeys both requirements");
    assert!(
        runner.state().combat.is_some(),
        "the legal declaration must commit combat"
    );
}

/// T7 — NEGATIVE SIBLING: the goad KEYWORD path is untouched. Disrupt Decorum
/// still stamps the designation, and its reminder parenthetical (a word-for-word
/// instance of the compound this change recognizes) must not graft a competing
/// `MustAttackAwayFromSource` static.
///
/// REVERT PROBE (leg 1): route `Effect::GoadAll` through the new static instead
/// of `goad.rs` ⇒ `goaded_by` stays empty ⇒ leg 1 fails. Leg 1 also proves the
/// spell resolved, so leg 2's `is_none()` is reached rather than vacuous.
///
/// Leg 2 is a regression TRIPWIRE, not a discriminator: measured twice (executor
/// and reviewer), deleting the `strip_reminder_text` call from `parser/oracle.rs`
/// leaves this test green (suite still 12/12), so no known single edit flips it.
/// Its non-vacuity comes only from the pairing with leg 1 above.
#[test]
fn keyword_goad_still_sets_the_designation() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Disrupt Decorum", false, DISRUPT_DECORUM_ORACLE)
        .id();
    let opp = scenario.add_creature(P1, "Ornery Ox", 2, 2).id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();

    // Leg 1 (positive) — CR 701.15a/b: the keyword still designates.
    assert!(
        runner.state().objects[&opp].goaded_by.contains(&P0),
        "the goad KEYWORD must still stamp the designation, got {:?}",
        runner.state().objects[&opp].goaded_by
    );

    // Leg 2 (negative, reached because leg 1 proved resolution) — the reminder
    // text must not produce a second, non-designating grant.
    assert!(
        !bound_by_requirement(runner.state(), opp),
        "the reminder parenthetical must not graft a competing \
         MustAttackAwayFromSource static alongside the keyword"
    );
}

/// T8 — DURATION: the grant is bound to `UntilNextTurnOf { Controller }`, so it
/// survives the intervening player's turn and dies at the controller's next
/// untap step (CR 502). Note the expiry mechanism MOVED with this change: goad
/// expired via `goaded_by.remove(active_player)`; this expires via
/// `prune_until_next_turn_effects` on the transient continuous effect.
///
/// REVERT PROBE (measured): force `dur = Duration::UntilEndOfTurn`
/// unconditionally in `game/effects/effect.rs::resolve` ⇒ P0's cleanup prunes the
/// grant ⇒ leg A flips to fail. Leg A is therefore the discriminator for the
/// duration carry-through the GoadAll → GenericEffect migration had to preserve.
/// NOT a probe (measured): dropping only the `ability.duration` term of that same
/// expression (leaving `duration.clone().unwrap_or(UntilEndOfTurn)`) leaves all 12
/// tests passing — `ability.duration` is not the sole carrier at that seam.
#[test]
fn requirement_expires_at_the_controllers_next_untap() {
    let (mut runner, ox) = cast_requirement_spell(KARDUR_EFFECT);
    assert!(
        bound_by_requirement(runner.state(), ox),
        "reach: the requirement must be grafted before the turn advances"
    );

    // CR 514.2: P0's cleanup step — this is where an `UntilEndOfTurn` grant dies.
    let mut events = Vec::new();
    execute_cleanup(runner.state_mut(), &mut events);

    // P1's untap step: only P1-controlled `UntilNextTurnOf` effects are pruned.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.turn_number = 2;
    }
    let mut events = Vec::new();
    execute_untap(runner.state_mut(), &mut events);
    // MANDATORY: the derived per-object graft only reflects a prune after the
    // next layer pass, and `arm_declare_attackers` writes `waiting_for` directly
    // (bypassing the phase machinery that would flush). Without this the leg-A
    // assertion below reads STALE derived statics and passes for any duration.
    evaluate_layers(runner.state_mut());

    // LEG A (positive, the duration discriminator) — still bound during the
    // intervening turn. An `UntilEndOfTurn` grant died at P0's cleanup above.
    assert!(
        bound_by_requirement(runner.state(), ox),
        "CR 514.2: an `UntilNextTurnOf {{ Controller }}` grant survives P0's \
         cleanup and P1's untap — it must still be grafted here"
    );
    arm_declare_attackers(&mut runner, ox);
    assert!(
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .is_err(),
        "CR 508.1d: the requirement lasts until the CONTROLLER's next turn, so \
         it is still enforced during the intervening turn"
    );

    // CR 502 (Untap Step): at the start of P0's next turn
    // `prune_until_next_turn_effects` drops the transient effect. (CR 514.2 is the
    // CLEANUP-step prune above, which this duration deliberately survives.)
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.turn_number = 3;
    }
    let mut events = Vec::new();
    execute_untap(runner.state_mut(), &mut events);
    // The prune removes the TCE and marks layers dirty; the per-object derived
    // `static_definitions` only lose the graft on the next layer pass.
    evaluate_layers(runner.state_mut());

    // LEG B (negative, reached because leg A proved the grant existed).
    assert!(
        !bound_by_requirement(runner.state(), ox),
        "CR 502: the grant must be gone at the controller's next untap step"
    );
    arm_declare_attackers(&mut runner, ox);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("with the requirement expired, declining to attack is legal again");
}

/// T9 — RESTRICTION beats REQUIREMENT (CR 508.1c over CR 508.1d): a creature
/// that can't attack is not force-attacked, while its unrestricted sibling under
/// the same effect still is.
///
/// REVERT PROBE: remove the `MustAttackAwayFromSource` contribution from
/// `players_to_attack_away_from_gated` ⇒ the AWAY-FROM leg below flips to `Ok`.
/// (Measured: the plain "declaring no attackers is illegal" leg does NOT flip
/// under that probe — the same `StaticDefinition` also grafts `MustAttack`,
/// which carries it independently. The away-from leg is the discriminator.)
#[test]
fn cant_attack_restriction_beats_the_spelled_out_requirement() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Spelled-Out Requirement", false, KARDUR_EFFECT)
        .id();
    let pacified = scenario.add_creature(P1, "Pacified Ox", 2, 2).id();
    let free = scenario.add_creature(P1, "Ornery Ox", 2, 2).id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();

    pacify(&mut runner, pacified);
    // Reach: BOTH creatures carry the requirement, so the legality difference
    // below is the Pacifism restriction and not a missing grant.
    assert!(
        bound_by_requirement(runner.state(), pacified),
        "reach: the pacified creature must still carry the requirement after \
         Pacifism is applied"
    );
    assert!(
        bound_by_requirement(runner.state(), free),
        "reach: the unrestricted creature must carry the requirement"
    );

    // POSITIVE — the unrestricted creature is still forced.
    arm_declare_attackers_against(&mut runner, &[pacified, free], &[P0, P2]);
    assert!(
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .is_err(),
        "CR 508.1d: the unrestricted creature under the requirement must attack"
    );

    // AWAY-FROM leg (CR 701.15b second clause, the discriminator for the
    // requirement AUTHORITY): the unrestricted creature may not attack the
    // effect's controller while P2 is attackable. Pairs with the negative below.
    arm_declare_attackers_against(&mut runner, &[pacified, free], &[P0, P2]);
    assert!(
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(free, AttackTarget::Player(P0))],
                bands: vec![],
            })
            .is_err(),
        "CR 508.1d + CR 701.15b: the away-from requirement must forbid attacking \
         the effect's controller while another player is attackable"
    );

    // NEGATIVE — the pacified creature is NOT force-attacked (CR 508.1c).
    arm_declare_attackers_against(&mut runner, &[pacified, free], &[P0, P2]);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(free, AttackTarget::Player(P2))],
            bands: vec![],
        })
        .expect("CR 508.1c: a creature that can't attack is not forced to");
}

/// T10 — WIRE: a creature bound by the requirement must not render
/// `Static: MustAttackAwayFromSource` as an unimplemented mechanic. The graft
/// reach assertion is mandatory: an unbound creature has an empty vec vacuously.
///
/// REVERT PROBE: delete
/// `registry.insert(StaticMode::MustAttackAwayFromSource, handle_rule_mod)` ⇒
/// `coverage::unimplemented_mechanics` reports the mode ⇒ this fails.
#[test]
fn bound_creature_reports_no_unimplemented_mechanics() {
    let (mut runner, ox) = cast_requirement_spell(KARDUR_EFFECT);

    // Reach: the graft happened, so the per-object registry scan in
    // `coverage::unimplemented_mechanics` actually visits this mode.
    assert!(
        bound_by_requirement(runner.state(), ox),
        "reach: the creature must carry the grafted requirement"
    );

    mark_public_state_all_dirty(runner.state_mut());
    derive_display_state(runner.state_mut());

    assert!(
        runner.state().objects[&ox]
            .unimplemented_mechanics
            .is_empty(),
        "a bound creature must not surface the requirement as an unimplemented \
         mechanic, got {:?}",
        runner.state().objects[&ox].unimplemented_mechanics
    );
}

/// T12 — "no such players" (Maximum Carnage ruling 2025-09-19: "if such a
/// creature can't attack players other than you, it must attack you"). With the
/// effect's controller the ONLY attackable defender, the away-from requirement
/// is unobeyable, so CR 508.1d's maximum is 1 (the generic requirement) and the
/// creature must still attack.
///
/// REVERT PROBE: delete the unconditional `MustAttackGeneric` push in
/// `AttackDeclarationConstraints::build` ⇒ `max_no_payment == 0` ⇒ declaring no
/// attackers becomes legal ⇒ leg 1 fails. (Gating the away-from push on
/// `attackable_players`, like the `MustAttackPlayer` push, would NOT flip leg 1 —
/// `MustAttackGeneric` still carries it — which is why that is not the probe.)
#[test]
fn only_avoided_player_attackable_still_forces_the_attack() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Spelled-Out Requirement", false, KARDUR_EFFECT)
        .id();
    let ox = scenario.add_creature(P1, "Ornery Ox", 2, 2).id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    assert!(
        bound_by_requirement(runner.state(), ox),
        "reach: the creature must carry the requirement"
    );

    // Leg 1 — declining is illegal even though the only defender is the player
    // the creature is supposed to avoid.
    arm_declare_attackers_against(&mut runner, &[ox], &[P0]);
    assert!(
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .is_err(),
        "CR 508.1d: the generic 'attacks each combat if able' requirement is \
         still obeyable, so declaring no attackers is illegal"
    );

    // Leg 2 — attacking the avoided player is legal and commits combat.
    arm_declare_attackers_against(&mut runner, &[ox], &[P0]);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(ox, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("with no other attackable player the creature must attack the avoided player");
    assert!(
        runner.state().combat.is_some(),
        "the forced declaration must commit combat"
    );
}

/// T14 — the `MustAttack` HALF of the grant actually reaches combat.
///
/// Every legality leg elsewhere in this file is satisfied by the away-from term
/// alone (`creature_must_attack_with_attackable_players_gated` ORs `has_must_attack`
/// with `must_attack_away`), and `bound_by_requirement` only probes
/// `MustAttackAwayFromSource` — so the suite would stay green if the grafted
/// `AddStaticMode { MustAttack }` stopped being seen. That is a live risk: #6296
/// rewrote exactly that computation (`has_local_must_attack` →
/// `check_static_ability(MustAttack, static_target_ctx(id))`).
///
/// `CombatRequirement::MustAttack::sources` is the term-specific observable.
/// `must_attack_sources_gated` unions three contributors: gated
/// `check_static_ability_sources(MustAttack)`, `StaticMode::Goaded` carriers, and
/// attackable `MustAttackPlayer` carriers. This fixture has neither of the latter
/// two (asserted below), and `MustAttackAwayFromSource` deliberately contributes
/// NO source (see `players_to_attack_away_from_gated`), so a non-empty `sources`
/// can only come from the `MustAttack` graft — via the same gate + check pair the
/// enforcement path uses.
///
/// REVERT PROBE (measured): delete the `AddStaticMode { mode: MustAttack }`
/// element from `must_attack_away_static_definition`
/// (`parser/oracle_effect/imperative.rs`) ⇒ `sources` is empty ⇒ the final
/// assertion here fails, **along with** the parse-shape assertion in
/// `neither_card_lowers_to_goad`, whose `is_requirement_grant` helper pins the
/// same two-element `modifications` vec. The other 11 tests in this file stay
/// green. Measured: `11 passed; 2 failed`.
#[test]
fn grafted_must_attack_half_reaches_the_combat_authority() {
    let (mut runner, ox) = cast_requirement_spell(KARDUR_EFFECT);
    assert!(
        bound_by_requirement(runner.state(), ox),
        "reach: the creature must carry the grafted requirement"
    );

    arm_declare_attackers(&mut runner, ox);
    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        valid.contains(&ox),
        "reach: the bound creature must be an eligible attacker, got {valid:?}"
    );

    let constraints = attacker_constraints_for_active_player(runner.state(), &valid);
    let Some(CombatRequirement::MustAttack { players, sources }) = constraints.get(&ox) else {
        panic!(
            "expected a MustAttack requirement for the bound creature, got {:?}",
            constraints.get(&ox)
        );
    };
    // Exclude the other two `sources` contributors, each by the thing that
    // actually feeds it.
    //
    // (a) `MustAttackPlayer`: `players` and the carrier list are built from the
    //     same filtered directive iterator, so empty players ⟺ no carriers.
    assert!(
        players.is_empty(),
        "no MustAttackPlayer directive exists in this fixture, got {players:?}"
    );
    // (b) `StaticMode::Goaded`: fed by `goad_static_hits_for_creature`, which
    //     supplies the CARRIER's id — so it could only yield `ox` if `ox` itself
    //     carried a functioning `Goaded` static. `goaded_by` does NOT guard this
    //     (a `Goaded` static never writes `goaded_by`); the static sweep does.
    assert!(
        !active_static_definitions(runner.state(), &runner.state().objects[&ox])
            .any(|sd| sd.mode == StaticMode::Goaded),
        "no functioning `StaticMode::Goaded` static is carried by the creature, so \
         `goad_static_hits_for_creature` cannot contribute its id to `sources`"
    );
    // Not a `sources` guard — this is the CR 701.15a property the whole file
    // exists to pin: the requirement without the designation.
    assert!(
        runner.state().objects[&ox].goaded_by.is_empty(),
        "CR 701.15a: the spelled-out requirement must not set the goad designation"
    );
    assert_eq!(
        sources,
        &vec![ox],
        "CR 508.1d: the grafted `AddStaticMode {{ MustAttack }}` must reach \
         `check_static_ability(MustAttack)` — its `affected: SelfRef` graft makes \
         the creature its own carrier. Empty here means combat no longer sees the \
         MustAttack half of the grant"
    );
}

/// T13 — CR 109.5 + CR 611.2c: the affected population's "your opponents"
/// reference is the RESOLVING controller, snapshotted; only the *set of objects*
/// is dynamic. Kardur is cast as a real creature spell so the source is a
/// battlefield permanent whose controller can change mid-window (Control Magic
/// class) — the sorcery-typed fixture used by every other test in this file puts
/// the source in a graveyard where this is unobservable.
///
/// REVERT PROBE: restore the bare
/// `FilterContext::from_source(state, effect.source_id)` at the
/// `apply_continuous_effect_filtered` scan in `layers.rs` ⇒ the population is
/// re-derived from the THIEF's perspective ⇒ P1's creature falls out (positive
/// leg fails) and P0's own creature is pulled in (negative leg fails).
#[test]
fn affected_population_does_not_follow_a_stolen_source() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let kardur = scenario
        .add_creature_to_hand_from_oracle(P0, "Kardur, Doomscourge", 3, 3, KARDUR_ORACLE)
        .id();
    let own = scenario.add_creature(P0, "Bear", 2, 2).id();
    let p1_wolf = scenario.add_creature(P1, "Wolf", 2, 2).id();
    let p2_ox = scenario.add_creature(P2, "Ornery Ox", 2, 2).id();
    let mut runner = scenario.build();
    runner.cast(kardur).resolve();

    // Reach — the ETB trigger resolved and bound P0's opponents' creatures only.
    assert!(
        bound_by_requirement(runner.state(), p1_wolf)
            && bound_by_requirement(runner.state(), p2_ox),
        "reach: Kardur's ETB must bind both opponents' creatures"
    );
    assert!(
        !bound_by_requirement(runner.state(), own),
        "reach: Kardur must not bind its own controller's creature"
    );

    // An opponent steals Kardur mid-window. `evaluate_layers` resets `controller`
    // from `base_controller` on every pass, so both fields must be set.
    {
        let obj = runner.state_mut().objects.get_mut(&kardur).unwrap();
        obj.base_controller = Some(P1);
        obj.controller = P1;
    }
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // POSITIVE — the population is still P0's opponents (CR 109.5: for a
    // triggered ability, "you" is the controller when the ability triggered).
    assert!(
        bound_by_requirement(runner.state(), p1_wolf),
        "CR 109.5: the bound population must not follow the stolen source — \
         P1's creature must stay bound"
    );
    assert!(
        bound_by_requirement(runner.state(), p2_ox),
        "CR 109.5: P2's creature must stay bound"
    );

    // NEGATIVE (the discriminator), paired with the positives above.
    assert!(
        !bound_by_requirement(runner.state(), own),
        "CR 109.5 + CR 611.2c: only the SET OF OBJECTS is dynamic — the effect \
         controller's own creature must never become bound because the source \
         changed hands"
    );
}
