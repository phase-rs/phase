//! R4 advanced Horde mill — the deployment completion through PUBLIC damage
//! resolution (a real Lightning Bolt cast by a survivor, resolved through the
//! runner; no `process_triggers` calls, no seam-level drivers).
//!
//! Three defects, one design (per the review: the phase-out work must survive
//! both trigger collection and a batch pause):
//!
//! 1. The legendary phase-out must run SYNCHRONOUSLY after the batch's own
//!    trigger collection (printed ETBs still fire) and before the next SBA
//!    check — no synthetic graft, no legend-rule exemption. Two copies of one
//!    boss both phase out; their ETBs still resolve.
//! 2. The deployment completion must ride the parked batch tail, so a CR 616.1
//!    pause on an EARLIER card does not strand a LATER legendary phased in.
//! 3. The advanced delivery must publish `Milled` events exactly like ordinary
//!    mill, so milled triggers (Zellix) fire.

use engine::game::combat::AttackTarget;
use engine::game::game_object::PhaseStatus;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::format::{ChallengeDeck, FormatConfig, HordeLegendaryDeath};
use engine::types::game_state::{ActionResult, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const HORDE: PlayerId = PlayerId(0);
const SURVIVOR: PlayerId = PlayerId(1);

/// A 2-player Horde game under the opt-in ADVANCED legendary rule (milled
/// legendary permanents deploy, ETB triggers fire, then immediately phase
/// out), ready for step-by-step play from P0's pre-combat main.
fn advanced_horde_scenario() -> GameScenario {
    let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
    ruleset.legendary_death = HordeLegendaryDeath::EtbThenPhaseOut;
    let mut scenario = GameScenario::new_with_format(FormatConfig::horde(ruleset), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// Give `player` one red mana (Lightning Bolt's cost), mirroring the
/// Mothman milled-trigger tests' direct pool grant.
fn add_red_mana(runner: &mut GameRunner) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == SURVIVOR)
        .unwrap()
        .mana_pool;
    pool.add(ManaUnit::new(ManaType::Red, dummy, false, vec![]));
}

/// Park the game at SURVIVOR's priority on SURVIVOR's pre-combat main (a
/// mid-game turn, mirroring `horde_spine_runtime`'s proven parking): the
/// survivor can cast at instant speed immediately, and a survivor's main
/// fires no Horde wave.
fn park_survivor_priority(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 4;
    state.active_player = SURVIVOR;
    state.phase = Phase::PreCombatMain;
    state.priority_player = SURVIVOR;
    state.waiting_for = WaitingFor::Priority { player: SURVIVOR };
}

/// Cast SURVIVOR's Lightning Bolt at the Horde through the full pipeline and
/// resolve it. The game must already be parked at the survivor's priority.
fn bolt_the_horde(runner: &mut GameRunner, bolt: ObjectId) {
    runner.cast(bolt).target_players(&[HORDE]).resolve();
}

/// Manual Bolt cast that STOPS at the first prompt instead of resolving
/// through it: returns once the spell is committed and any mid-resolution
/// prompt (a CR 616.1 ordering choice) is parked.
fn cast_bolt_stop_at_prompt(runner: &mut GameRunner, bolt: ObjectId) -> ActionResult {
    let card_id = runner.state().objects[&bolt].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Bolt cast should be accepted");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        result = runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(HORDE)),
            })
            .expect("Bolt should accept the Horde player target");
    }
    // The committed Bolt sits on the stack at the caster's priority: both
    // players pass (bounded) until resolution parks the mid-batch prompt.
    let mut guard = 0;
    while matches!(result.waiting_for, WaitingFor::Priority { .. }) {
        guard += 1;
        assert!(
            guard < 8,
            "Bolt never left the stack; last waiting_for = {:?}",
            result.waiting_for
        );
        result = runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance the stack");
    }
    result
}

/// Activate SURVIVOR's Tim targeting the Horde through the full pipeline,
/// stopping once the mid-resolution prompt parks (the activation does NOT
/// resolve through it). The game must already be parked at the survivor's
/// priority.
fn activate_tim_stop_at_prompt(runner: &mut GameRunner, tim: ObjectId) -> ActionResult {
    let mut result = runner
        .act(GameAction::ActivateAbility {
            source_id: tim,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Tim activation should be accepted");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        result = runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(HORDE)),
            })
            .expect("Tim should accept the Horde player target");
    }
    // The committed activation sits on the stack at the activator's priority:
    // both players pass (bounded) until resolution parks the mid-batch prompt.
    let mut guard = 0;
    while matches!(result.waiting_for, WaitingFor::Priority { .. }) {
        guard += 1;
        assert!(
            guard < 8,
            "Tim activation never left the stack; last waiting_for = {:?}",
            result.waiting_for
        );
        result = runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance the stack");
    }
    result
}

/// Count battlefield objects named `name` controlled by `controller`.
fn count_battlefield_named(runner: &GameRunner, name: &str, controller: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.name == name && o.controller == controller)
        })
        .count()
}

/// R4-#1 — duplicate deployed legends phase out synchronously AFTER their
/// printed ETBs are collected: two milled Breyas both leave phased-in play,
/// no `ChooseLegend` pause ever surfaces (CR 704.5j sees nothing to choose
/// between), and both Breya ETBs still resolve into four Thopters.
///
/// Revert guard: the graft design never fires in production (entry events are
/// consumed before the graft lands), so both Breyas stay phased in — the
/// phase-out assertions flip. Suppressing collection instead (phasing out
/// before the scan) keeps the phase-out green but starves the Thopters.
#[test]
fn advanced_deployment_duplicate_legends_phase_out_after_etbs_fire() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = advanced_horde_scenario();
    // Library top first: `add_to_zone` pushes to the back and the mill takes
    // from the front, so Bolt's mill-3 takes Breya, Breya, Bolt.
    let breya1 = scenario.add_real_card(HORDE, "Breya, Etherium Shaper", Zone::Library, db);
    let breya2 = scenario.add_real_card(HORDE, "Breya, Etherium Shaper", Zone::Library, db);
    for _ in 0..9 {
        scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    }
    let bolt = scenario.add_real_card(SURVIVOR, "Lightning Bolt", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_red_mana(&mut runner);
    park_survivor_priority(&mut runner);

    // PRECONDITIONS: the discriminator's premise — both milled cards are
    // legendary permanents carrying a battlefield-entry trigger.
    for (i, breya) in [breya1, breya2].iter().enumerate() {
        let obj = runner
            .state()
            .objects
            .get(breya)
            .expect("Breya fixture object exists");
        assert!(
            obj.card_types.supertypes.contains(&Supertype::Legendary),
            "Breya {i} must be legendary, or the advanced rule never applies"
        );
        assert!(
            !obj.trigger_definitions.is_empty(),
            "Breya {i} must carry its printed ETB trigger, or the Thopter \
             assertion passes for the wrong reason"
        );
    }

    bolt_the_horde(&mut runner, bolt);
    runner.advance_until_stack_empty();

    // DISCRIMINATORS: phased out, never legend-ruled, ETBs resolved.
    for (i, breya) in [breya1, breya2].iter().enumerate() {
        let obj = runner
            .state()
            .objects
            .get(breya)
            .expect("deployed Breya still exists");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "Breya {i} must still be on the battlefield (phased out, not gone)"
        );
        assert!(
            matches!(obj.phase_status, PhaseStatus::PhasedOut { .. }),
            "Breya {i} must be phased out after deployment (stays phased in \
             when the phase-out never fires in production)"
        );
    }
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::ChooseLegend { .. }),
        "no legend-rule choice may surface: both copies phase out before the \
         next SBA check"
    );
    assert_eq!(
        count_battlefield_named(&runner, "Thopter", HORDE),
        4,
        "both printed Breya ETBs must resolve into four Thopter tokens"
    );
}

/// R4-#2 — the deployment completion rides the parked batch tail: with Rest
/// in Peace + Leyline of the Void redirecting the FIRST milled card's
/// graveyard move (a CR 616.1 ordering pause), Uglúk entering from the RESUMED
/// tail still phases out.
///
/// Revert guard: the pause returns after grafting only what already entered,
/// and the generic drain never grafts — Uglúk stays phased in and the
/// phase-out assertion flips.
#[test]
fn advanced_deployment_completion_survives_batch_pause() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = advanced_horde_scenario();
    scenario.add_real_card(SURVIVOR, "Rest in Peace", Zone::Battlefield, db);
    scenario.add_real_card(SURVIVOR, "Leyline of the Void", Zone::Battlefield, db);
    // Library top first: the Bolt's graveyard move collides both redirects
    // (the pause), then Uglúk deploys from the resumed tail.
    let paused_bolt = scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    let ugluk = scenario.add_real_card(HORDE, "Uglúk of the White Hand", Zone::Library, db);
    for _ in 0..9 {
        scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    }
    let bolt = scenario.add_real_card(SURVIVOR, "Lightning Bolt", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_red_mana(&mut runner);
    park_survivor_priority(&mut runner);

    // PRECONDITION: Uglúk is a legendary permanent — the advanced rule's
    // subject — or the phase-out assertion is vacuous.
    let ugluk_obj = runner
        .state()
        .objects
        .get(&ugluk)
        .expect("Uglúk fixture object exists");
    assert!(
        ugluk_obj
            .card_types
            .supertypes
            .contains(&Supertype::Legendary),
        "Uglúk must be legendary, or the advanced rule never applies"
    );

    let result = cast_bolt_stop_at_prompt(&mut runner, bolt);

    // The pause must park (mill-3 takes Bolt, Uglúk, Bolt; the first Bolt's
    // graveyard move collides the two exile redirects).
    assert!(
        matches!(result.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the ordering choice must park mid-batch, got {:?}",
        result.waiting_for
    );
    assert!(
        runner.state().active_batch_delivery().is_some(),
        "the undelivered tail must be stashed for the resume path"
    );

    // Answer every prompt (bounded: a resume loop fails loudly, never hangs).
    // Both redirects exile, so either ordering lands the Bolts in exile.
    for _ in 0..10 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("answer the CR 616.1 ordering prompt");
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "all replacement prompts must be answerable"
    );
    runner.advance_until_stack_empty();

    // DISCRIMINATORS: the resumed-tail legendary phased out; both milled
    // Bolts exiled (never graveyard, never stranded in the library).
    let ugluk_obj = runner
        .state()
        .objects
        .get(&ugluk)
        .expect("deployed Uglúk still exists");
    assert_eq!(
        ugluk_obj.zone,
        Zone::Battlefield,
        "Uglúk must deploy from the resumed tail"
    );
    assert!(
        matches!(ugluk_obj.phase_status, PhaseStatus::PhasedOut { .. }),
        "Uglúk must phase out even though it entered after the pause"
    );
    for (i, id) in [paused_bolt].iter().enumerate() {
        let obj = runner
            .state()
            .objects
            .get(id)
            .expect("milled Bolt still exists");
        assert_eq!(
            obj.zone,
            Zone::Exile,
            "milled Bolt {i} must be exiled by the answered redirect"
        );
    }
    assert!(
        runner.state().players[HORDE.0 as usize]
            .library
            .iter()
            .all(|id| *id != paused_bolt && *id != ugluk),
        "no milled card may strand in the library across the pause"
    );
}

/// R4-#4 — the advanced delivery publishes `Milled` events exactly like
/// ordinary mill: Zellix's "whenever a player mills one or more creature
/// cards" trigger fires off a Horde damage-mill and creates its Horror.
///
/// Revert guard: without the mill completion the batch emits no `Milled`
/// events, Zellix never triggers, and the Horror count stays 0.
#[test]
fn advanced_mill_publishes_milled_events_zellix() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = advanced_horde_scenario();
    scenario.add_real_card(SURVIVOR, "Zellix, Sanity Flayer", Zone::Battlefield, db);
    // Library top first: three creatures (Zellix's filter), then padding.
    for _ in 0..3 {
        scenario.add_real_card(HORDE, "Grizzly Bears", Zone::Library, db);
    }
    for _ in 0..9 {
        scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    }
    let bolt = scenario.add_real_card(SURVIVOR, "Lightning Bolt", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_red_mana(&mut runner);
    park_survivor_priority(&mut runner);

    bolt_the_horde(&mut runner, bolt);
    runner.advance_until_stack_empty();

    // DISCRIMINATORS: the mill landed (3 Bears in the Horde graveyard) AND
    // its events fired Zellix exactly once (batched "one or more" → 1 Horror).
    assert_eq!(
        runner.state().players[HORDE.0 as usize].graveyard.len(),
        3,
        "Bolt's mill-3 must land three Bears in the Horde's graveyard"
    );
    assert_eq!(
        count_battlefield_named(&runner, "Horror", SURVIVOR),
        1,
        "Zellix must see the advanced mill's Milled events and create one Horror"
    );
}

/// R4-#3 — a paused damage-to-mill preserves the damage's post-loss work:
/// Tim (granted lifelink by Unflinching Courage) pings the Horde for 1 while
/// Rest in Peace + Leyline of the Void pause the redirected mill. After every
/// prompt is answered, the survivor still gains the lifelink life (CR 120.3f)
/// and the damage is recorded (CR 120.1) — each exactly once.
///
/// Lifelink-source note: the review names Soulfire Grand Master, but its
/// "spells you control have lifelink" static is unparsed (no `static_abilities`
/// entry — only self-Lifelink plus an Unimplemented activated ability), so a
/// Soulfire test would fail with no lifelink granted at all, pause or no
/// pause. Tim + Courage are both fully parsed, keeping the pause as the only
/// variable.
///
/// Revert guard: without the post-loss continuation, the early `NeedsChoice`
/// return drops recording + lifelink — the life and record assertions flip.
/// A double-resume gains 2 and double-records — the exactness assertions flip.
#[test]
fn paused_damage_to_mill_preserves_lifelink_and_recording() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = advanced_horde_scenario();
    scenario.with_life(SURVIVOR, 20);
    scenario.add_real_card(SURVIVOR, "Rest in Peace", Zone::Battlefield, db);
    scenario.add_real_card(SURVIVOR, "Leyline of the Void", Zone::Battlefield, db);
    let tim = scenario.add_real_card(SURVIVOR, "Prodigal Sorcerer", Zone::Battlefield, db);
    let courage = scenario.add_real_card(SURVIVOR, "Unflinching Courage", Zone::Hand, db);
    // Mill-1 target: one nonlegendary on top (its graveyard move collides the
    // two exile redirects), padding behind so the library never empties.
    let milled = scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    for _ in 0..9 {
        scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    // Courage costs {1}{G}{W}: grant it directly (no lands in this scenario).
    {
        let dummy = ObjectId(0);
        let pool = &mut runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == SURVIVOR)
            .unwrap()
            .mana_pool;
        for mana in [ManaType::Green, ManaType::White, ManaType::Colorless] {
            pool.add(ManaUnit::new(mana, dummy, false, vec![]));
        }
    }
    park_survivor_priority(&mut runner);

    // Aura up: Courage resolves onto Tim first (no pause — nothing mills).
    runner.cast(courage).target_objects(&[tim]).resolve();
    runner.advance_until_stack_empty();

    // PRECONDITIONS: the Aura attached and grants lifelink (after a layers
    // flush), and Tim's ping ability is live — or the life assertion below
    // would fail for fixture reasons rather than the pause defect.
    flush_layers(runner.state_mut());
    let tim_obj = runner
        .state()
        .objects
        .get(&tim)
        .expect("Tim still on the battlefield");
    assert!(
        tim_obj.keywords.contains(&Keyword::Lifelink),
        "Courage must grant Tim lifelink, or the life assertion is vacuous"
    );
    assert!(
        !tim_obj.abilities.is_empty(),
        "Tim must carry his ping ability"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == SURVIVOR)
            .unwrap()
            .life,
        20,
        "casting the Aura must not change the survivor's life"
    );

    let result = activate_tim_stop_at_prompt(&mut runner, tim);
    assert!(
        matches!(result.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the ordering choice must park mid-mill, got {:?}",
        result.waiting_for
    );

    // Answer every prompt (bounded); both redirects exile either way.
    for _ in 0..10 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("answer the CR 616.1 ordering prompt");
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "all replacement prompts must be answerable"
    );
    runner.advance_until_stack_empty();

    // DISCRIMINATORS: the mill landed (exiled by the answered redirects) AND
    // the damage's post-loss work survived the pause — the lifelink point and
    // the CR 120.1 record, each exactly once.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&milled)
            .expect("milled Bolt still exists")
            .zone,
        Zone::Exile,
        "the milled Bolt must be exiled by the answered redirect"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == SURVIVOR)
            .unwrap()
            .life,
        21,
        "Tim's 1 lifelink damage must gain exactly 1 life even though the \
         redirected mill paused"
    );
    let records: Vec<_> = runner
        .state()
        .damage_dealt_this_turn
        .iter()
        .filter(|r| r.source_id == tim)
        .collect();
    assert_eq!(
        records.len(),
        1,
        "Tim's damage must be recorded exactly once, got {:?}",
        runner.state().damage_dealt_this_turn
    );
    assert_eq!(
        records[0].target,
        TargetRef::Player(HORDE),
        "the record must name the Horde as the damage recipient"
    );
    assert_eq!(
        records[0].amount, 1,
        "the record must carry the 1 damage dealt"
    );
}

/// R6-#3 — the SECOND pause must not re-propose the lifelink gain: the R4-#3
/// board plus Boon Reflection and Rhox Faithmender on the survivor's side.
/// Tim's ping pauses twice — once for the mill ordering (RiP vs Leyline),
/// once for the gain ordering (Boon vs Rhox). Answering the gain prompt
/// settles the ONE gain (with the chosen replacement folded once); no third
/// prompt may appear and the survivor's life is the exact total.
///
/// Gain-source note: the review names Angel of Vitality, but the curated
/// integration fixture has no Angel entry — only Boon, Rhox, and
/// Alhammarret's Archive carry parsed GainLife replacements there. Boon +
/// Rhox are the same competing-replacement class (two mandatory doublers),
/// so the pause is identical; 1 gains exactly 1*2*2 = 4 either order.
///
/// Revert guard: without the stage handoff, the post-answer funnel drain
/// re-proposes the gain after every answer — a third ordering prompt parks
/// and each further answer gains again.
#[test]
fn paused_damage_second_pause_gains_lifelink_once() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = advanced_horde_scenario();
    scenario.with_life(SURVIVOR, 20);
    scenario.add_real_card(SURVIVOR, "Rest in Peace", Zone::Battlefield, db);
    scenario.add_real_card(SURVIVOR, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(SURVIVOR, "Boon Reflection", Zone::Battlefield, db);
    scenario.add_real_card(SURVIVOR, "Rhox Faithmender", Zone::Battlefield, db);
    let tim = scenario.add_real_card(SURVIVOR, "Prodigal Sorcerer", Zone::Battlefield, db);
    let courage = scenario.add_real_card(SURVIVOR, "Unflinching Courage", Zone::Hand, db);
    let milled = scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    for _ in 0..9 {
        scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    {
        let dummy = ObjectId(0);
        let pool = &mut runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == SURVIVOR)
            .unwrap()
            .mana_pool;
        for mana in [ManaType::Green, ManaType::White, ManaType::Colorless] {
            pool.add(ManaUnit::new(mana, dummy, false, vec![]));
        }
    }
    park_survivor_priority(&mut runner);

    runner.cast(courage).target_objects(&[tim]).resolve();
    runner.advance_until_stack_empty();

    flush_layers(runner.state_mut());
    assert!(
        runner
            .state()
            .objects
            .get(&tim)
            .expect("Tim still on the battlefield")
            .keywords
            .contains(&Keyword::Lifelink),
        "Courage must grant Tim lifelink, or the life assertion is vacuous"
    );

    // PAUSE 1: the redirected mill parks on the RiP/Leyline ordering.
    let result = activate_tim_stop_at_prompt(&mut runner, tim);
    assert!(
        matches!(result.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the ordering choice must park mid-mill, got {:?}",
        result.waiting_for
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer the mill ordering prompt");

    // PAUSE 2: the lifelink gain parks on the Boon/Rhox ordering.
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "the lifelink gain must park on its ordering prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "Boon and Rhox must both contest the gain"
    );
    let names: Vec<&str> = candidates.iter().map(|c| c.source_name.as_str()).collect();
    assert!(
        names.contains(&"Boon Reflection") && names.contains(&"Rhox Faithmender"),
        "the gain prompt must name both replacements, got {names:?}"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer the gain ordering prompt");

    // NO PAUSE 3: the answered gain is settled, not re-proposed.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the answered gain must settle with no further prompt, got {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner
            .state()
            .objects
            .get(&milled)
            .expect("milled Bolt still exists")
            .zone,
        Zone::Exile,
        "the milled Bolt must be exiled by the answered redirect"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == SURVIVOR)
            .unwrap()
            .life,
        24,
        "Tim's 1 lifelink damage must gain exactly 1*2*2 = 4 through both pauses"
    );
    assert_eq!(
        runner
            .state()
            .damage_dealt_this_turn
            .iter()
            .filter(|r| r.source_id == tim)
            .count(),
        1,
        "Tim's damage must be recorded exactly once"
    );
}

/// R6-#4 — combat damage across a mill pause: an unblocked Rhox Faithmender
/// (1/5) hits the Horde while Rest in Peace + Leyline of the Void pause the
/// redirected mill-1. After the prompt is answered the combat leg completes:
/// the milled card exiles, Rhox's controller gains the doubled lifelink
/// (1 damage, doubled once by Rhox itself to 2 — a lone replacement, so the
/// gain never pauses), and the combat damage is recorded — each exactly once.
///
/// Revert guard: without the batch remnant, Phase C turns the paused leg's
/// `NeedsChoice` into 0 — no lifelink gain, no damage record.
#[test]
fn combat_damage_to_mill_preserves_lifelink_and_recording() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = advanced_horde_scenario();
    scenario.with_life(SURVIVOR, 20);
    scenario.add_real_card(SURVIVOR, "Rest in Peace", Zone::Battlefield, db);
    scenario.add_real_card(SURVIVOR, "Leyline of the Void", Zone::Battlefield, db);
    let rhox = scenario.add_real_card(SURVIVOR, "Rhox Faithmender", Zone::Battlefield, db);
    let milled = scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    for _ in 0..9 {
        scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    park_survivor_priority(&mut runner);

    // PREMISES: Rhox swings for exactly 1 with lifelink — or the life and
    // record assertions below fail for fixture reasons, not the pause defect.
    flush_layers(runner.state_mut());
    let rhox_obj = runner
        .state()
        .objects
        .get(&rhox)
        .expect("Rhox still on the battlefield");
    assert_eq!(rhox_obj.power, Some(1), "Rhox must hit for exactly 1");
    assert!(
        rhox_obj.keywords.contains(&Keyword::Lifelink),
        "Rhox must have lifelink"
    );

    // Walk the survivor's turn into combat and send Rhox at the Horde.
    runner.pass_both_players();
    runner
        .declare_attackers(&[(rhox, AttackTarget::Player(HORDE))])
        .expect("Rhox must be able to attack the Horde");
    // Drive through combat damage to post-combat, leaving Rhox unblocked and
    // answering each mill-ordering prompt (both redirects exile either way).
    let mut mill_prompts = 0;
    for _ in 0..40 {
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("declaring no blockers should succeed");
            }
            WaitingFor::ReplacementChoice { .. } => {
                mill_prompts += 1;
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("answer the CR 616.1 ordering prompt");
            }
            other => panic!("unexpected window during combat: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().phase,
        Phase::PostCombatMain,
        "combat must complete, stuck at {:?} with {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
    assert_eq!(
        mill_prompts, 1,
        "the redirected mill-1 must pause on its ordering — zero means the \
         board stopped pausing and the test is vacuous"
    );

    // DISCRIMINATORS: the mill landed (the Bolt exiled) AND the combat
    // leg's post-loss work survived the pause — the doubled lifelink and the
    // CR 120.1 record, each exactly once.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&milled)
            .expect("milled Bolt still exists")
            .zone,
        Zone::Exile,
        "the milled Bolt must be exiled by the answered redirect"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == SURVIVOR)
            .unwrap()
            .life,
        22,
        "Rhox's 1 combat damage must gain exactly 1*2 = 2 even though the \
         redirected mill paused"
    );
    let records: Vec<_> = runner
        .state()
        .damage_dealt_this_turn
        .iter()
        .filter(|r| r.source_id == rhox)
        .collect();
    assert_eq!(
        records.len(),
        1,
        "Rhox's damage must be recorded exactly once, got {:?}",
        runner.state().damage_dealt_this_turn
    );
    assert_eq!(
        records[0].target,
        TargetRef::Player(HORDE),
        "the record must name the Horde as the damage recipient"
    );
    assert_eq!(
        records[0].amount, 1,
        "the record must carry the 1 damage dealt"
    );
}
