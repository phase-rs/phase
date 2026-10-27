//! CR 608.2n + CR 702.88a: the final part of an instant or sorcery's resolution (its
//! on-resolution hooks, its destination and the move off the stack) is owed on the resolution
//! carrier (`GameState::resolving_spell_final_part`) while the spell's instructions are paused
//! for a choice. This file pins two things around that marker:
//!
//! 1. THE LEGACY SAVE (`a_legacy_*`, `scrubbing_*`). A build before the marker recorded a spell
//!    paused on its own free-cast window as `GameState::deferred_spell_delivery`
//!    (`{ object_id, destination }`) AFTER it had already armed the spell's on-resolution hooks
//!    (rebound's delayed trigger, paradigm, epic). The marker means the opposite: the hooks have
//!    NOT yet run. No mapping from the record to the marker can say which hooks ran, so
//!    completing such a snapshot installs a second rebound trigger and loses the chosen
//!    destination (CR 702.88a), and dropping the record strands the spell in `Zone::Stack`
//!    outside `state.stack`. `GameStateDecode` therefore refuses a snapshot that carries the
//!    key, on every route (persisted raw, trusted envelope, bare `GameState`, resolution wire
//!    v2-v4).
//!
//!    The three fixtures under `fixtures/legacy_deferred_spell_delivery/` are REAL old-format
//!    snapshots, taken with `PersistedGameState::Raw` / `serde_json::to_value(&state)` at commit
//!    9e4e73da6 (the last tree that wrote the record) from boards built through the cast
//!    pipeline and stopped on the spell's own free-cast window, `resolution_state_version` 5:
//!    - `taigam_collected_conjuring_{raw,plain}`: Taigam, Ojutai Master attacked, Collected
//!      Conjuring cast from hand in the second main phase (it gained rebound; the delayed
//!      trigger is ALREADY installed), record `{ object_id: 2, destination: "Exile" }`.
//!    - `finale_of_promise_raw`: Finale of Promise (X = 3, an instant and a sorcery in the
//!      graveyard), record `{ object_id: 1, destination: "Graveyard" }`, no delayed trigger.
//!
//!    Regeneration needs a build of 9e4e73da6 and the files must be re-gzipped (`gzip -9 -n`).
//!
//! 2. THE LAYER SEAM (`taigam_rebound_survives_*`, `a_current_format_*`). The final part reads
//!    the spell's keywords when it runs, after the pause. Taigam's rebound is a transient
//!    keyword grant on the spell, so every layer pass between the cast and the final part must
//!    re-apply it to the popped resolving spell. It must survive a nested cast in the window, a
//!    save restored at a choice prompt, the answer to a prompt whose own state change runs a
//!    layer pass, and a layer pass before the prompt opens.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastOfferKind, CastPaymentMode, GameState, OwedSpellFinalPart, PersistedGameState, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::resolution::ResolutionStateWire;
use engine::types::zones::Zone;
use serde_json::Value;

const TAIGAM: &str = "Instant, sorcery, and Dragon spells you control can't be countered.\nWhenever you cast an instant or sorcery spell from your hand, if Taigam attacked this turn, that spell gains rebound. (Exile the spell as it resolves. At the beginning of your next upkeep, you may cast that card from exile without paying its mana cost.)";
const COLLECTED_CONJURING: &str = "Exile the top six cards of your library. You may cast up to two sorcery spells with mana value 3 or less from among them without paying their mana costs. Put the exiled cards not cast this way on the bottom of your library in a random order.";
const FINALE_OF_PROMISE: &str = "You may cast up to one target instant card and/or up to one target sorcery card from your graveyard each with mana value X or less without paying their mana costs. If a spell cast this way would be put into your graveyard, exile it instead. If X is 10 or more, copy each of those spells twice. You may choose new targets for the copies.";
const INVOKE_CALAMITY: &str = "You may cast up to two instant and/or sorcery spells with total mana value 6 or less from your graveyard and/or hand without paying their mana costs. If those spells would be put into your graveyard, exile them instead. Exile Invoke Calamity.";
const CAREFUL_STUDY: &str = "Draw two cards, then discard two cards.";
// The reminder text is part of the printed Oracle text; the parser strips it.
const PREORDAIN: &str = "Scry 2, then draw a card. (To scry 2, look at the top two cards of your library, then put any number of them on the bottom and the rest on top in any order.)";
const IMPULSE: &str = "Look at the top four cards of your library. Put one of them into your hand and the rest on the bottom of your library in any order.";

/// Stable prefix of `LEGACY_DEFERRED_SPELL_DELIVERY_ERROR`.
const LEGACY_ERROR: &str = "legacy deferred_spell_delivery snapshot cannot prove";

// Fixtures are stored gzip-compressed; inflate at first use.
fn gunzip_fixture(gz: &[u8]) -> Value {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    let mut fixture: Value = serde_json::from_str(&json).expect("fixture is JSON");
    // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
    engine::types::game_state::record_captured_onsets(&mut fixture);
    fixture
}

fn taigam_raw() -> Value {
    gunzip_fixture(include_bytes!(
        "../fixtures/legacy_deferred_spell_delivery/taigam_collected_conjuring_raw.json.gz"
    ))
}

fn taigam_plain() -> Value {
    gunzip_fixture(include_bytes!(
        "../fixtures/legacy_deferred_spell_delivery/taigam_collected_conjuring_plain.json.gz"
    ))
}

fn finale_raw() -> Value {
    gunzip_fixture(include_bytes!(
        "../fixtures/legacy_deferred_spell_delivery/finale_of_promise_raw.json.gz"
    ))
}

// ---------------------------------------------------------------------------------------------
// Boards (cast pipeline, no hand-built stack)
// ---------------------------------------------------------------------------------------------

struct Scene {
    runner: GameRunner,
    spell: ObjectId,
    free_sorcery: Option<ObjectId>,
}

fn window_candidates(runner: &GameRunner) -> Option<Vec<ObjectId>> {
    match &runner.state().waiting_for {
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => Some(candidates.clone()),
        _ => None,
    }
}

fn pass_to_window(runner: &mut GameRunner) {
    for _ in 0..12 {
        if window_candidates(runner).is_some() {
            return;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("window never opened: {:?}", runner.state().waiting_for);
}

fn cast(runner: &mut GameRunner, object_id: ObjectId) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
}

/// Taigam attacked and dealt combat damage; the runner is in the second main phase.
fn taigam_attacks(scenario: GameScenario, taigam: ObjectId) -> GameRunner {
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(taigam, AttackTarget::Player(P1))])
        .expect("Taigam attacks");
    runner.combat_damage();
    runner.advance_to_phase(Phase::PostCombatMain);
    runner
}

fn has_rebound(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id].keywords.contains(&Keyword::Rebound)
}

/// Taigam attacked; Collected Conjuring (a sorcery cast from hand) gained rebound from his
/// trigger and resolved to its own free-cast window, where the library offers one free sorcery.
fn taigam_conjuring_scene() -> Scene {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let taigam = scenario
        .add_creature_from_oracle(P0, "Taigam, Ojutai Master", 3, 4, TAIGAM)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Collected Conjuring", false, COLLECTED_CONJURING)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario
        .add_spell_to_library_top(P0, "Unreached", true)
        .from_oracle_text("You gain 1 life.");
    for i in 0..5 {
        scenario.add_land_to_library_top(P0, &format!("Land {i}"));
    }
    let free_sorcery = scenario
        .add_spell_to_library_top(P0, "Free Sorcery", false)
        .from_oracle_text("You gain 1 life.")
        .id();
    let mut runner = taigam_attacks(scenario, taigam);
    cast(&mut runner, spell);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach: Collected Conjuring and Taigam's trigger are on the stack"
    );
    pass_to_window(&mut runner);
    let candidates = window_candidates(&runner).expect("reach: the free-cast window is open");
    assert!(
        candidates.contains(&free_sorcery),
        "reach: the free sorcery is offered"
    );
    Scene {
        runner,
        spell,
        free_sorcery: Some(free_sorcery),
    }
}

/// Finale of Promise (X = 3) paused on its window with an instant and a sorcery to cast.
fn finale_scene() -> Scene {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let finale = scenario
        .add_spell_to_hand_from_oracle(P0, "Finale of Promise", false, FINALE_OF_PROMISE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let instant = scenario
        .add_spell_to_graveyard(P0, "Graveyard Instant", true)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("You gain 2 life.")
        .id();
    let sorcery = scenario
        .add_spell_to_graveyard(P0, "Graveyard Sorcery", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("You gain 3 life.")
        .id();
    let source = ObjectId(0);
    let mut pool = vec![
        ManaUnit::new(ManaType::Red, source, false, vec![]),
        ManaUnit::new(ManaType::Red, source, false, vec![]),
    ];
    pool.extend((0..3).map(|_| ManaUnit::new(ManaType::Colorless, source, false, vec![])));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    cast(&mut runner, finale);
    let mut slot = 0;
    for _ in 0..16 {
        let action = match &runner.state().waiting_for {
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX { value: 3 },
            WaitingFor::TargetSelection { .. } => {
                let target = if slot == 0 { instant } else { sorcery };
                slot += 1;
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                }
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                GameAction::PassPriority
            }
            _ => break,
        };
        runner.act(action).expect("drive the cast");
    }
    assert!(
        window_candidates(&runner).is_some(),
        "reach: Finale of Promise's window is open"
    );
    Scene {
        runner,
        spell: finale,
        free_sorcery: None,
    }
}

/// Invoke Calamity paused on its window (a graveyard instant and a hand sorcery to cast).
fn invoke_scene() -> Scene {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let invoke = scenario
        .add_spell_to_hand_from_oracle(P0, "Invoke Calamity", true, INVOKE_CALAMITY)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario
        .add_spell_to_graveyard(P0, "Graveyard Bolt", true)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Draw a card.");
    scenario
        .add_spell_to_hand(P0, "Hand Divination", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("Draw a card.");
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    cast(&mut runner, invoke);
    pass_to_window(&mut runner);
    Scene {
        runner,
        spell: invoke,
        free_sorcery: None,
    }
}

// ---------------------------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------------------------

fn restore_persisted(value: Value) -> Result<GameState, String> {
    let persisted: PersistedGameState =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    persisted
        .into_game_state()
        .map_err(|error| format!("{error:?}"))
}

fn restore_plain(value: Value) -> Result<GameState, String> {
    serde_json::from_value::<GameState>(value).map_err(|error| error.to_string())
}

fn raw_json(state: &GameState) -> Value {
    serde_json::to_value(PersistedGameState::Raw(Box::new(state.clone()))).expect("raw serializes")
}

fn trusted_json(state: &GameState) -> Value {
    serde_json::to_value(PersistedGameState::capture(state.clone())).expect("trusted serializes")
}

fn plain_json(state: &GameState) -> Value {
    serde_json::to_value(state).expect("plain serializes")
}

fn restore_raw(state: &GameState) -> GameState {
    restore_persisted(raw_json(state)).expect("a current-format raw save restores")
}

/// Decline the spell's free-cast window and drain the stack.
fn decline_window_and_drain(state: GameState) -> GameRunner {
    let mut runner = GameRunner::from_state(state);
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("decline the free-cast window");
    runner.advance_until_stack_empty();
    runner
}

fn assert_refused(result: Result<GameState, String>, route: &str) {
    let error = result.expect_err(&format!(
        "{route}: a legacy deferred-delivery save must be refused"
    ));
    assert!(error.contains(LEGACY_ERROR), "{route}: {error}");
}

// ---------------------------------------------------------------------------------------------
// U2: the legacy save is refused
// ---------------------------------------------------------------------------------------------

fn assert_taigam_fixture_reaches_the_legacy_shape(value: &Value) {
    let record = value
        .get("deferred_spell_delivery")
        .expect("reach: the fixture carries the removed record");
    assert_eq!(record["destination"], "Exile", "reach: the chosen delivery");
    assert_eq!(
        record["object_id"], value["resolving_stack_entry"]["id"],
        "reach: the record names the resolving spell"
    );
    assert_eq!(
        value["delayed_triggers"].as_array().map(Vec::len),
        Some(1),
        "reach: rebound's delayed trigger is already installed"
    );
    assert_eq!(value["waiting_for"]["type"], "CastOffer");
    assert_eq!(
        value["waiting_for"]["data"]["kind"]["type"],
        "FreeCastWindow"
    );
    assert!(
        value.get("resolving_spell_final_part").is_none(),
        "reach: an old-format snapshot has no marker"
    );
}

/// CR 608.2n + CR 702.88a: the old Taigam save cannot be completed without a second rebound
/// installation or a lost delivery, so it is refused on every route into the engine.
#[test]
fn a_legacy_taigam_free_cast_save_is_refused_at_every_decode_boundary() {
    let raw = taigam_raw();
    let plain = taigam_plain();
    assert_taigam_fixture_reaches_the_legacy_shape(&raw);
    assert_taigam_fixture_reaches_the_legacy_shape(&plain);
    assert_eq!(
        raw["resolution_state_version"], 5,
        "reach: the raw wire carries the current version label, so only the key tells old from new"
    );

    assert_refused(restore_persisted(raw.clone()), "PersistedGameState (raw)");
    assert_refused(restore_plain(plain.clone()), "GameState (plain)");
    assert_refused(restore_plain(raw.clone()), "GameState (raw payload)");

    // The trusted envelope nests the state under `state` next to the private runtime data.
    let runtime = trusted_json(&taigam_conjuring_scene().runner.state().clone())
        ["precast_shortcut_runtime"]
        .clone();
    let envelope = serde_json::json!({ "state": raw, "precast_shortcut_runtime": runtime });
    assert_refused(restore_persisted(envelope), "PersistedGameState (trusted)");

    // `prepare_resolution_wire` is the choke point for every declared wire version below the
    // current one; the version label alone must not let the record through.
    for version in [2_u64, 3, 4] {
        let mut relabelled = raw.clone();
        relabelled["resolution_state_version"] = Value::from(version);
        let error = serde_json::from_value::<ResolutionStateWire>(relabelled)
            .expect_err("a relabelled legacy save must be refused")
            .to_string();
        assert!(error.contains(LEGACY_ERROR), "declared v{version}: {error}");
    }
}

/// The refusal depends on the key and on nothing else: the same bytes restore without it.
#[test]
fn scrubbing_the_legacy_key_is_what_makes_the_fixture_restorable() {
    let raw = taigam_raw();
    assert_taigam_fixture_reaches_the_legacy_shape(&raw);

    let mut scrubbed = raw.clone();
    scrubbed
        .as_object_mut()
        .expect("state is an object")
        .remove("deferred_spell_delivery");
    restore_persisted(scrubbed.clone()).expect("without the key the raw fixture restores");
    restore_plain(scrubbed).expect("without the key the plain payload restores");

    // `null` is not a record (the field was omitted when absent, never written as null).
    let mut nulled = raw;
    nulled["deferred_spell_delivery"] = Value::Null;
    restore_persisted(nulled.clone()).expect("a null record restores");
    restore_plain(nulled).expect("a null record restores (plain)");
}

/// The hook-free class (no rebound, destination Graveyard) is refused too: a record cannot say
/// which hooks ran, and that is a stated cost of the refusal.
#[test]
fn a_legacy_finale_of_promise_save_is_refused_too() {
    let raw = finale_raw();
    let record = raw
        .get("deferred_spell_delivery")
        .expect("reach: the fixture carries the removed record");
    assert_eq!(record["destination"], "Graveyard");
    assert_eq!(raw["delayed_triggers"].as_array().map(Vec::len), Some(0));
    assert_eq!(raw["waiting_for"]["data"]["kind"]["type"], "FreeCastWindow");

    assert_refused(restore_persisted(raw.clone()), "PersistedGameState (raw)");
    assert_refused(restore_plain(raw), "GameState (raw payload)");
}

// ---------------------------------------------------------------------------------------------
// The current format round-trips
// ---------------------------------------------------------------------------------------------

/// A Taigam + Collected Conjuring position saved at the spell's own window restores, completes
/// with exactly one rebound installation and its Exile delivery, and equals uninterrupted play,
/// on the raw, trusted and plain forms.
#[test]
fn a_current_format_taigam_free_cast_save_roundtrips_to_one_installation_and_its_delivery() {
    let scene = taigam_conjuring_scene();
    let spell = scene.spell;
    let at_window = scene.runner.state().clone();
    assert_eq!(
        at_window.owed_spell_final_part(),
        Some(&OwedSpellFinalPart { spell }),
        "reach: the final part is owed at the window"
    );
    assert!(
        has_rebound(&at_window, spell),
        "reach: Taigam's trigger resolved, the spell holds rebound"
    );
    assert!(
        at_window.delayed_triggers.is_empty(),
        "the hooks have not run yet, so nothing is installed at the window"
    );

    // Uninterrupted play from the same position is the reference.
    let uninterrupted = decline_window_and_drain(at_window.clone());
    assert_eq!(uninterrupted.state().objects[&spell].zone, Zone::Exile);
    assert_eq!(uninterrupted.state().delayed_triggers.len(), 1);

    let forms: [(&str, Value, bool); 3] = [
        ("raw", raw_json(&at_window), false),
        ("trusted", trusted_json(&at_window), false),
        ("plain", plain_json(&at_window), true),
    ];
    for (form, json, plain) in forms {
        assert!(
            json.get("deferred_spell_delivery").is_none()
                && json["state"].get("deferred_spell_delivery").is_none(),
            "{form}: the removed key is never written"
        );
        let restored = if plain {
            restore_plain(json)
        } else {
            restore_persisted(json)
        }
        .unwrap_or_else(|error| panic!("{form}: a current-format save restores: {error}"));
        assert!(
            has_rebound(&restored, spell),
            "{form}: the restored spell still holds Taigam's granted rebound (a layer pass at \
             restore must re-apply the grant to the popped resolving spell)"
        );
        let done = decline_window_and_drain(restored);
        let state = done.state();
        assert_eq!(
            state.objects[&spell].zone,
            Zone::Exile,
            "{form}: rebound exiles the spell as its final part"
        );
        assert_eq!(
            state.delayed_triggers.len(),
            1,
            "{form}: exactly one rebound installation"
        );
        assert!(
            state.resolving_stack_entry.is_none(),
            "{form}: carrier retired"
        );
        assert!(
            state.owed_spell_final_part().is_none(),
            "{form}: marker retired"
        );
    }
}

/// The hook-free current-format roundtrips keep their delivery (controls for the refusal).
#[test]
fn a_current_format_finale_and_invoke_calamity_saves_keep_their_delivery() {
    for (name, scene, expected) in [
        ("Finale of Promise", finale_scene(), Zone::Graveyard),
        ("Invoke Calamity", invoke_scene(), Zone::Exile),
    ] {
        let spell = scene.spell;
        let at_window = scene.runner.state().clone();
        assert!(
            at_window.owed_spell_final_part().is_some(),
            "{name}: reach: the final part is owed at the window"
        );
        let uninterrupted = decline_window_and_drain(at_window.clone());
        assert_eq!(
            uninterrupted.state().objects[&spell].zone,
            expected,
            "{name}"
        );
        for (form, json, plain) in [
            ("raw", raw_json(&at_window), false),
            ("trusted", trusted_json(&at_window), false),
            ("plain", plain_json(&at_window), true),
        ] {
            let restored = if plain {
                restore_plain(json)
            } else {
                restore_persisted(json)
            }
            .unwrap_or_else(|error| panic!("{name} {form}: restores: {error}"));
            let done = decline_window_and_drain(restored);
            assert_eq!(
                done.state().objects[&spell].zone,
                expected,
                "{name} {form}: delivery kept"
            );
            assert!(
                done.state().owed_spell_final_part().is_none(),
                "{name} {form}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// U3: the layer scan sees the popped resolving spell
// ---------------------------------------------------------------------------------------------

/// A free sorcery cast inside Collected Conjuring's own window runs layer passes while the
/// Conjuring is popped off the stack. The pass must not drop Taigam's grant: rebound is read at
/// the final part, after the window closes.
#[test]
fn taigam_rebound_survives_a_free_cast_in_the_free_cast_window() {
    let mut scene = taigam_conjuring_scene();
    let spell = scene.spell;
    let sorcery = scene.free_sorcery.expect("the scene offers a free sorcery");
    let life_before = scene.runner.state().players[0].life;
    assert!(
        window_candidates(&scene.runner).is_some(),
        "reach: the window was offered before the pick"
    );
    scene
        .runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(sorcery),
        })
        .expect("cast the free sorcery");
    for _ in 0..16 {
        match &scene.runner.state().waiting_for {
            WaitingFor::CastOffer { .. } => {
                scene
                    .runner
                    .act(GameAction::FreeCastWindowChoice { selection: None })
                    .expect("decline the re-offered window");
            }
            WaitingFor::Priority { .. } if scene.runner.state().stack.is_empty() => break,
            _ => {
                scene
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority");
            }
        }
    }
    let state = scene.runner.state();
    assert_eq!(
        state.players[0].life,
        life_before + 1,
        "reach: the nested free sorcery was cast and resolved"
    );
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Exile,
        "rebound (granted before the window) exiles the Conjuring"
    );
    assert_eq!(state.delayed_triggers.len(), 1, "one rebound installation");
}

struct PauseCase {
    name: &'static str,
    oracle: &'static str,
    instant: bool,
    library_lands: usize,
    pause: fn(&WaitingFor) -> bool,
    pause_name: &'static str,
}

fn build_pause(case: &PauseCase) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let taigam = scenario
        .add_creature_from_oracle(P0, "Taigam, Ojutai Master", 3, 4, TAIGAM)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, case.name, case.instant, case.oracle)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.add_card_to_hand(P0, "Spare A");
    scenario.add_card_to_hand(P0, "Spare B");
    for i in 0..case.library_lands {
        scenario.add_land_to_library_top(P0, &format!("Land {i}"));
    }
    let mut runner = taigam_attacks(scenario, taigam);
    cast(&mut runner, spell);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach: {} and Taigam's trigger are on the stack",
        case.name
    );
    for _ in 0..8 {
        if (case.pause)(&runner.state().waiting_for) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    assert!(
        (case.pause)(&runner.state().waiting_for),
        "reach: {} pauses on {}, got {:?}",
        case.name,
        case.pause_name,
        runner.state().waiting_for
    );
    (runner, spell)
}

/// Answer the pause (and any prompt after it) and drain the stack.
fn answer_and_drain(runner: &mut GameRunner) {
    for _ in 0..16 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::ScryChoice { .. } => GameAction::SelectCards { cards: vec![] },
            WaitingFor::DiscardChoice { cards, count, .. } => GameAction::SelectCards {
                cards: cards.into_iter().take(count).collect(),
            },
            WaitingFor::DigChoice {
                keep_count,
                selectable_cards,
                cards,
                ..
            } => {
                let pool = if selectable_cards.is_empty() {
                    cards
                } else {
                    selectable_cards
                };
                GameAction::SelectCards {
                    cards: pool.into_iter().take(keep_count).collect(),
                }
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt while answering: {other:?}"),
        };
        runner.act(action).expect("answer");
    }
    panic!(
        "the resolution never settled: {:?}",
        runner.state().waiting_for
    );
}

/// The shared body of the pause rows: a granted rebound is on the spell at the pause and in the
/// restored state, and the spell ends in exile with one installation, uninterrupted and across
/// a raw save taken at the pause. The grant guards come BEFORE the outcome assertions so a
/// failing outcome is only ever reached past the proof that the grant was there.
fn assert_rebound_survives_the_pause(case: &PauseCase, grant_expected_at_pause: bool) {
    // Uninterrupted continuation.
    let (mut runner, spell) = build_pause(case);
    let at_pause = runner.state().clone();
    assert_eq!(
        at_pause.owed_spell_final_part(),
        Some(&OwedSpellFinalPart { spell }),
        "reach: the final part is owed at the {} pause",
        case.pause_name
    );
    assert_eq!(
        at_pause.objects[&spell].zone,
        Zone::Stack,
        "reach: still on the stack"
    );
    assert_eq!(
        has_rebound(&at_pause, spell),
        grant_expected_at_pause,
        "{}: the granted rebound at the pause (a layer pass that does not enumerate the popped \
         resolving spell drops it)",
        case.pause_name
    );
    answer_and_drain(&mut runner);
    let state = runner.state();
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Exile,
        "{} uninterrupted: rebound exiles the spell",
        case.name
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "{} uninterrupted: one installation",
        case.name
    );
    assert!(state.owed_spell_final_part().is_none() && state.resolving_stack_entry.is_none());

    // Across a raw save taken at the pause.
    let (runner, spell) = build_pause(case);
    let restored = restore_raw(runner.state());
    assert!(
        restored.owed_spell_final_part().is_some(),
        "reach: marker restored"
    );
    assert_eq!(
        restored.objects[&spell].zone,
        Zone::Stack,
        "reach: still on the stack"
    );
    assert_eq!(
        has_rebound(&restored, spell),
        grant_expected_at_pause,
        "{}: the granted rebound in the restored state (the restore's layer pass must re-apply \
         the grant to the popped resolving spell)",
        case.pause_name
    );
    let mut runner = GameRunner::from_state(restored);
    answer_and_drain(&mut runner);
    let state = runner.state();
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Exile,
        "{} after a save at the pause: rebound exiles the spell",
        case.name
    );
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "{} after a save at the pause: one installation",
        case.name
    );
    assert!(state.owed_spell_final_part().is_none() && state.resolving_stack_entry.is_none());
}

/// A layer pass BEFORE the prompt opens (the draw ahead of the discard) must not drop the grant.
#[test]
fn taigam_rebound_survives_a_discard_pause() {
    let case = PauseCase {
        name: "Careful Study",
        oracle: CAREFUL_STUDY,
        instant: false,
        library_lands: 4,
        pause: |wf| matches!(wf, WaitingFor::DiscardChoice { count: 2, cards, .. } if cards.len() == 4),
        pause_name: "DiscardChoice",
    };
    assert_rebound_survives_the_pause(&case, true);
}

/// A save restored at a choice prompt: the restore's layer pass must not drop the grant.
#[test]
fn taigam_rebound_survives_a_save_at_a_scry_pause() {
    let case = PauseCase {
        name: "Preordain",
        oracle: PREORDAIN,
        instant: false,
        library_lands: 4,
        pause: |wf| matches!(wf, WaitingFor::ScryChoice { .. }),
        pause_name: "ScryChoice",
    };
    assert_rebound_survives_the_pause(&case, true);
}

/// The answer to a prompt whose own state change runs a layer pass (the dig moves a card to
/// hand) must not drop the grant, with or without a save.
#[test]
fn taigam_rebound_survives_a_dig_answer() {
    let case = PauseCase {
        name: "Impulse",
        oracle: IMPULSE,
        instant: true,
        library_lands: 6,
        pause: |wf| matches!(wf, WaitingFor::DigChoice { .. }),
        pause_name: "DigChoice",
    };
    assert_rebound_survives_the_pause(&case, true);
}
