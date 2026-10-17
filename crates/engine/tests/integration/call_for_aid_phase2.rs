//! Phase 2 (Call for Aid): scope-aware temporary attack prohibition (protect
//! the TARGETED opponent, not the source's controller) + duration-bound
//! sacrifice prohibition over the stolen set via the EXISTING
//! `CantBeSacrificed` chokepoint (no new `ProhibitedActivity` variant).
//!
//! V-row map: V2 targeted protection in >=3 players; V3 snapshot survival
//! across source changes; V4 shipped-user byte-compat; V5a/V5b cost-path and
//! effect-path sacrifice blocks; V6 snapshot freeze (late arrivals + flicker);
//! V7 expiry boundary; V8c Stilt-Man shared-path proof; V9 readouts.
//!
//! Call for Aid's haste clause ("They gain haste") binds the frozen stolen set
//! through the single-mass-antecedent assembly arm — the same `TrackedSet(0)`
//! the untap/sacrifice siblings carry. `call_for_aid_stolen_creatures_gain_haste`
//! proves it functionally via an immediate attack at another opponent.

use engine::game::combat::{build_declare_attackers_waiting_for, declare_attackers, AttackTarget};
use engine::game::functioning_abilities::active_static_definitions;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::static_abilities::object_has_static_other;
use engine::game::turns::execute_cleanup;
use engine::game::zones::{create_object, move_to_zone};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ContinuousModification, GameRestriction, ProhibitedActivity, RestrictionPlayerScope,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use super::rules::run_combat;

use crate::support::shared_card_db as load_db;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

const CALL_FOR_AID: &str = "Gain control of all creatures target opponent controls until end of turn. Untap those creatures. They gain haste until end of turn. You can't attack that player this turn. You can't sacrifice those creatures this turn.";

const VILLAGE_RITES: &str =
    "As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.";

const DIABOLIC_EDICT: &str = "Target player sacrifices a creature of their choice.";

const STILT_MAN: &str = "Reach\nWhenever one or more Villains you control deal combat damage to a player, gain control of target noncreature, nonland permanent that player controls until the end of your next turn. It gains \"This permanent can't be sacrificed\" until the end of your next turn.";

const CITY_HALL: &str = "When you planeswalk here, you become the Monarch.\nWhenever Chaos ensues, each player may create two tapped treasure tokens. Each player who does can't attack you during their next turn.";
const ORZHOV_ADVOKIST: &str = "At the beginning of your upkeep, each player may put two +1/+1 counters on a creature they control. If a player does, creatures that player controls can't attack you or planeswalkers you control until your next turn.";
const SANDSWIRL_WANDERGLYPH: &str = "Flying\nWhenever an opponent casts a spell during their turn, they can't attack you or planeswalkers you control this turn.\nEach opponent who attacked you or a planeswalker you control this turn can't cast spells.";
const THE_SECOND_DOCTOR: &str = "Players have no maximum hand size.\nHow Civil of You \u{2014} At the beginning of your end step, each player may draw a card. Each opponent who does can't attack you or permanents you control during their next turn.";
const WILLIE_LUMPKIN: &str = "Willie Lumpkin can't be blocked.\nWhenever Willie Lumpkin deals combat damage to an opponent, you draw a card and that player may draw a card. If they do, that player can't attack you or permanents you control during their next turn.";
const ZAGORKA: &str = "Vigilance, trample\nAt the beginning of your end step, each player may create a tapped land token named Sanctum with \"{T}: Add one mana of any color.\" Each opponent who does can't attack you during their next turn.";

/// Add `count` units of `ty` mana to `player`'s pool — deterministic payment
/// without modelling lands (mirrors `chord_of_calling.rs::add_mana`).
fn add_mana(runner: &mut GameRunner, player: PlayerId, ty: ManaType, count: usize) {
    let unit_source = ObjectId(0);
    let target = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    for _ in 0..count {
        target
            .mana_pool
            .add(ManaUnit::new(ty, unit_source, false, vec![]));
    }
}

/// CR 508.1c legality probe on a state clone (mirrors orzhov_advokist's
/// helper): can `attacker` (controlled by `controller`) attack `target`?
fn attack_legal(
    state: &GameState,
    controller: PlayerId,
    attacker: ObjectId,
    target: AttackTarget,
) -> bool {
    let mut state = state.clone();
    state.active_player = controller;
    let mut events = Vec::new();
    declare_attackers(&mut state, &[(attacker, target)], &mut events).is_ok()
}

/// Park the runner at a DeclareAttackers prompt for P0 over P1+P2 (mirrors
/// rules/combat.rs `park_3p_declare`): the submission validator recomputes
/// legality, so the hardcoded payload never masks a prohibition.
fn park_declare(runner: &mut GameRunner, attackers: &[ObjectId]) {
    let state = runner.state_mut();
    state.active_player = P0;
    state.priority_player = P0;
    state.phase = Phase::DeclareAttackers;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: attackers.to_vec(),
        valid_attack_targets: vec![AttackTarget::Player(P1), AttackTarget::Player(P2)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
}

/// Per-attacker legal targets from the REAL payload builder (V2/V9).
fn payload_targets_for(state: &GameState, attacker: ObjectId) -> Vec<AttackTarget> {
    match build_declare_attackers_waiting_for(state) {
        WaitingFor::DeclareAttackers {
            valid_attack_targets_by_attacker: Some(map),
            ..
        } => map.get(&attacker).cloned().unwrap_or_default(),
        other => panic!("expected a DeclareAttackers payload, got {other:?}"),
    }
}

/// Chokepoint query: does `object` carry the sacrifice prohibition (printed
/// or transient-granted)?
fn cant_be_sacrificed(state: &GameState, object: ObjectId) -> bool {
    object_has_static_other(state, object, "CantBeSacrificed")
}

/// 3-player stage: P0 holds Call for Aid ({4}{R} funded) + one own creature;
/// P1 holds two creatures; P2 is present but bare. All creatures attack-ready.
struct AidStage {
    runner: GameRunner,
    aid: ObjectId,
    p0_attacker: ObjectId,
    p1_a: ObjectId,
    p1_b: ObjectId,
}

fn stage_3p_aid() -> AidStage {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p0_attacker = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let p1_a = scenario.add_creature(P1, "P1 Goblin A", 1, 1).id();
    let p1_b = scenario.add_creature(P1, "P1 Goblin B", 1, 1).id();
    let aid = scenario
        .add_spell_to_hand_from_oracle(P0, "Call for Aid", false, CALL_FOR_AID)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Red, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 4);
    for id in [p0_attacker, p1_a, p1_b] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("staged creature exists")
            .summoning_sick = false;
    }
    // Tap the soon-stolen victims: fresh creatures enter untapped, so without
    // this the sibling `!tapped` asserts below pass vacuously. The aid must
    // untap them for real.
    for id in [p1_a, p1_b] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("staged victim exists")
            .tapped = true;
    }
    AidStage {
        runner,
        aid,
        p0_attacker,
        p1_a,
        p1_b,
    }
}

/// Cast the staged aid targeting P1 and assert the sibling reach-guards: both
/// creatures changed control to P0 and untapped (the chain ran past the gain
/// head), and exactly one restriction was installed.
fn cast_aid_and_check_siblings(stage: &mut AidStage) {
    let outcome = stage.runner.cast(stage.aid).target_player(P1).resolve();
    let state = outcome.state();
    assert_eq!(state.objects[&stage.p1_a].controller, P0);
    assert_eq!(state.objects[&stage.p1_b].controller, P0);
    assert!(!state.objects[&stage.p1_a].tapped);
    assert!(!state.objects[&stage.p1_b].tapped);
    assert_eq!(
        state.restrictions.len(),
        1,
        "exactly the attack prohibition is installed"
    );
    assert!(
        matches!(
            &state.restrictions[0],
            GameRestriction::ProhibitActivity {
                affected_players: RestrictionPlayerScope::SpecificPlayer(P0),
                activity: ProhibitedActivity::Attack {
                    protected_player: Some(P1),
                    ..
                },
                ..
            }
        ),
        "P0 is restricted from attacking the targeted opponent P1, got {:?}",
        state.restrictions[0]
    );
}

/// V2 (row 2) — CR 508.1c + CR 109.5 + CR 608.2c: in a >=3-player game the
/// targeted opponent (not the source's controller) is protected. Pre-cast
/// reach-guard, post-cast payload exclusion, and submission reject/accept.
#[test]
fn call_for_aid_protects_targeted_opponent_in_three_player() {
    let mut stage = stage_3p_aid();
    // Reach-guard: pre-cast, P0's creature can attack P1.
    assert!(
        attack_legal(
            stage.runner.state(),
            P0,
            stage.p0_attacker,
            AttackTarget::Player(P1)
        ),
        "pre-cast the attack at P1 must be legal"
    );

    cast_aid_and_check_siblings(&mut stage);

    // Legality: P1 barred, P2 open.
    assert!(
        !attack_legal(
            stage.runner.state(),
            P0,
            stage.p0_attacker,
            AttackTarget::Player(P1)
        ),
        "P0 must not attack the targeted opponent P1"
    );
    assert!(
        attack_legal(
            stage.runner.state(),
            P0,
            stage.p0_attacker,
            AttackTarget::Player(P2)
        ),
        "P0 must still attack the untargeted opponent P2"
    );

    // Payload: P1 absent from P0's per-attacker targets while P2 present.
    park_declare(&mut stage.runner, &[stage.p0_attacker]);
    let targets = payload_targets_for(stage.runner.state(), stage.p0_attacker);
    assert!(
        !targets.contains(&AttackTarget::Player(P1)),
        "payload must exclude P1, got {targets:?}"
    );
    assert!(
        targets.contains(&AttackTarget::Player(P2)),
        "payload must keep P2, got {targets:?}"
    );

    // Submission: DeclareAttackers at P1 rejected, at P2 accepted.
    park_declare(&mut stage.runner, &[stage.p0_attacker]);
    assert!(
        stage
            .runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(stage.p0_attacker, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .is_err(),
        "submitting the attack at P1 must be rejected"
    );
    park_declare(&mut stage.runner, &[stage.p0_attacker]);
    stage
        .runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(stage.p0_attacker, AttackTarget::Player(P2))],
            bands: vec![],
        })
        .expect("submitting the attack at P2 must be accepted");
}

/// V10 (row 10) — CR 702.10b + CR 608.2c: the stolen creatures gain haste —
/// "They gain haste" binds the frozen stolen set (the same `TrackedSet(0)`
/// the untap/sacrifice siblings carry), so a stolen goblin attacks another
/// opponent immediately. Without the grant it is summoning-sick under its new
/// controller and the attack is illegal, so legality itself discriminates.
#[test]
fn call_for_aid_stolen_creatures_gain_haste() {
    let mut stage = stage_3p_aid();
    cast_aid_and_check_siblings(&mut stage);
    // Haste proof, functional: the stolen goblin may attack the untargeted
    // opponent immediately despite changing control this turn.
    assert!(
        attack_legal(
            stage.runner.state(),
            P0,
            stage.p1_a,
            AttackTarget::Player(P2)
        ),
        "a hasted stolen creature must attack another opponent immediately"
    );
    // Haste does not lift the attack prohibition at the targeted opponent.
    assert!(
        !attack_legal(
            stage.runner.state(),
            P0,
            stage.p1_a,
            AttackTarget::Player(P1)
        ),
        "the stolen creature must still not attack P1"
    );
    // Declare for real through the combat pipeline.
    park_declare(&mut stage.runner, &[stage.p1_a]);
    stage
        .runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(stage.p1_a, AttackTarget::Player(P2))],
            bands: vec![],
        })
        .expect("the hasted stolen creature's attack must be accepted");
    assert!(
        stage.runner.state().objects[&stage.p1_a].tapped,
        "declaring the attack taps the attacker"
    );
}

/// V3 (row 3) — CR 109.5 + CR 611.2c: the protection is a creation-time
/// snapshot, not a live source lookup. It survives the source changing
/// controller mid-stack and again after resolution; the untargeted opponent
/// stays attackable throughout (hostile).
#[test]
fn call_for_aid_protection_survives_source_controller_changes() {
    // Part A: the source changes controller while on the stack — the already-
    // chosen player target (P1) is still the protected player at creation.
    let mut stage = stage_3p_aid();
    let card_id = stage.runner.state().objects[&stage.aid].card_id;
    stage
        .runner
        .act(GameAction::CastSpell {
            object_id: stage.aid,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce must be accepted");
    // Answer the "target opponent" slot through the standard prompt.
    match stage.runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { .. } => {
            stage
                .runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                })
                .expect("choosing P1 must be accepted");
        }
        other => panic!("expected a TargetSelection prompt, got {other:?}"),
    }
    assert_eq!(
        stage.runner.state().objects[&stage.aid].zone,
        Zone::Stack,
        "the aid must be on the stack before the hostile controller change"
    );
    stage
        .runner
        .state_mut()
        .objects
        .get_mut(&stage.aid)
        .expect("aid on stack")
        .controller = P2;
    stage.runner.advance_until_stack_empty();
    assert!(
        matches!(
            stage.runner.state().restrictions.as_slice(),
            [GameRestriction::ProhibitActivity {
                activity: ProhibitedActivity::Attack {
                    protected_player: Some(P1),
                    ..
                },
                ..
            }]
        ),
        "mid-stack controller change must not move the snapshot off P1, got {:?}",
        stage.runner.state().restrictions
    );

    // Part B: post-resolution the source (now in the graveyard) changes
    // controller — protection still holds (snapshot read, not live lookup).
    let mut stage = stage_3p_aid();
    cast_aid_and_check_siblings(&mut stage);
    assert_eq!(
        stage.runner.state().objects[&stage.aid].zone,
        Zone::Graveyard
    );
    stage
        .runner
        .state_mut()
        .objects
        .get_mut(&stage.aid)
        .expect("aid in graveyard")
        .controller = P1;
    assert!(
        matches!(
            &stage.runner.state().restrictions[0],
            GameRestriction::ProhibitActivity {
                activity: ProhibitedActivity::Attack {
                    protected_player: Some(P1),
                    ..
                },
                ..
            }
        ),
        "post-resolution controller change must not move the snapshot"
    );
    assert!(
        !attack_legal(
            stage.runner.state(),
            P0,
            stage.p0_attacker,
            AttackTarget::Player(P1)
        ),
        "P1 must stay barred after the source changes controller"
    );
    // Hostile: the untargeted opponent is unaffected by both changes.
    assert!(
        attack_legal(
            stage.runner.state(),
            P0,
            stage.p0_attacker,
            AttackTarget::Player(P2)
        ),
        "P2 must stay attackable"
    );
}

/// V4 (row 4) — byte-compat: every shipped user of the temporary
/// `ProhibitActivity::Attack` prohibition parses WITHOUT the new
/// `protected_scope` key (serde-defaulted field). Each card's reach-guard —
/// its export contains an `Attack` node — keeps the negative honest.
#[test]
fn shipped_attack_prohibitions_emit_no_protected_scope_key() {
    const ROW4: &[(&str, &str, &[&str], &[&str])] = &[
        ("City Hall", CITY_HALL, &["Plane"], &["Chicago"]),
        (
            "Orzhov Advokist",
            ORZHOV_ADVOKIST,
            &["Creature"],
            &["Human", "Advisor"],
        ),
        (
            "Sandswirl Wanderglyph",
            SANDSWIRL_WANDERGLYPH,
            &["Artifact", "Creature"],
            &["Golem"],
        ),
        (
            "The Second Doctor",
            THE_SECOND_DOCTOR,
            &["Creature"],
            &["Time Lord", "Doctor"],
        ),
        (
            "Willie Lumpkin, Postman",
            WILLIE_LUMPKIN,
            &["Creature"],
            &["Human", "Citizen"],
        ),
        (
            "Zagorka, Mother of Sanctum",
            ZAGORKA,
            &["Creature"],
            &["Human", "Peasant"],
        ),
    ];
    assert_eq!(ROW4.len(), 6, "the row-4 census is exactly six cards");
    for (name, oracle, cores, subtypes) in ROW4 {
        let cores: Vec<String> = cores.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        let parsed = parse_oracle_text(oracle, name, &[], &cores, &subtypes);
        let json = serde_json::to_string(&parsed).expect("parsed card serializes");
        assert!(
            json.contains("\"type\":\"Attack\""),
            "{name}: reach-guard — export must carry an Attack prohibition"
        );
        assert!(
            !json.contains("protected_scope"),
            "{name}: legacy Attack nodes must not emit protected_scope"
        );
    }
}

/// 2-player sacrifice stage: P0 holds Call for Aid (funded) + Village Rites
/// (funded); P1 holds two creatures + Diabolic Edict (funded). Unused cards in
/// hand are harmless, so V5a/V5b/V6/V7 share the one stage.
struct SacStage {
    runner: GameRunner,
    aid: ObjectId,
    rites: ObjectId,
    edict: ObjectId,
    edict_two: ObjectId,
    p1_a: ObjectId,
    p1_b: ObjectId,
}

fn stage_2p_sac() -> SacStage {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let p1_a = scenario.add_creature(P1, "P1 Goblin A", 1, 1).id();
    let p1_b = scenario.add_creature(P1, "P1 Goblin B", 1, 1).id();
    // Pad P0's library: Village Rites draws two, and an empty library would
    // deck P0 mid-test (elimination exiles owned cards, masking zones).
    for _ in 0..5 {
        scenario.add_card_to_library_top(P0, "Plains");
    }
    let aid = scenario
        .add_spell_to_hand_from_oracle(P0, "Call for Aid", false, CALL_FOR_AID)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        .id();
    let rites = scenario
        .add_spell_to_hand_from_oracle(P0, "Village Rites", true, VILLAGE_RITES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let edict = scenario
        .add_spell_to_hand_from_oracle(P1, "Diabolic Edict", true, DIABOLIC_EDICT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let edict_two = scenario
        .add_spell_to_hand_from_oracle(P1, "Diabolic Edict", true, DIABOLIC_EDICT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Red, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 4);
    add_mana(&mut runner, P0, ManaType::Black, 1);
    add_mana(&mut runner, P1, ManaType::Black, 4);
    SacStage {
        runner,
        aid,
        rites,
        edict,
        edict_two,
        p1_a,
        p1_b,
    }
}

/// Cast the staged aid targeting P1; assert the theft + prohibition
/// reach-guards (both creatures stolen AND prohibited).
fn cast_aid_and_check_theft(stage: &mut SacStage) {
    stage.runner.cast(stage.aid).target_player(P1).resolve();
    let state = stage.runner.state();
    assert_eq!(state.objects[&stage.p1_a].controller, P0);
    assert_eq!(state.objects[&stage.p1_b].controller, P0);
    assert!(
        cant_be_sacrificed(state, stage.p1_a) && cant_be_sacrificed(state, stage.p1_b),
        "both stolen creatures must carry the prohibition"
    );
}

/// Hand priority to P1 (CR 117.3c) so they may cast (mirrors the edict test).
fn hand_priority_to_p1(runner: &mut GameRunner) {
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
}

fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == player && o.zone == Zone::Hand)
        .count()
}

/// Announce a sacrifice-cost spell and select its cost victim (mirrors the
/// cost_zone_pipeline Village Rites harness), then resolve fully.
fn cast_sacrifice_cost_spell(runner: &mut GameRunner, spell: ObjectId, victim: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce must be accepted");
    runner
        .act(GameAction::SelectCards {
            cards: vec![victim],
        })
        .expect("sacrifice selection must be accepted");
    runner.advance_until_stack_empty();
}

/// V5a (row 5, cost path) — CR 118.3 + CR 109.5 + CR 701.21: the barred
/// caster cannot fund a sacrifice cost with a stolen victim — the payment
/// fails (rejected at announce when no victim is eligible, or at selection
/// when the barred victim is named) so the spell never resolves and nothing
/// moves. Paired with the unprohibited-creature reach-guard (normal payment
/// works).
#[test]
fn call_for_aid_blocks_sacrifice_cost_path() {
    // Reach-guard: an unprohibited creature pays the cost normally.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let fodder = scenario.add_creature(P0, "Rites Fodder", 1, 1).id();
        for _ in 0..5 {
            scenario.add_card_to_library_top(P0, "Plains");
        }
        let rites = scenario
            .add_spell_to_hand_from_oracle(P0, "Village Rites", true, VILLAGE_RITES)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            })
            .id();
        let mut runner = scenario.build();
        add_mana(&mut runner, P0, ManaType::Black, 1);
        let hand_before = hand_count(&runner, P0);
        cast_sacrifice_cost_spell(&mut runner, rites, fodder);
        assert_eq!(
            runner.state().objects[&fodder].zone,
            Zone::Graveyard,
            "unprohibited payment sacrifices normally"
        );
        assert_eq!(
            hand_count(&runner, P0),
            hand_before + 1,
            "spell left hand (-1), draw two (+2)"
        );
    }
    // Prohibited: P0 controls only the two stolen creatures.
    {
        let mut stage = stage_2p_sac();
        cast_aid_and_check_theft(&mut stage);
        let hand_before = hand_count(&stage.runner, P0);
        // CR 118.3: the barred caster names a stolen victim. The payment
        // fails at announce (empty eligible set) or at selection (barred
        // victim rejected) — either way the spell never resolves.
        let card_id = stage.runner.state().objects[&stage.rites].card_id;
        let announce = stage.runner.act(GameAction::CastSpell {
            object_id: stage.rites,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        });
        if announce.is_ok() {
            let selection = stage.runner.act(GameAction::SelectCards {
                cards: vec![stage.p1_a],
            });
            assert!(
                selection.is_err(),
                "the barred cost victim must be rejected at selection"
            );
        }
        let state = stage.runner.state();
        assert_eq!(
            state.objects[&stage.p1_a].zone,
            Zone::Battlefield,
            "the prohibited cost victim must stay on the battlefield"
        );
        assert_eq!(
            state.objects[&stage.p1_b].zone,
            Zone::Battlefield,
            "the other stolen creature is untouched"
        );
        assert_eq!(
            state.objects[&stage.rites].zone,
            Zone::Hand,
            "the unpaid spell stays in hand"
        );
        assert_eq!(
            hand_count(&stage.runner, P0),
            hand_before,
            "no draw: the spell never resolved"
        );
    }
}

/// V5b (row 5, effect path) — CR 701.21 + CR 101.2: a resolving edict takes
/// nothing from a player whose creatures are all prohibited — no zone change.
/// Paired with the unprohibited-creature reach-guard.
#[test]
fn call_for_aid_blocks_sacrifice_effect_path() {
    // Reach-guard: the edict sacrifices an unprohibited creature.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let fodder = scenario.add_creature(P0, "Edict Fodder", 1, 1).id();
        let edict = scenario
            .add_spell_to_hand_from_oracle(P1, "Diabolic Edict", true, DIABOLIC_EDICT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 1,
            })
            .id();
        let mut runner = scenario.build();
        add_mana(&mut runner, P1, ManaType::Black, 2);
        hand_priority_to_p1(&mut runner);
        runner.cast(edict).target_player(P0).resolve();
        assert_eq!(
            runner.state().objects[&fodder].zone,
            Zone::Graveyard,
            "unprohibited edict victim is sacrificed"
        );
    }
    // Prohibited: P0 controls only the two stolen creatures.
    {
        let mut stage = stage_2p_sac();
        cast_aid_and_check_theft(&mut stage);
        hand_priority_to_p1(&mut stage.runner);
        // Both candidates are barred, so no choice prompt is raised at all:
        // the pool excludes prohibited permanents (CR 608.2d) and the empty
        // pool resolves as a no-op (CR 609.3) — the edict can no longer be
        // answered by naming a barred victim and evading.
        let outcome = stage.runner.cast(stage.edict).target_player(P0).resolve();
        // Anti-vacuity: the edict RESOLVED (not halted at a choice prompt).
        assert_eq!(
            outcome.state().objects[&stage.edict].zone,
            Zone::Graveyard,
            "the edict must resolve for the survival asserts to mean anything"
        );
        assert!(
            matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
            "the pipeline must halt at Priority, got {:?}",
            outcome.final_waiting_for()
        );
        let state = stage.runner.state();
        assert_eq!(
            state.objects[&stage.p1_a].zone,
            Zone::Battlefield,
            "the prohibited edict victims must stay on the battlefield"
        );
        assert_eq!(state.objects[&stage.p1_b].zone, Zone::Battlefield);
        assert_eq!(state.objects[&stage.p1_a].controller, P0);
        assert_eq!(state.objects[&stage.p1_b].controller, P0);
        assert!(
            cant_be_sacrificed(state, stage.p1_a) && cant_be_sacrificed(state, stage.p1_b),
            "the prohibition survives the edict"
        );
    }
}

/// M3 companion — CR 118.3 + CR 109.5: with mixed candidates (one
/// stolen/barred, one normal), the eligibility filter separates them inside a
/// single payment — the barred victim is rejected at selection while the
/// normal one pays normally. The bar is per-object, not per-player.
#[test]
fn call_for_aid_mixed_sacrifice_candidates() {
    let mut stage = stage_2p_sac();
    cast_aid_and_check_theft(&mut stage);
    // A normal creature entering after resolution is outside the frozen set
    // (V6) and carries no bar.
    let fodder = create_battlefield_creature(&mut stage.runner, P0, "Rites Fodder");
    let hand_before = hand_count(&stage.runner, P0);
    // One payment: the barred victim is rejected, the normal one accepted.
    let card_id = stage.runner.state().objects[&stage.rites].card_id;
    stage
        .runner
        .act(GameAction::CastSpell {
            object_id: stage.rites,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce must be accepted");
    assert!(
        stage
            .runner
            .act(GameAction::SelectCards {
                cards: vec![stage.p1_a],
            })
            .is_err(),
        "the barred cost victim must be rejected at selection"
    );
    stage
        .runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("the normal cost victim must be accepted");
    stage.runner.advance_until_stack_empty();
    assert_eq!(
        stage.runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "the normal victim pays the cost"
    );
    assert_eq!(
        stage.runner.state().objects[&stage.p1_a].zone,
        Zone::Battlefield,
        "the rejected barred victim stays"
    );
    assert_eq!(
        hand_count(&stage.runner, P0),
        hand_before + 1,
        "spell left hand (-1), draw two (+2)"
    );
}

/// M4 companion — CR 109.5 + CR 611.2c: the resolution-created bar binds the
/// INSTALLER (the grant's TCE controller), not the object. When a stolen
/// creature later passes to a third player, that player sacrifices it freely
/// — the bar does not follow the object. Only printed statics bar all actors.
#[test]
fn call_for_aid_bar_does_not_follow_control_change() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_a = scenario.add_creature(P1, "P1 Goblin A", 1, 1).id();
    let p1_b = scenario.add_creature(P1, "P1 Goblin B", 1, 1).id();
    for _ in 0..5 {
        scenario.add_card_to_library_top(P2, "Plains");
    }
    let aid = scenario
        .add_spell_to_hand_from_oracle(P0, "Call for Aid", false, CALL_FOR_AID)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        .id();
    let rites = scenario
        .add_spell_to_hand_from_oracle(P2, "Village Rites", true, VILLAGE_RITES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Red, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 4);
    add_mana(&mut runner, P2, ManaType::Black, 1);
    runner.cast(aid).target_player(P1).resolve();
    {
        let state = runner.state();
        assert_eq!(state.objects[&p1_a].controller, P0);
        assert_eq!(state.objects[&p1_b].controller, P0);
        assert!(
            cant_be_sacrificed(state, p1_a) && cant_be_sacrificed(state, p1_b),
            "both stolen creatures must carry the prohibition"
        );
    }
    // Subsequent control change: end the THEFT of p1_a (drop its
    // `ChangeController` grant) and pass it to P2. The sacrifice bar is a
    // separate installer-bound grant and stays installed.
    pass_control_to(&mut runner, p1_a, P2);
    assert_eq!(
        runner.state().objects[&p1_a].controller,
        P2,
        "the control change must stick for the sacrifice below to mean anything"
    );
    // P2 — never bound by another player's "you" — sacrifices freely.
    runner.state_mut().priority_player = P2;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P2 };
    let hand_before = hand_count(&runner, P2);
    cast_sacrifice_cost_spell(&mut runner, rites, p1_a);
    assert_eq!(
        runner.state().objects[&p1_a].zone,
        Zone::Graveyard,
        "the later controller sacrifices the creature normally"
    );
    assert_eq!(
        hand_count(&runner, P2),
        hand_before + 1,
        "spell left hand (-1), draw two (+2)"
    );
    // The sibling still stolen by the installer stays barred and controlled.
    assert_eq!(runner.state().objects[&p1_b].controller, P0);
    assert!(
        cant_be_sacrificed(runner.state(), p1_b),
        "the installer's bar still binds its own stolen creature"
    );
}

/// CR 109.5 + CR 701.21a (Matt M4): the later-controller control driven
/// through REAL-CARD loading. The committed fixture's Call for Aid row must
/// carry the parser-emitted `bound_actor: You` so the bar binds the
/// installer, not the object. Fails on the stale row: an absent binding
/// defaults to `None`, an object-wide prohibition, and P2 cannot sacrifice.
#[test]
fn call_for_aid_real_card_bar_does_not_follow_control_change() {
    let Some(db) = load_db() else {
        return;
    };

    // Fixture-shape pin: the nested "you can't sacrifice those creatures"
    // def carries the actor binding the parser emits.
    let face = db
        .get_face_by_name("Call for Aid")
        .expect("fixture carries Call for Aid");
    let v = serde_json::to_value(face).expect("face serializes");
    let mut node = &v["abilities"][0];
    for _ in 0..4 {
        node = &node["sub_ability"];
    }
    assert_eq!(
        node["effect"]["static_abilities"][0]["bound_actor"],
        serde_json::json!("You"),
        "fixture row must carry the parser-emitted bound_actor"
    );

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_a = scenario.add_creature(P1, "P1 Goblin A", 1, 1).id();
    let p1_b = scenario.add_creature(P1, "P1 Goblin B", 1, 1).id();
    for _ in 0..5 {
        scenario.add_card_to_library_top(P2, "Plains");
    }
    let aid = scenario.add_real_card(P0, "Call for Aid", Zone::Hand, db);
    let rites = scenario
        .add_spell_to_hand_from_oracle(P2, "Village Rites", true, VILLAGE_RITES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Red, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 4);
    add_mana(&mut runner, P2, ManaType::Black, 1);
    runner.cast(aid).target_player(P1).resolve();
    {
        let state = runner.state();
        assert_eq!(state.objects[&p1_a].controller, P0);
        assert_eq!(state.objects[&p1_b].controller, P0);
        assert!(
            cant_be_sacrificed(state, p1_a) && cant_be_sacrificed(state, p1_b),
            "both stolen creatures must carry the prohibition"
        );
    }
    // Subsequent control change: end the THEFT of p1_a and pass it to P2.
    pass_control_to(&mut runner, p1_a, P2);
    assert_eq!(
        runner.state().objects[&p1_a].controller,
        P2,
        "the control change must stick for the sacrifice below to mean anything"
    );
    // P2 — never bound by another player's "you" — sacrifices freely.
    runner.state_mut().priority_player = P2;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P2 };
    let hand_before = hand_count(&runner, P2);
    cast_sacrifice_cost_spell(&mut runner, rites, p1_a);
    assert_eq!(
        runner.state().objects[&p1_a].zone,
        Zone::Graveyard,
        "the later controller sacrifices the creature normally"
    );
    assert_eq!(
        hand_count(&runner, P2),
        hand_before + 1,
        "spell left hand (-1), draw two (+2)"
    );
    // The sibling still stolen by the installer stays barred and controlled.
    assert_eq!(runner.state().objects[&p1_b].controller, P0);
    assert!(
        cant_be_sacrificed(runner.state(), p1_b),
        "the installer's bar still binds its own stolen creature"
    );
}

/// Create a 2/2 creature directly on the battlefield (mirrors orzhov_advokist's
/// helper) — models a creature ENTERING after the aid resolved.
fn create_battlefield_creature(
    runner: &mut GameRunner,
    controller: PlayerId,
    name: &str,
) -> ObjectId {
    let card = CardId(runner.state().next_object_id);
    let object = create_object(
        runner.state_mut(),
        card,
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let creature = runner
        .state_mut()
        .objects
        .get_mut(&object)
        .expect("created object");
    creature.card_types.core_types = vec![CoreType::Creature];
    creature.base_card_types = creature.card_types.clone();
    creature.power = Some(2);
    creature.toughness = Some(2);
    creature.base_power = Some(2);
    creature.base_toughness = Some(2);
    creature.summoning_sick = false;
    object
}

/// V6 (row 6) — CR 611.2c affected-set freeze via per-object `SpecificObject`
/// TCEs: a creature entering after resolution is not covered, and a flickered
/// (CR 400.7 new incarnation) member drops out when its pin is pruned on exit.
/// Closed behaviorally: the edict takes the free creatures, never the member.
#[test]
fn call_for_aid_covers_snapshotted_set_only() {
    let mut stage = stage_2p_sac();
    cast_aid_and_check_theft(&mut stage);

    // Late arrival enters after resolution: not covered; members still are.
    let late = create_battlefield_creature(&mut stage.runner, P0, "Late Arrival");
    assert!(
        !cant_be_sacrificed(stage.runner.state(), late),
        "a creature entering after resolution must be sacrificable"
    );
    assert!(cant_be_sacrificed(stage.runner.state(), stage.p1_a));
    assert!(cant_be_sacrificed(stage.runner.state(), stage.p1_b));

    // Flicker hostile: p1_a leaves and returns (new incarnation) — its
    // SpecificObject pin is pruned on exit, so the grant drops.
    move_to_zone(
        stage.runner.state_mut(),
        stage.p1_a,
        Zone::Exile,
        &mut Vec::new(),
    );
    move_to_zone(
        stage.runner.state_mut(),
        stage.p1_a,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    assert_eq!(
        stage.runner.state().objects[&stage.p1_a].zone,
        Zone::Battlefield,
        "the flickered member must return to the battlefield"
    );
    assert!(
        !cant_be_sacrificed(stage.runner.state(), stage.p1_a),
        "the flickered (new-incarnation) member must drop out"
    );
    assert!(
        cant_be_sacrificed(stage.runner.state(), stage.p1_b),
        "the unmoved member must stay covered"
    );

    assert_eq!(
        stage.runner.state().objects[&stage.p1_a].controller,
        P1,
        "the flickered member returns under its owner's control"
    );

    // Behavioral close: edicts take each free creature; the covered member
    // survives both. P1 controls only the flickered creature (singleton fast
    // path); P0 controls the covered member plus the late arrival (only the
    // free late arrival is listed -- prohibited creatures are excluded from
    // candidates (CR 608.2d) -- so the harness chooses it via
    // EffectZoneChoice).
    hand_priority_to_p1(&mut stage.runner);
    stage.runner.cast(stage.edict).target_player(P1).resolve();
    assert_eq!(
        stage.runner.state().objects[&stage.p1_a].zone,
        Zone::Graveyard,
        "the flickered creature is sacrificable"
    );
    hand_priority_to_p1(&mut stage.runner);
    // Only the free late arrival is listed (the covered member is excluded);
    // P0 chooses it.
    stage
        .runner
        .cast(stage.edict_two)
        .target_player(P0)
        .effect_zone(&[late])
        .resolve();
    assert_eq!(
        stage.runner.state().objects[&late].zone,
        Zone::Graveyard,
        "the late arrival is sacrificable"
    );
    assert_eq!(
        stage.runner.state().objects[&stage.p1_b].zone,
        Zone::Battlefield,
        "the covered member survives both edicts"
    );
}

/// V7 (row 7) — CR 514.2 expiry boundary: both prohibitions live
/// pre-cleanup; post-cleanup the restriction is pruned, the grants lapse, and
/// the actions are legal again.
#[test]
fn call_for_aid_prohibitions_expire_at_cleanup() {
    let mut stage = stage_3p_aid();
    cast_aid_and_check_siblings(&mut stage);

    // Pre-cleanup: both prohibitions live.
    assert!(
        !attack_legal(
            stage.runner.state(),
            P0,
            stage.p0_attacker,
            AttackTarget::Player(P1)
        ),
        "pre-cleanup the attack at P1 must be barred"
    );
    assert!(
        cant_be_sacrificed(stage.runner.state(), stage.p1_a)
            && cant_be_sacrificed(stage.runner.state(), stage.p1_b),
        "pre-cleanup both stolen creatures must be prohibited"
    );

    // The expiry boundary.
    execute_cleanup(stage.runner.state_mut(), &mut Vec::new());

    // Post-cleanup: restriction pruned, grants lapsed, actions legal again.
    assert!(
        stage.runner.state().restrictions.is_empty(),
        "the EndOfTurn restriction must prune at cleanup"
    );
    assert!(
        !cant_be_sacrificed(stage.runner.state(), stage.p1_a)
            && !cant_be_sacrificed(stage.runner.state(), stage.p1_b),
        "the UntilEndOfTurn grants must lapse at cleanup"
    );
    assert!(
        attack_legal(
            stage.runner.state(),
            P0,
            stage.p0_attacker,
            AttackTarget::Player(P1)
        ),
        "post-cleanup the attack at P1 must be legal again"
    );
}

/// Drive trigger targeting + ordering + priority until the stack is empty
/// (mirrors the alania/rev loops). A single-legal-target trigger is
/// auto-assigned without a prompt, so firing is proven by the caller's
/// resolution asserts (control change), not by prompt observation here.
fn drive_trigger_to_stack_empty(runner: &mut GameRunner, target: ObjectId) {
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = slot
                    .legal_targets
                    .iter()
                    .find(|t| **t == TargetRef::Object(target))
                    .cloned();
                assert!(
                    choice.is_some(),
                    "the bauble must be a legal trigger target, got {:?}",
                    slot.legal_targets
                );
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("choose trigger target must be accepted");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger ordering must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance");
            }
            other => panic!("unexpected prompt while resolving trigger: {other:?}"),
        }
    }
    assert!(runner.state().stack.is_empty(), "the stack must drain");
}

/// V8c (row 8c) — shared-path proof: SHIPPED Stilt-Man (verbatim Oracle)
/// prohibits sacrifice through the SAME shape-C grant → TCE → chokepoint path
/// Call for Aid uses, with its `UntilEndOfNextTurnOf` duration honored past
/// this turn's cleanup. The synthetic probe edict is reach-guarded by first
/// sacrificing an unprotected bauble, so the later survival is meaningful.
#[test]
fn stilt_man_shares_the_sacrifice_prohibition_path() {
    const PROBE_EDICT: &str = "Target player sacrifices an enchantment of their choice.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stiltman = scenario
        .add_creature_from_oracle(P0, "Stilt-Man, Towering Terror", 4, 2, STILT_MAN)
        .id();
    let villain = scenario.add_creature(P0, "Villain Thug", 2, 2).id();
    let bauble_a = scenario
        .add_creature(P1, "Bauble A", 0, 0)
        .as_enchantment()
        .id();
    let bauble_b = scenario
        .add_creature(P1, "Bauble B", 0, 0)
        .as_enchantment()
        .id();
    let probe_one = scenario
        .add_spell_to_hand_from_oracle(P1, "Stilt-Man Test Edict", true, PROBE_EDICT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let probe_two = scenario
        .add_spell_to_hand_from_oracle(P1, "Stilt-Man Test Edict", true, PROBE_EDICT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P1, ManaType::Black, 4);
    // The attacker is a Villain; both baubles are noncreature nonland
    // permanents (the trigger's target filter rejects anything else).
    // Stamp base AND live subtypes: the layer system reseeds live types from
    // base every pass, so a live-only push would be wiped before combat.
    let villain_obj = runner
        .state_mut()
        .objects
        .get_mut(&villain)
        .expect("villain exists");
    villain_obj.card_types.subtypes.push("Villain".to_string());
    villain_obj.base_card_types = villain_obj.card_types.clone();
    villain_obj.summoning_sick = false;
    for bauble in [bauble_a, bauble_b] {
        let obj = &runner.state().objects[&bauble];
        assert!(
            obj.card_types.core_types.contains(&CoreType::Enchantment),
            "bauble must be an enchantment"
        );
        assert!(
            !obj.card_types.core_types.contains(&CoreType::Creature),
            "bauble must be a noncreature permanent"
        );
    }
    let _ = stiltman;

    // Reach-guard: the probe edict parses and resolves as a sacrifice effect —
    // P1 sacrifices their own unprotected bauble A.
    hand_priority_to_p1(&mut runner);
    runner
        .cast(probe_one)
        .target_player(P1)
        .effect_zone(&[bauble_a])
        .resolve();
    assert_eq!(
        runner.state().objects[&bauble_a].zone,
        Zone::Graveyard,
        "reach-guard: the probe edict must sacrifice an unprotected enchantment"
    );

    // P0's Villain deals combat damage to P1; Stilt-Man triggers targeting
    // bauble B.
    run_combat(&mut runner, vec![villain], vec![]);
    drive_trigger_to_stack_empty(&mut runner, bauble_b);
    assert_eq!(
        runner.state().objects[&bauble_b].controller,
        P0,
        "the trigger must move bauble B to P0 (resolution reach-guard)"
    );
    assert!(
        cant_be_sacrificed(runner.state(), bauble_b),
        "the stolen permanent must carry the prohibition via the same path"
    );

    // Behavioral: the probe edict cannot take the prohibited bauble B.
    // (Re-fund: pools drained on the combat phase changes.)
    add_mana(&mut runner, P1, ManaType::Black, 2);
    hand_priority_to_p1(&mut runner);
    runner.cast(probe_two).target_player(P0).resolve();
    assert_eq!(
        runner.state().objects[&bauble_b].zone,
        Zone::Battlefield,
        "the prohibited permanent must survive the edict"
    );

    // Duration honored: UntilEndOfNextTurnOf survives this turn's cleanup.
    execute_cleanup(runner.state_mut(), &mut Vec::new());
    assert_eq!(
        runner.state().objects[&bauble_b].controller,
        P0,
        "control must persist past this turn's cleanup"
    );
    assert!(
        cant_be_sacrificed(runner.state(), bauble_b),
        "the prohibition must persist past this turn's cleanup"
    );
}

/// V9 (row 9) — readouts: the DeclareAttackers payload excludes the protected
/// player per-attacker (the client consumes it without inference), and each
/// stolen creature's derived statics carry the granted prohibition (the
/// layers graft, identical to Stilt-Man). No new UI.
#[test]
fn call_for_aid_prohibitions_surface_in_readouts() {
    let mut stage = stage_3p_aid();
    cast_aid_and_check_siblings(&mut stage);

    // Attack payload: P1 excluded, P2 kept.
    park_declare(&mut stage.runner, &[stage.p0_attacker]);
    let targets = payload_targets_for(stage.runner.state(), stage.p0_attacker);
    assert!(
        !targets.contains(&AttackTarget::Player(P1)),
        "payload must exclude P1, got {targets:?}"
    );
    assert!(
        targets.contains(&AttackTarget::Player(P2)),
        "payload must keep P2, got {targets:?}"
    );

    // Sacrifice derived statics: the grant is visible on each creature.
    for id in [stage.p1_a, stage.p1_b] {
        let state = stage.runner.state();
        let obj = &state.objects[&id];
        assert!(
            active_static_definitions(state, obj)
                .any(|sd| sd.mode == StaticMode::Other("CantBeSacrificed".to_string())),
            "the granted prohibition must be visible on the derived statics"
        );
        assert!(
            cant_be_sacrificed(state, id),
            "the chokepoint must agree with the derived statics"
        );
    }
}

const ULAMOG_FULL_ORACLE: &str = "When you cast this spell, target opponent exiles the top half of their library, rounded up.\nWard\u{2014}Sacrifice two permanents.\nUlamog enters with a number of +1/+1 counters on it equal to the greatest mana value among cards in exile.\nUlamog has annihilator X, where X is the number of +1/+1 counters on it.";

const MURDER: &str = "Destroy target creature.";

/// HIGH-3 stage: P2 holds Ulamog (verbatim Oracle — Ward, sacrifice two
/// permanents); P1 holds the steal victim; P0 holds `rocks` legal permanents
/// plus a funded Call for Aid and a costless Murder. The aid is cast and the
/// steal reach-guards are asserted; the caller then targets Ulamog.
struct WardStage {
    runner: GameRunner,
    ulamog: ObjectId,
    bear: ObjectId,
    rocks: Vec<ObjectId>,
    murder: ObjectId,
}

fn stage_ward_aid(rocks: usize) -> WardStage {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let ulamog = scenario
        .add_creature_from_oracle(P2, "Ulamog, the Defiler", 7, 7, ULAMOG_FULL_ORACLE)
        .id();
    let bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let mut rock_ids = Vec::new();
    for i in 0..rocks {
        let name = ["P0 Rock A", "P0 Rock B"][i];
        rock_ids.push(scenario.add_creature(P0, name, 1, 1).id());
    }
    let aid = scenario
        .add_spell_to_hand_from_oracle(P0, "Call for Aid", false, CALL_FOR_AID)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Red, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 4);

    runner.cast(aid).target_player(P1).resolve();
    assert_eq!(runner.state().objects[&bear].controller, P0);
    assert!(
        cant_be_sacrificed(runner.state(), bear),
        "the stolen bear must carry the sacrifice bar"
    );
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    WardStage {
        runner,
        ulamog,
        bear,
        rocks: rock_ids,
        murder,
    }
}

/// Cast Murder at Ulamog and drive to the Ward unless-payment prompt.
fn reach_ward_payment(stage: &mut WardStage) {
    stage
        .runner
        .cast(stage.murder)
        .target_objects(&[stage.ulamog])
        .commit();
    stage.runner.advance_until_stack_empty();
    assert!(
        matches!(
            stage.runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P0, .. }
        ),
        "targeting Ulamog must prompt P0 for its Ward, got {:?}",
        stage.runner.state().waiting_for
    );
}

/// HIGH 3 — CR 118.3 + CR 702.21a: with exactly one legal permanent and one
/// Call-for-Aid-protected creature, Ulamog's "sacrifice two permanents" Ward
/// is an impossible full cost. Choosing to pay must NOT start a sacrifice
/// prompt (the old code offered the barred bear, spent the rock, then
/// stranded the payer at the Prohibited arm): the payment fails, Ward
/// counters Murder, and nothing leaves the battlefield.
#[test]
fn call_for_aid_protected_creature_makes_ward_unpayable() {
    let mut stage = stage_ward_aid(1);
    reach_ward_payment(&mut stage);

    stage
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing to pay an impossible unless-cost is accepted and fails");
    assert!(
        !matches!(
            stage.runner.state().waiting_for,
            WaitingFor::WardSacrificeChoice { .. }
        ),
        "no sacrifice prompt may start for an impossible full cost, got {:?}",
        stage.runner.state().waiting_for
    );
    stage.runner.advance_until_stack_empty();

    assert_eq!(
        stage.runner.state().objects[&stage.rocks[0]].zone,
        Zone::Battlefield,
        "the legal permanent must not be spent on an impossible cost"
    );
    assert_eq!(
        stage.runner.state().objects[&stage.bear].zone,
        Zone::Battlefield,
        "the protected bear must survive"
    );
    assert_eq!(
        stage.runner.state().objects[&stage.ulamog].zone,
        Zone::Battlefield,
        "unpaid Ward must counter Murder, leaving Ulamog alive"
    );
    assert_eq!(
        stage.runner.state().objects[&stage.murder].zone,
        Zone::Graveyard,
        "the countered Murder must be in the graveyard"
    );
    assert!(
        stage.runner.state().stack.is_empty(),
        "the stack must be empty after the Ward settles"
    );
}

/// HIGH 3 control — CR 118.3 + CR 702.21a: with two legal permanents plus the
/// protected bear, the Ward IS fully payable. The prompt must offer exactly
/// the legal permanents (the barred bear excluded), both sacrifices pay, and
/// the paid Ward lets Murder destroy Ulamog.
#[test]
fn call_for_aid_protected_creature_skipped_by_payable_ward() {
    let mut stage = stage_ward_aid(2);
    reach_ward_payment(&mut stage);

    stage
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("a fully payable Ward must offer its sacrifice prompt");
    match &stage.runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            permanents,
            remaining: 2,
            ..
        } => {
            assert_eq!(
                permanents.len(),
                2,
                "exactly the two legal permanents must be offered, got {permanents:?}"
            );
            assert!(
                permanents.contains(&stage.rocks[0]) && permanents.contains(&stage.rocks[1]),
                "both rocks must be offered, got {permanents:?}"
            );
            assert!(
                !permanents.contains(&stage.bear),
                "the protected bear must be excluded, got {permanents:?}"
            );
        }
        other => panic!("a payable Ward must prompt its sacrifice choice, got {other:?}"),
    }

    stage
        .runner
        .act(GameAction::SelectCards {
            cards: vec![stage.rocks[0]],
        })
        .expect("the first legal sacrifice pays");
    match &stage.runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            permanents,
            remaining: 1,
            ..
        } => assert_eq!(
            permanents,
            &vec![stage.rocks[1]],
            "the re-prompt must offer only the remaining legal permanent"
        ),
        other => panic!("one sacrifice must remain, got {other:?}"),
    }
    stage
        .runner
        .act(GameAction::SelectCards {
            cards: vec![stage.rocks[1]],
        })
        .expect("the second legal sacrifice pays");
    stage.runner.advance_until_stack_empty();

    for rock in &stage.rocks {
        assert_eq!(
            stage.runner.state().objects[rock].zone,
            Zone::Graveyard,
            "each paid permanent must be sacrificed"
        );
    }
    assert_eq!(
        stage.runner.state().objects[&stage.bear].zone,
        Zone::Battlefield,
        "the protected bear must survive the paid Ward"
    );
    assert!(
        cant_be_sacrificed(stage.runner.state(), stage.bear),
        "the bear must stay protected after the paid Ward"
    );
    assert_eq!(
        stage.runner.state().objects[&stage.ulamog].zone,
        Zone::Graveyard,
        "the paid Ward must let Murder destroy Ulamog"
    );
}

/// MED-1 driver — subsequent control change (mirrors the M4 companion): end
/// every theft of `object` (drop its `ChangeController` grants) and pass it
/// to `new_controller`. Separate prohibition grants stay installed. Sets
/// BOTH the live controller and `base_controller`: layers reseed control
/// from base every pass (CR 613.1b), so a live-only set is wiped by the next
/// derivation and the object silently reverts to its previous controller.
fn pass_control_to(runner: &mut GameRunner, object: ObjectId, new_controller: PlayerId) {
    let state = runner.state_mut();
    let grants = std::mem::take(&mut state.transient_continuous_effects);
    state.transient_continuous_effects = grants
        .into_iter()
        .filter(|grant| {
            !(grant
                .modifications
                .contains(&ContinuousModification::ChangeController)
                && matches!(&grant.affected, TargetFilter::SpecificObject { id } if *id == object))
        })
        .collect();
    let obj = state
        .objects
        .get_mut(&object)
        .expect("stolen object exists");
    obj.controller = new_controller;
    obj.base_controller = Some(new_controller);
}

/// Hand priority to P2 (CR 117.3c) so they may cast.
fn hand_priority_to_p2(runner: &mut GameRunner) {
    runner.state_mut().priority_player = P2;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P2 };
}

/// Drive one P0 combat in a 3-player game: the villain hits P1 once while the
/// Stilt-Man trigger steals `steal_target` (fealty-style guarded loop —
/// `run_combat` passes priority twice, so it stalls with three players).
fn run_3p_stilt_combat(runner: &mut GameRunner, villain: ObjectId, steal_target: ObjectId) {
    let mut attacked = false;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 2000,
            "never reached postcombat (phase {:?}, waiting {:?})",
            runner.state().phase,
            runner.state().waiting_for
        );
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                let attacks = if runner.state().active_player == P0 && !attacked {
                    attacked = true;
                    vec![(villain, AttackTarget::Player(P1))]
                } else {
                    vec![]
                };
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks,
                        bands: vec![],
                    })
                    .expect("declare attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare blockers");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = slot
                    .legal_targets
                    .iter()
                    .find(|t| **t == TargetRef::Object(steal_target))
                    .cloned();
                assert!(
                    choice.is_some(),
                    "the bauble must be a legal trigger target, got {:?}",
                    slot.legal_targets
                );
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("choose trigger target must be accepted");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger ordering must be accepted");
            }
            _ => {
                let _ = runner.act(GameAction::PassPriority);
            }
        }
    }
    assert!(attacked, "the villain must have attacked P1");
}

/// MED 1 (object class) — CR 109.5 + CR 701.21a: a Stilt-Man-granted "this
/// permanent can't be sacrificed" binds the OBJECT, not the installer. When
/// the stolen permanent later passes to a third player, that player is still
/// barred — the bar follows the object. (Contrast the M4 companion, where a
/// Call-for-Aid bar frees its later controller.)
#[test]
fn stilt_man_bar_follows_object_to_later_controller() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let _stiltman = scenario
        .add_creature_from_oracle(P0, "Stilt-Man, Towering Terror", 4, 2, STILT_MAN)
        .id();
    let villain = scenario.add_creature(P0, "Villain Thug", 2, 2).id();
    let bauble = scenario
        .add_creature(P1, "Bauble", 0, 0)
        .as_enchantment()
        .id();
    let fodder = scenario.add_creature(P2, "P2 Fodder", 1, 1).id();
    for _ in 0..5 {
        scenario.add_card_to_library_top(P2, "Plains");
    }
    let rites_a = scenario
        .add_spell_to_hand_from_oracle(P2, "Village Rites", true, VILLAGE_RITES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let rites_b = scenario
        .add_spell_to_hand_from_oracle(P2, "Village Rites", true, VILLAGE_RITES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let villain_obj = runner
        .state_mut()
        .objects
        .get_mut(&villain)
        .expect("villain exists");
    villain_obj.card_types.subtypes.push("Villain".to_string());
    villain_obj.base_card_types = villain_obj.card_types.clone();
    villain_obj.summoning_sick = false;

    run_3p_stilt_combat(&mut runner, villain, bauble);
    assert_eq!(
        runner.state().objects[&bauble].controller,
        P0,
        "the trigger must steal the bauble (resolution reach-guard)"
    );
    assert!(
        cant_be_sacrificed(runner.state(), bauble),
        "the stolen bauble must carry the prohibition"
    );

    pass_control_to(&mut runner, bauble, P2);
    assert_eq!(
        runner.state().objects[&bauble].controller,
        P2,
        "the control change must stick for the sacrifice below to mean anything"
    );

    // Creature surgery: Village Rites needs a CREATURE victim, but Stilt-Man
    // steals noncreatures. Stamp the bauble a 2/2 enchantment creature
    // (mirrors the Villain-subtype + P/T stamps): the Stilt grant pins the
    // object by id, so the bar survives the type change.
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&bauble)
            .expect("bauble exists");
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }
    {
        let obj = &runner.state().objects[&bauble];
        assert!(
            obj.card_types.core_types.contains(&CoreType::Creature),
            "the surgery must leave a creature for Rites to name"
        );
    }
    assert!(
        cant_be_sacrificed(runner.state(), bauble),
        "the Stilt bar must survive the type change"
    );

    // Re-fund: pools drained on the combat phase changes (mirrors V8c).
    add_mana(&mut runner, P2, ManaType::Black, 2);
    hand_priority_to_p2(&mut runner);
    let hand_before = hand_count(&runner, P2);

    // Positive reach-guard: P2's Rites works in this game — the free fodder
    // pays normally. Without this, the negative leg below could pass by
    // P2 being unable to cast at all.
    cast_sacrifice_cost_spell(&mut runner, rites_b, fodder);
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "unprohibited payment sacrifices normally"
    );
    assert_eq!(
        hand_count(&runner, P2),
        hand_before + 1,
        "spell left hand (-1), draw two (+2)"
    );

    // Negative: P2 — bound by the object's own bar — cannot fund a sacrifice
    // cost with the bauble. Announce fails (empty eligible set) or selection
    // rejects the barred victim (mirrors the V5a negative).
    hand_priority_to_p2(&mut runner);
    // Re-fund leg 2 separately: leg 1's stack drain may advance phases and
    // empty the pool, which would fail the announce for lack of mana rather
    // than for the bar.
    add_mana(&mut runner, P2, ManaType::Black, 1);
    let card_id = runner.state().objects[&rites_a].card_id;
    let announce = runner.act(GameAction::CastSpell {
        object_id: rites_a,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    if announce.is_ok() {
        let selection = runner.act(GameAction::SelectCards {
            cards: vec![bauble],
        });
        assert!(
            selection.is_err(),
            "the object-bound victim must be rejected at selection"
        );
    }
    assert_eq!(
        runner.state().objects[&bauble].zone,
        Zone::Battlefield,
        "the barred victim must stay on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&rites_a].zone,
        Zone::Hand,
        "the unpaid spell stays in hand"
    );
    assert_eq!(
        hand_count(&runner, P2),
        hand_before + 1,
        "no further draw: the second spell never resolved"
    );
}

const SELF_BARRED_BEAR: &str = "P1 Bear can't be sacrificed.";

/// MED 1 (concurrency) — CR 109.5 + CR 701.21a: a printed object bar and a
/// Call-for-Aid installer bar coexist on one object. When the doubly-barred
/// creature later passes to a third player, the installer's bar frees them
/// but the printed bar still binds — the exemption applies graft-by-graft.
#[test]
fn printed_bar_survives_aid_transfer_to_later_controller() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario
        .add_creature_from_oracle(P1, "P1 Bear", 2, 2, SELF_BARRED_BEAR)
        .id();
    let fodder = scenario.add_creature(P2, "P2 Fodder", 1, 1).id();
    for _ in 0..5 {
        scenario.add_card_to_library_top(P2, "Plains");
    }
    let aid = scenario
        .add_spell_to_hand_from_oracle(P0, "Call for Aid", false, CALL_FOR_AID)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        .id();
    let rites = scenario
        .add_spell_to_hand_from_oracle(P2, "Village Rites", true, VILLAGE_RITES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Red, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 4);
    add_mana(&mut runner, P2, ManaType::Black, 1);

    assert!(
        cant_be_sacrificed(runner.state(), bear),
        "reach-guard: the printed bar functions before the aid"
    );
    runner.cast(aid).target_player(P1).resolve();
    assert_eq!(
        runner.state().objects[&bear].controller,
        P0,
        "the aid must steal the bear"
    );
    pass_control_to(&mut runner, bear, P2);
    assert_eq!(
        runner.state().objects[&bear].controller,
        P2,
        "the control change must stick for the sacrifice below to mean anything"
    );

    // P2 is freed by the installer's bar but still bound by the printed one.
    // The free fodder keeps a legal victim available, so the announce
    // succeeds (proving P2 can cast) and the barred bear is rejected
    // deterministically at selection.
    hand_priority_to_p2(&mut runner);
    let hand_before = hand_count(&runner, P2);
    let card_id = runner.state().objects[&rites].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: rites,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce succeeds while the free fodder can pay");
    let selection = runner.act(GameAction::SelectCards { cards: vec![bear] });
    assert!(
        selection.is_err(),
        "the printed-barred victim must be rejected at selection"
    );
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Battlefield,
        "the rejected payment sacrifices nothing"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "the printed-barred victim must stay on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&rites].zone,
        Zone::Hand,
        "the unpaid spell stays in hand"
    );
    assert_eq!(
        hand_count(&runner, P2),
        hand_before,
        "no draw: the spell never resolved"
    );
}

/// MED 1 (multi-installer) — CR 109.5: two concurrent Call-for-Aid bars from
/// different installers bind per-TCE. A later third controller is bound by
/// neither and sacrifices freely.
#[test]
fn concurrent_aid_bars_free_later_controller() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    for _ in 0..5 {
        scenario.add_card_to_library_top(P2, "Plains");
    }
    let aid_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Call for Aid", false, CALL_FOR_AID)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        .id();
    let aid_two = scenario
        .add_spell_to_hand_from_oracle(P1, "Call for Aid", false, CALL_FOR_AID)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        .id();
    let rites = scenario
        .add_spell_to_hand_from_oracle(P2, "Village Rites", true, VILLAGE_RITES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Red, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 4);
    add_mana(&mut runner, P1, ManaType::Red, 1);
    add_mana(&mut runner, P1, ManaType::Colorless, 4);
    add_mana(&mut runner, P2, ManaType::Black, 1);

    runner.cast(aid_one).target_player(P1).resolve();
    assert_eq!(
        runner.state().objects[&bear].controller,
        P0,
        "the first aid must steal the bear"
    );
    // P1's turn for the second sorcery (no cleanup runs, so both bars stay live).
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.cast(aid_two).target_player(P0).resolve();
    assert_eq!(
        runner.state().objects[&bear].controller,
        P1,
        "the second aid must steal the bear back"
    );
    let live_bars = runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|tce| {
            matches!(&tce.affected, TargetFilter::SpecificObject { id } if *id == bear)
                && tce.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::Other(name),
                        } if name == "CantBeSacrificed"
                    )
                })
        })
        .count();
    assert_eq!(
        live_bars, 2,
        "both installers' bars must be live on the bear"
    );

    pass_control_to(&mut runner, bear, P2);
    hand_priority_to_p2(&mut runner);
    let hand_before = hand_count(&runner, P2);
    cast_sacrifice_cost_spell(&mut runner, rites, bear);
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "the later controller sacrifices freely under concurrent installer bars"
    );
    assert_eq!(
        hand_count(&runner, P2),
        hand_before + 1,
        "spell left hand (-1), draw two (+2)"
    );
}

/// MED 2 — CR 608.2d + CR 609.3 + the Call for Aid ruling: with mixed
/// candidates (stolen/barred + free), a Diabolic Edict offers ONLY the free
/// creature; naming the protected victim is rejected at submission, and the
/// free one is sacrificed. Answering a mandatory sacrifice by naming a barred
/// victim and evading is gone.
#[test]
fn diabolic_edict_excludes_aid_protected_victim() {
    let mut stage = stage_2p_sac();
    cast_aid_and_check_theft(&mut stage);
    // Two free creatures so the edict raises a real prompt (a singleton
    // pool takes the no-prompt fast path) whose offered set is observable.
    let fodder_a = create_battlefield_creature(&mut stage.runner, P0, "Edict Fodder A");
    let fodder_b = create_battlefield_creature(&mut stage.runner, P0, "Edict Fodder B");
    hand_priority_to_p1(&mut stage.runner);

    stage.runner.cast(stage.edict).target_player(P0).commit();
    stage.runner.advance_until_stack_empty();
    match &stage.runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            cards, player: P0, ..
        } => {
            assert_eq!(
                cards.len(),
                2,
                "only the two free creatures may be offered, got {cards:?}"
            );
            assert!(
                cards.contains(&fodder_a) && cards.contains(&fodder_b),
                "both free creatures must be offered, got {cards:?}"
            );
            assert!(
                !cards.contains(&stage.p1_a) && !cards.contains(&stage.p1_b),
                "neither protected victim may be offered, got {cards:?}"
            );
        }
        other => panic!("the edict must prompt its sacrifice choice, got {other:?}"),
    }
    // The barred victim is not among the offered cards, so naming it is
    // rejected at submission (membership and the bar revalidation agree),
    // and the prompt persists for a legal pick.
    assert!(
        stage
            .runner
            .act(GameAction::SelectCards {
                cards: vec![stage.p1_a],
            })
            .is_err(),
        "naming the protected victim must be rejected at submission"
    );
    stage
        .runner
        .act(GameAction::SelectCards {
            cards: vec![fodder_a],
        })
        .expect("the free victim must be accepted");
    stage.runner.advance_until_stack_empty();

    assert_eq!(
        stage.runner.state().objects[&fodder_a].zone,
        Zone::Graveyard,
        "the chosen free creature is sacrificed"
    );
    assert_eq!(
        stage.runner.state().objects[&fodder_b].zone,
        Zone::Battlefield,
        "the unchosen free creature survives"
    );
    assert_eq!(
        stage.runner.state().objects[&stage.p1_a].zone,
        Zone::Battlefield,
        "the protected victim survives"
    );
    assert_eq!(
        stage.runner.state().objects[&stage.p1_b].zone,
        Zone::Battlefield,
        "the other protected victim survives"
    );
    assert!(
        cant_be_sacrificed(stage.runner.state(), stage.p1_a)
            && cant_be_sacrificed(stage.runner.state(), stage.p1_b),
        "the prohibition survives the edict"
    );
}
