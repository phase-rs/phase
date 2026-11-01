//! Sandbox battlefield entries seed the intrinsic loyalty / defense counters.
//!
//! CR 306.5b: a planeswalker has the intrinsic ability "This permanent enters
//! with a number of loyalty counters on it equal to its printed loyalty number",
//! and CR 306.5c makes its loyalty on the battlefield the number of loyalty
//! counters on it. CR 310.4b / CR 310.4c say the same of a battle's defense
//! counters. The sandbox Create Card and Move to zone tools reach the
//! battlefield three ways, and each must leave the counter map holding the
//! printed number:
//!
//! - the consulting entry (`CreateCard { run_etb: true }` of a card,
//!   `MoveToZone { simulate: true }`): the counters ride the proposal through
//!   the replacement window, so Doubling Season class replacements (CR 614.1a)
//!   and the CR 616.1 order choice apply to them;
//! - the token entry (`CreateCard { run_etb: true }` of a token): the printed
//!   loyalty seeds the liminal token entry, applied through the same window;
//! - the raw placement (`CreateCard { run_etb: false }`,
//!   `MoveToZone { simulate: false }`): no replacement consult and no prompt,
//!   but the CR 306.5b / CR 310.4b characteristic counters are still installed,
//!   because CR 306.5c / CR 310.4c make them the permanent's loyalty / defense.
//!   Every other enters-with replacement (Saga lore, "enters with N +1/+1
//!   counters") stays skipped by the raw contract.
//!
//! Move to zone with simulate on an object already on the battlefield (or on a
//! token that has left it) is no entry, so it consults and applies nothing
//! (CR 400.6, CR 603.6d, CR 111.8). An entry a "can't enter the battlefield"
//! static refuses puts no counters on the card it leaves behind (CR 614.17c).
//!
//! Every card is the committed fixture's verbatim Oracle face (Weathered
//! Runestone, absent from the fixture, is built from its verbatim Oracle
//! text), and every entry goes through `create_debug_cards` (the engine entry
//! both transports call), `GameAction::Debug(DebugAction::MoveToZone)` or, for
//! the zone pipeline's own caller, `ZoneMoveRequest::effect`.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::game::{create_debug_cards, debug_card_entry_source, DebugCardCreateRequest};
use engine::types::ability::AbilityCost;
use engine::types::actions::{DebugAction, DebugCardCreationKind, GameAction};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::ResolutionFrame;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const COMET: &str = "Comet, Stellar Pup";
const ELSPETH: &str = "Elspeth Tirel";
const SIEGE: &str = "Invasion of Amonkhet";
const DOUBLING_SEASON: &str = "Doubling Season";
const VORINCLEX: &str = "Vorinclex, Monstrous Raider";
const SPIKE_FEEDER: &str = "Spike Feeder";
const HISTORY_OF_BENALIA: &str = "History of Benalia";
const BLOODFELL_CAVES: &str = "Bloodfell Caves";

/// Verbatim Oracle text of Shock.
const SHOCK: &str = "Shock deals 2 damage to any target.";

/// Weathered Runestone is not in the committed fixture, so it is built from
/// its verbatim Oracle text.
const WEATHERED_RUNESTONE: &str = "Weathered Runestone";
const WEATHERED_RUNESTONE_ORACLE: &str = "Nonland permanent cards in graveyards and libraries can't enter the battlefield.\nPlayers can't cast spells from graveyards or libraries.";

/// The three Create Card routes that put the created object onto the
/// battlefield: `(run_etb, creation_kind)`. A token with `run_etb: false`
/// never reaches the battlefield (CR 111.8), so it is not a row.
const CREATE_ROUTES: [(bool, DebugCardCreationKind); 3] = [
    (true, DebugCardCreationKind::Card),
    (false, DebugCardCreationKind::Card),
    (true, DebugCardCreationKind::Token),
];

fn db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration fixture loads")
}

/// A main-phase sandbox table: `setup` stages its battlefield fixtures, then
/// debug mode is enabled.
fn board(setup: impl FnOnce(&mut GameScenario)) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    setup(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    runner
}

/// A table with no battlefield fixtures.
fn plain_board(_: &mut GameScenario) {}

/// P0's Doubling Season: "If an effect would put one or more counters on a
/// permanent you control, it puts twice that many of those counters on that
/// permanent instead."
fn with_doubling_season(scenario: &mut GameScenario) {
    scenario.add_real_card(P0, DOUBLING_SEASON, Zone::Battlefield, db());
}

/// P0's Doubling Season and P1's Vorinclex, Monstrous Raider: two counter
/// replacements whose order changes the result (CR 616.1), so a consulting
/// entry must ask P0 and a raw placement must not.
fn with_order_matters(scenario: &mut GameScenario) {
    with_doubling_season(scenario);
    scenario.add_real_card(P1, VORINCLEX, Zone::Battlefield, db());
}

/// P0's Weathered Runestone: "Nonland permanent cards in graveyards and
/// libraries can't enter the battlefield." (CR 614.17d).
fn with_weathered_runestone(scenario: &mut GameScenario) {
    scenario.add_artifact_from_oracle(P0, WEATHERED_RUNESTONE, WEATHERED_RUNESTONE_ORACLE);
}

/// Create one `name` through the engine's debug Create Card entry.
fn create(
    runner: &mut GameRunner,
    name: &str,
    owner: PlayerId,
    zone: Zone,
    run_etb: bool,
    creation_kind: DebugCardCreationKind,
) {
    let db = db();
    let face = db
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("{name} is in the committed fixture"));
    create_debug_cards(
        runner.state_mut(),
        DebugCardCreateRequest {
            actor: P0,
            source: debug_card_entry_source(db, face),
            owner,
            zone,
            count: 1,
            attach_to: None,
            run_etb,
            nonlegendary: false,
            creation_kind,
        },
    )
    .unwrap_or_else(|error| panic!("the debug creation of {name} is accepted: {error:?}"));
}

/// The one object named `name`. A second object would mean a token was
/// doubled or the staged card survived beside its token.
fn only_object_named(runner: &GameRunner, name: &str) -> ObjectId {
    let ids: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.name == name)
        .map(|object| object.id)
        .collect();
    assert_eq!(ids.len(), 1, "exactly one {name} exists: {ids:?}");
    ids[0]
}

/// Send `MoveToZone { to_zone, simulate }` through the action boundary and
/// return the accepted action's events.
fn move_to(
    runner: &mut GameRunner,
    object_id: ObjectId,
    to_zone: Zone,
    simulate: bool,
) -> Vec<GameEvent> {
    runner
        .act(GameAction::Debug(DebugAction::MoveToZone {
            object_id,
            to_zone,
            library_position: None,
            simulate,
        }))
        .unwrap_or_else(|error| panic!("the debug move is accepted: {error:?}"))
        .events
}

fn is_counter_added(event: &GameEvent) -> bool {
    matches!(event, GameEvent::CounterAdded { .. })
}

fn is_zone_changed(event: &GameEvent) -> bool {
    matches!(event, GameEvent::ZoneChanged { .. })
}

/// Assert a move put nothing onto the battlefield: no counter was put on
/// anything and nothing changed zones.
fn assert_no_entry_events(events: &[GameEvent], row: &str) {
    assert!(
        !events.iter().any(is_counter_added),
        "{row}: no counter is put: {events:?}"
    );
    assert!(
        !events.iter().any(is_zone_changed),
        "{row}: nothing changes zones: {events:?}"
    );
}

fn incarnation(runner: &GameRunner, id: ObjectId) -> u64 {
    runner.state().objects[&id].incarnation
}

fn counter(runner: &GameRunner, id: ObjectId, counter_type: CounterType) -> Option<u32> {
    runner.state().objects[&id]
        .counters
        .get(&counter_type)
        .copied()
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn is_priority(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
}

/// A readable head of a wait state.
fn short(waiting_for: &WaitingFor) -> String {
    format!("{waiting_for:?}").chars().take(120).collect()
}

/// Assert `id` holds `count` loyalty counters and that its loyalty is that
/// count (CR 306.5c).
fn assert_loyalty(runner: &GameRunner, id: ObjectId, count: u32, row: &str) {
    assert_eq!(
        counter(runner, id, CounterType::Loyalty),
        Some(count),
        "{row}: loyalty counters"
    );
    assert_eq!(
        runner.state().objects[&id].loyalty,
        Some(count),
        "{row}: CR 306.5c loyalty"
    );
}

/// Assert `id` holds `count` defense counters and that its defense is that
/// count (CR 310.4c).
fn assert_defense(runner: &GameRunner, id: ObjectId, count: u32, row: &str) {
    assert_eq!(
        counter(runner, id, CounterType::Defense),
        Some(count),
        "{row}: defense counters"
    );
    assert_eq!(
        runner.state().objects[&id].defense,
        Some(count),
        "{row}: CR 310.4c defense"
    );
}

/// The index of `id`'s loyalty ability whose cost is `[amount]`, read from the
/// parsed abilities.
fn loyalty_ability(runner: &GameRunner, id: ObjectId, amount: i32) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|ability| ability.cost == Some(AbilityCost::Loyalty { amount }))
        .unwrap_or_else(|| panic!("the fixture face carries a [{amount}] ability"))
}

fn activate(
    runner: &mut GameRunner,
    source_id: ObjectId,
    ability_index: usize,
) -> Result<engine::types::game_state::ActionResult, engine::game::engine::EngineError> {
    runner.act(GameAction::ActivateAbility {
        source_id,
        ability_index,
        payment_mode: CastPaymentMode::Auto,
    })
}

/// Pass priority until the stack is empty or the engine asks for an answer.
fn settle(runner: &mut GameRunner) {
    for _ in 0..20 {
        if runner.state().stack.is_empty() || !is_priority(runner) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes while the stack resolves");
    }
}

/// CR 306.5b + CR 306.5c, on every Create Card route that reaches the
/// battlefield: Comet (printed loyalty 5) enters with five loyalty counters,
/// and his loyalty is that count.
///
/// Red at base: each route left the loyalty field at 5 with no loyalty counter.
#[test]
fn debug_created_planeswalker_enters_with_printed_loyalty_counters() {
    for (run_etb, kind) in CREATE_ROUTES {
        let row = format!("run_etb={run_etb}, {kind:?}");
        let mut runner = board(|_| {});
        create(&mut runner, COMET, P0, Zone::Battlefield, run_etb, kind);
        let comet = only_object_named(&runner, COMET);

        assert_eq!(zone_of(&runner, comet), Zone::Battlefield, "{row}");
        assert_eq!(
            runner.state().objects[&comet].is_token,
            kind == DebugCardCreationKind::Token,
            "{row}"
        );
        assert_loyalty(&runner, comet, 5, &row);
        assert!(
            is_priority(&runner),
            "{row}: {}",
            short(&runner.state().waiting_for)
        );
    }
}

/// CR 306.5b + CR 306.5c through the Move to zone tool, in both modes: a
/// Comet card in the graveyard moved onto the battlefield enters with five
/// loyalty counters.
///
/// Red at base: neither mode installed a loyalty counter.
#[test]
fn debug_moved_planeswalker_enters_with_printed_loyalty_counters() {
    for simulate in [true, false] {
        let row = format!("simulate={simulate}");
        let mut runner = board(|_| {});
        create(
            &mut runner,
            COMET,
            P0,
            Zone::Graveyard,
            true,
            DebugCardCreationKind::Card,
        );
        let comet = only_object_named(&runner, COMET);
        assert_eq!(
            zone_of(&runner, comet),
            Zone::Graveyard,
            "{row}: precondition"
        );
        assert_eq!(
            counter(&runner, comet, CounterType::Loyalty),
            None,
            "{row}: precondition, no counters in the graveyard"
        );

        move_to(&mut runner, comet, Zone::Battlefield, simulate);

        assert_eq!(zone_of(&runner, comet), Zone::Battlefield, "{row}");
        assert_loyalty(&runner, comet, 5, &row);
        assert!(is_priority(&runner), "{row}");
    }
}

/// The consulting and token routes put the intrinsic counters through the
/// replacement window and the raw route does not consult it. With P0's
/// Doubling Season, Comet entering under P0 gets twice five (CR 614.1a; the
/// card's ruling: "Planeswalkers will enter with double the normal number of
/// loyalty counters"), and the token route still creates one token: the
/// sandbox tool is not an effect creating tokens. The raw placement installs
/// the printed five. Owner axis: a Comet P1 controls is not "a permanent you
/// control" for P0's Doubling Season, so it enters with five on either
/// consulting route.
///
/// Red at base: no route put any loyalty counter on Comet.
#[test]
fn debug_created_planeswalker_loyalty_follows_the_replacement_window() {
    // (run_etb, kind, owner, expected loyalty)
    let rows = [
        (true, DebugCardCreationKind::Card, P0, 10),
        (true, DebugCardCreationKind::Token, P0, 10),
        (false, DebugCardCreationKind::Card, P0, 5),
        (true, DebugCardCreationKind::Card, P1, 5),
        (true, DebugCardCreationKind::Token, P1, 5),
    ];
    for (run_etb, kind, owner, expected) in rows {
        let row = format!("run_etb={run_etb}, {kind:?}, owner {owner:?}");
        let mut runner = board(with_doubling_season);
        create(&mut runner, COMET, owner, Zone::Battlefield, run_etb, kind);
        let comet = only_object_named(&runner, COMET);

        let object = &runner.state().objects[&comet];
        assert_eq!(object.zone, Zone::Battlefield, "{row}");
        assert_eq!(object.controller, owner, "{row}");
        assert_eq!(
            object.is_token,
            kind == DebugCardCreationKind::Token,
            "{row}"
        );
        assert_loyalty(&runner, comet, expected, &row);
        assert!(
            is_priority(&runner),
            "{row}: {}",
            short(&runner.state().waiting_for)
        );
    }
}

/// The raw placement never pauses. On a board where the order of two counter
/// replacements matters (P0's Doubling Season, P1's Vorinclex), a consulting
/// entry asks P0 to order them (see
/// `debug_entered_planeswalker_loyalty_follows_the_chosen_replacement_order`);
/// the raw placement consults neither, installs the printed five, and leaves
/// the game at Priority with nothing parked.
///
/// Red at base: the raw placement installed no loyalty counter.
#[test]
fn raw_debug_placement_installs_printed_loyalty_without_a_replacement_choice() {
    for via_move in [false, true] {
        let row = if via_move {
            "MoveToZone simulate=false"
        } else {
            "CreateCard run_etb=false"
        };
        let mut runner = board(with_order_matters);
        if via_move {
            create(
                &mut runner,
                COMET,
                P0,
                Zone::Graveyard,
                true,
                DebugCardCreationKind::Card,
            );
            let comet = only_object_named(&runner, COMET);
            assert_eq!(zone_of(&runner, comet), Zone::Graveyard, "{row}");
            move_to(&mut runner, comet, Zone::Battlefield, false);
        } else {
            create(
                &mut runner,
                COMET,
                P0,
                Zone::Battlefield,
                false,
                DebugCardCreationKind::Card,
            );
        }
        let comet = only_object_named(&runner, COMET);

        assert_eq!(zone_of(&runner, comet), Zone::Battlefield, "{row}");
        assert_loyalty(&runner, comet, 5, row);
        assert!(
            is_priority(&runner),
            "{row}: no replacement choice: {}",
            short(&runner.state().waiting_for)
        );
        assert!(
            runner.state().resolution_stack.is_empty(),
            "{row}: nothing is parked"
        );
    }
}

/// CR 310.4b + CR 310.4c: Invasion of Amonkhet (printed defense 4) enters with
/// four defense counters. On the consulting routes (card and token) the
/// CR 614.12a protector choice pauses the entry before it enters, and the
/// counters survive the resume, after which its ETB trigger (CR 603.6a) waits
/// on the stack. On the raw route the counters are installed
/// directly, and they are what damage removes: Shock deals 2 damage to the
/// battle, which removes two defense counters (CR 120.3h + CR 310.6).
///
/// Red at base: no route installed a defense counter, so Shock left the raw
/// battle at defense 4. The token route stayed red until its TokenEntry
/// proposal carried the staged card's printed defense.
#[test]
fn debug_created_battle_enters_with_printed_defense_counters() {
    // Consulting routes, card and token: the protector choice, then the
    // counters.
    for kind in [DebugCardCreationKind::Card, DebugCardCreationKind::Token] {
        let row = format!("run_etb=true, {kind:?}");
        let mut runner = board(|_| {});
        create(&mut runner, SIEGE, P0, Zone::Battlefield, true, kind);
        let choice = match &runner.state().waiting_for {
            WaitingFor::NamedChoice {
                player, options, ..
            } => {
                assert_eq!(
                    *player, P0,
                    "{row}: the Siege's controller chooses its protector"
                );
                options
                    .first()
                    .cloned()
                    .expect("an opponent can protect the Siege")
            }
            other => panic!("{row}: expected the protector choice, got {}", short(other)),
        };
        runner
            .act(GameAction::ChooseOption { choice })
            .expect("the protector choice is accepted");
        let siege = only_object_named(&runner, SIEGE);
        assert_eq!(zone_of(&runner, siege), Zone::Battlefield, "{row}");
        assert_eq!(
            runner.state().objects[&siege].is_token,
            kind == DebugCardCreationKind::Token,
            "{row}"
        );
        assert_defense(&runner, siege, 4, &row);
        assert!(
            is_priority(&runner),
            "{row}: {}",
            short(&runner.state().waiting_for)
        );
        // CR 603.6a: "When this Siege enters, each player mills three cards,
        // ..." triggers once the resumed entry completes, so its one ETB
        // trigger waits on the stack.
        let stack = &runner.state().stack;
        assert_eq!(stack.len(), 1, "{row}: only the Siege's ETB trigger");
        assert!(
            matches!(
                stack[0].kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == siege
            ),
            "{row}: the stack entry is the Siege's triggered ability"
        );
        let resolution_stack = &runner.state().resolution_stack;
        assert!(
            resolution_stack.is_empty(),
            "{row}: the parked entry resumed and finished, leftover frames {:?}",
            resolution_stack
                .iter()
                .map(ResolutionFrame::kind)
                .collect::<Vec<_>>()
        );
    }

    // Raw route: the counters, then Shock removes two of them.
    let mut shock = None;
    let mut runner = board(|scenario| {
        shock = Some(
            scenario
                .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
                .id(),
        );
    });
    let shock = shock.expect("Shock is in hand");
    create(
        &mut runner,
        SIEGE,
        P0,
        Zone::Battlefield,
        false,
        DebugCardCreationKind::Card,
    );
    let siege = only_object_named(&runner, SIEGE);
    assert_defense(&runner, siege, 4, "run_etb=false");
    assert!(is_priority(&runner), "run_etb=false");

    runner.cast(shock).target_objects(&[siege]).resolve();
    assert_eq!(
        zone_of(&runner, siege),
        Zone::Battlefield,
        "run_etb=false: two damage leaves it with defense"
    );
    assert_defense(&runner, siege, 2, "run_etb=false, after Shock");
}

/// The raw contract installs only the counters CR 306.5c / CR 310.4c make a
/// characteristic. A raw Spike Feeder ("enters with two +1/+1 counters") gets
/// no +1/+1 counter and no loyalty counter, and a raw History of Benalia gets
/// no lore counter (CR 714.3a is a replacement the raw contract skips). Paired
/// positives: a raw Comet does get its five loyalty counters, and the
/// consulting route still applies both printed replacements.
#[test]
fn raw_debug_placement_installs_only_characteristic_counters() {
    let raw = |name: &str| {
        let mut runner = board(|_| {});
        create(
            &mut runner,
            name,
            P0,
            Zone::Battlefield,
            false,
            DebugCardCreationKind::Card,
        );
        let id = only_object_named(&runner, name);
        assert_eq!(zone_of(&runner, id), Zone::Battlefield, "raw {name}");
        (runner, id)
    };
    let consulting = |name: &str| {
        let mut runner = board(|_| {});
        create(
            &mut runner,
            name,
            P0,
            Zone::Battlefield,
            true,
            DebugCardCreationKind::Card,
        );
        let id = only_object_named(&runner, name);
        assert_eq!(zone_of(&runner, id), Zone::Battlefield, "consulting {name}");
        (runner, id)
    };

    // Positive reach-guard: the install branch runs on a raw placement.
    let (runner, comet) = raw(COMET);
    assert_loyalty(&runner, comet, 5, "raw Comet");

    let (runner, feeder) = raw(SPIKE_FEEDER);
    assert_eq!(
        counter(&runner, feeder, CounterType::Plus1Plus1),
        None,
        "raw Spike Feeder: its enters-with replacement is skipped"
    );
    assert_eq!(
        counter(&runner, feeder, CounterType::Loyalty),
        None,
        "raw Spike Feeder: not a planeswalker"
    );
    let (runner, saga) = raw(HISTORY_OF_BENALIA);
    assert_eq!(
        counter(&runner, saga, CounterType::Lore),
        None,
        "raw History of Benalia: the CR 714.3a lore replacement is skipped"
    );

    let (runner, feeder) = consulting(SPIKE_FEEDER);
    assert_eq!(
        counter(&runner, feeder, CounterType::Plus1Plus1),
        Some(2),
        "consulting Spike Feeder"
    );
    let (runner, saga) = consulting(HISTORY_OF_BENALIA);
    assert_eq!(
        counter(&runner, saga, CounterType::Lore),
        Some(1),
        "consulting History of Benalia: CR 714.3a"
    );
}

/// The sandbox route a CR 616.1 order-choice row enters Comet by.
#[derive(Debug, Clone, Copy)]
enum ConsultingEntry {
    /// `CreateCard { run_etb: true }` of a card.
    CreateCard,
    /// `MoveToZone { simulate: true }` of a Comet card in the graveyard.
    MoveFromGraveyard,
    /// `CreateCard { run_etb: true }` of a token.
    CreateToken,
}

/// CR 616.1 on the consulting and token routes. With P0's Doubling Season and
/// P1's Vorinclex, Monstrous Raider both applying to Comet's five entering
/// loyalty counters, the affected object's controller P0 chooses the order,
/// and the entry resumes to the chosen result. Comet's controller puts the
/// counters (CR 122.6a), so Vorinclex's opponent clause halves them, rounded
/// down: Doubling Season first gives ⌊10 / 2⌋ = 5, Vorinclex first gives
/// ⌊5 / 2⌋ × 2 = 4.
///
/// Red at base: no route seeded the counters, so no choice was asked and no
/// loyalty counter was put on Comet.
#[test]
fn debug_entered_planeswalker_loyalty_follows_the_chosen_replacement_order() {
    for entry in [
        ConsultingEntry::CreateCard,
        ConsultingEntry::MoveFromGraveyard,
        ConsultingEntry::CreateToken,
    ] {
        for (first, expected) in [(DOUBLING_SEASON, 5u32), (VORINCLEX, 4)] {
            let row = format!("{entry:?}, {first} first");
            let mut runner = board(with_order_matters);
            match entry {
                ConsultingEntry::CreateCard => create(
                    &mut runner,
                    COMET,
                    P0,
                    Zone::Battlefield,
                    true,
                    DebugCardCreationKind::Card,
                ),
                ConsultingEntry::MoveFromGraveyard => {
                    create(
                        &mut runner,
                        COMET,
                        P0,
                        Zone::Graveyard,
                        true,
                        DebugCardCreationKind::Card,
                    );
                    let comet = only_object_named(&runner, COMET);
                    move_to(&mut runner, comet, Zone::Battlefield, true);
                }
                ConsultingEntry::CreateToken => create(
                    &mut runner,
                    COMET,
                    P0,
                    Zone::Battlefield,
                    true,
                    DebugCardCreationKind::Token,
                ),
            }

            let index = match &runner.state().waiting_for {
                WaitingFor::ReplacementChoice {
                    player,
                    candidate_count,
                    candidates,
                    ..
                } => {
                    assert_eq!(*player, P0, "{row}: the affected object's controller");
                    assert_eq!(*candidate_count, 2, "{row}");
                    let mut names: Vec<&str> = candidates
                        .iter()
                        .map(|candidate| candidate.source_name.as_str())
                        .collect();
                    names.sort_unstable();
                    assert_eq!(names, [DOUBLING_SEASON, VORINCLEX], "{row}");
                    candidates
                        .iter()
                        .position(|candidate| candidate.source_name == first)
                        .expect("the chosen replacement is a candidate")
                }
                other => panic!("{row}: expected the CR 616.1 choice, got {}", short(other)),
            };

            // While paused: Comet is on the battlefield and no loyalty counter
            // has been put on him yet.
            let comet = only_object_named(&runner, COMET);
            assert_eq!(zone_of(&runner, comet), Zone::Battlefield, "{row}: paused");
            assert_eq!(
                counter(&runner, comet, CounterType::Loyalty),
                None,
                "{row}: paused"
            );
            let resolution_stack = &runner.state().resolution_stack;
            assert!(
                !resolution_stack.is_empty(),
                "{row}: reach-guard, the paused addition is parked"
            );
            if !matches!(entry, ConsultingEntry::MoveFromGraveyard) {
                assert!(
                    resolution_stack
                        .iter()
                        .any(|frame| matches!(frame, ResolutionFrame::DebugCardEntries(_))),
                    "{row}: reach-guard, the debug batch parent is parked below the choice"
                );
            }

            runner
                .act(GameAction::ChooseReplacement { index })
                .unwrap_or_else(|error| panic!("{row}: the order is accepted: {error:?}"));

            let comet = only_object_named(&runner, COMET);
            assert_eq!(zone_of(&runner, comet), Zone::Battlefield, "{row}");
            assert_loyalty(&runner, comet, expected, &row);
            assert!(
                is_priority(&runner),
                "{row}: {}",
                short(&runner.state().waiting_for)
            );
            assert!(
                runner.state().resolution_stack.is_empty(),
                "{row}: the debug batch and the counter queue drained"
            );
        }
    }
}

/// CR 606.4 + CR 606.6: a debug-created Elspeth Tirel (printed loyalty 4) pays
/// her loyalty costs from her counters. Her [−5] can't be activated with four
/// loyalty counters; her [−2] is accepted and removes two of them during
/// activation, before the ability resolves, and it then creates three Soldier
/// tokens.
///
/// Red at base: she had no loyalty counter, so the [−2] was paid for free and
/// left her at loyalty 4.
#[test]
fn debug_created_planeswalker_pays_negative_loyalty_costs_from_its_counters() {
    for (run_etb, kind) in CREATE_ROUTES {
        let row = format!("run_etb={run_etb}, {kind:?}");
        let mut runner = board(|_| {});
        create(&mut runner, ELSPETH, P0, Zone::Battlefield, run_etb, kind);
        let elspeth = only_object_named(&runner, ELSPETH);
        assert_loyalty(&runner, elspeth, 4, &row);

        let minus_five = loyalty_ability(&runner, elspeth, -5);
        assert!(
            activate(&mut runner, elspeth, minus_five).is_err(),
            "{row}: CR 606.6, four loyalty counters can't pay [−5]"
        );

        let minus_two = loyalty_ability(&runner, elspeth, -2);
        activate(&mut runner, elspeth, minus_two)
            .unwrap_or_else(|error| panic!("{row}: the [−2] is accepted: {error:?}"));
        assert_eq!(
            runner.state().stack.len(),
            1,
            "{row}: the ability is on the stack"
        );
        assert_loyalty(&runner, elspeth, 2, &format!("{row}, cost paid"));

        settle(&mut runner);
        let state = runner.state();
        let soldiers = state
            .battlefield
            .iter()
            .filter(|id| {
                let object = &state.objects[id];
                object.is_token && object.name == "Soldier" && object.controller == P0
            })
            .count();
        assert_eq!(soldiers, 3, "{row}: reach-guard, the ability resolved");
        assert!(state.stack.is_empty(), "{row}");
        assert!(is_priority(&runner), "{row}");
        assert_eq!(zone_of(&runner, elspeth), Zone::Battlefield, "{row}");
        assert_loyalty(&runner, elspeth, 2, &format!("{row}, resolved"));
    }
}

/// Send `id`, a permanent already on the battlefield, onto the battlefield by
/// Move to zone with simulate, and assert the move put nothing onto the
/// battlefield (CR 400.6: it is not a move from one zone to another): no
/// `ZoneChanged` or `CounterAdded` event, the same object (no CR 400.7 new
/// object) still on the battlefield, no trigger added to the stack (CR 603.6a:
/// no event put a permanent onto the battlefield), nothing parked, and nothing
/// asked.
fn assert_battlefield_move_is_refused(runner: &mut GameRunner, id: ObjectId, row: &str) {
    assert_eq!(
        zone_of(runner, id),
        Zone::Battlefield,
        "{row}: precondition"
    );
    assert!(
        is_priority(runner),
        "{row}: precondition: {}",
        short(&runner.state().waiting_for)
    );
    let before = incarnation(runner, id);
    let stack = runner.state().stack.len();
    let resolution_stack = runner.state().resolution_stack.len();

    let events = move_to(runner, id, Zone::Battlefield, true);

    assert!(
        is_priority(runner),
        "{row}: nothing is asked: {}",
        short(&runner.state().waiting_for)
    );
    assert_no_entry_events(&events, row);
    assert_eq!(zone_of(runner, id), Zone::Battlefield, "{row}");
    assert_eq!(incarnation(runner, id), before, "{row}: the same object");
    assert_eq!(
        runner.state().stack.len(),
        stack,
        "{row}: no trigger is added"
    );
    assert_eq!(
        runner.state().resolution_stack.len(),
        resolution_stack,
        "{row}: nothing is parked"
    );
}

/// CR 400.6 + CR 603.6d + CR 614.12: Comet already on the battlefield, sent
/// onto the battlefield by Move to zone with simulate, is not moved from one
/// zone to another, so no event puts him onto the battlefield and his
/// CR 306.5b loyalty counters — an effect of that event — are not put on him
/// again. Doubling Season and Vorinclex modify counters being put on a
/// permanent (CR 122.6); none are, so nothing is doubled and there is no order
/// to choose (CR 616.1). His loyalty stays the count he had (CR 306.5c).
/// Paired positive: the same simulated move of a Comet card in the graveyard
/// enters him with five loyalty counters.
///
/// Red with the no-op guard reverted and the intrinsic seeding kept: the move
/// put another five loyalty counters on Comet (ten on Doubling Season's
/// board), and on the order-matters board it asked P0 to order the
/// replacements.
#[test]
fn moving_a_battlefield_planeswalker_onto_the_battlefield_changes_nothing() {
    type BoardSetup = fn(&mut GameScenario);
    // (board, its fixtures, run_etb, kind, loyalty)
    let rows: [(&str, BoardSetup, bool, DebugCardCreationKind, u32); 4] = [
        ("plain", plain_board, true, DebugCardCreationKind::Card, 5),
        ("plain", plain_board, true, DebugCardCreationKind::Token, 5),
        (
            "Doubling Season",
            with_doubling_season,
            true,
            DebugCardCreationKind::Card,
            10,
        ),
        (
            "order matters",
            with_order_matters,
            false,
            DebugCardCreationKind::Card,
            5,
        ),
    ];
    for (label, setup, run_etb, kind, loyalty) in rows {
        let row = format!("{label} board, run_etb={run_etb}, {kind:?}");
        let mut runner = board(setup);
        create(&mut runner, COMET, P0, Zone::Battlefield, run_etb, kind);
        let comet = only_object_named(&runner, COMET);
        assert_loyalty(&runner, comet, loyalty, &format!("{row}, before"));

        assert_battlefield_move_is_refused(&mut runner, comet, &row);

        assert!(
            runner.state().resolution_stack.is_empty(),
            "{row}: nothing is parked"
        );
        assert_eq!(only_object_named(&runner, COMET), comet, "{row}");
        assert_loyalty(&runner, comet, loyalty, &row);
    }

    // Paired positive: the same action shape enters a Comet card from the
    // graveyard.
    let mut runner = board(plain_board);
    create(
        &mut runner,
        COMET,
        P0,
        Zone::Graveyard,
        true,
        DebugCardCreationKind::Card,
    );
    let comet = only_object_named(&runner, COMET);
    assert_eq!(
        zone_of(&runner, comet),
        Zone::Graveyard,
        "positive: precondition"
    );
    let events = move_to(&mut runner, comet, Zone::Battlefield, true);
    assert!(
        events.iter().any(is_zone_changed),
        "positive: Comet enters: {events:?}"
    );
    assert!(
        events.iter().any(is_counter_added),
        "positive: his loyalty counters are put: {events:?}"
    );
    assert_eq!(zone_of(&runner, comet), Zone::Battlefield, "positive");
    assert_loyalty(&runner, comet, 5, "positive");
}

/// CR 603.6d + CR 614.12: an "enters with", "as enters" or "enters tapped"
/// ability applies as part of the event that puts the permanent onto the
/// battlefield. A permanent already on the battlefield, sent onto the
/// battlefield by Move to zone with simulate, is put there by no event, so
/// none of them applies again: Spike Feeder keeps its two +1/+1 counters,
/// History of Benalia keeps its one lore counter and its chapter II does not
/// trigger (CR 714.2b), Invasion of Amonkhet asks for no protector
/// (CR 310.9a + CR 310.12a), and an untapped Bloodfell Caves stays untapped.
/// Paired positives: each replacement applied when the consulting creation
/// entered the permanent. The refused move still checks state-based actions,
/// as every simulated move does: a raw-placed Spike Feeder (0/0, no counters)
/// gets no +1/+1 counter and is put into its owner's graveyard (CR 704.5f).
///
/// Red at base: the move re-applied every one of them — +1/+1 4, lore 2 with
/// chapter II on the stack, a second protector choice, Bloodfell Caves
/// tapped again, and two +1/+1 counters that kept the 0/0 Spike Feeder alive.
#[test]
fn moving_a_battlefield_permanent_onto_the_battlefield_reapplies_no_entry_replacement() {
    let created = |name: &str| {
        let mut runner = board(plain_board);
        create(
            &mut runner,
            name,
            P0,
            Zone::Battlefield,
            true,
            DebugCardCreationKind::Card,
        );
        runner
    };

    // Spike Feeder: "This creature enters with two +1/+1 counters on it."
    let mut runner = created(SPIKE_FEEDER);
    let feeder = only_object_named(&runner, SPIKE_FEEDER);
    assert_eq!(
        counter(&runner, feeder, CounterType::Plus1Plus1),
        Some(2),
        "Spike Feeder, before"
    );
    assert_battlefield_move_is_refused(&mut runner, feeder, "Spike Feeder");
    assert_eq!(
        counter(&runner, feeder, CounterType::Plus1Plus1),
        Some(2),
        "Spike Feeder: no counter is put on it"
    );

    // History of Benalia: CR 714.3a, a lore counter as it enters, and its
    // chapter I triggers.
    let mut runner = created(HISTORY_OF_BENALIA);
    let saga = only_object_named(&runner, HISTORY_OF_BENALIA);
    assert_eq!(
        counter(&runner, saga, CounterType::Lore),
        Some(1),
        "History of Benalia, before"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "History of Benalia, before: chapter I triggered"
    );
    assert_battlefield_move_is_refused(&mut runner, saga, "History of Benalia");
    assert_eq!(
        counter(&runner, saga, CounterType::Lore),
        Some(1),
        "History of Benalia: no lore counter is put on it"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "History of Benalia: CR 714.2b, chapter II does not trigger"
    );

    // Invasion of Amonkhet: the CR 614.12a protector choice as it enters.
    let mut runner = created(SIEGE);
    let choice = match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player, options, ..
        } => {
            assert_eq!(
                *player, P0,
                "Invasion of Amonkhet, before: its controller chooses its protector"
            );
            options
                .first()
                .cloned()
                .expect("an opponent can protect the Siege")
        }
        other => panic!(
            "Invasion of Amonkhet, before: expected the protector choice, got {}",
            short(other)
        ),
    };
    runner
        .act(GameAction::ChooseOption { choice })
        .expect("the protector choice is accepted");
    let siege = only_object_named(&runner, SIEGE);
    assert_defense(&runner, siege, 4, "Invasion of Amonkhet, before");
    assert_battlefield_move_is_refused(&mut runner, siege, "Invasion of Amonkhet");
    assert_defense(&runner, siege, 4, "Invasion of Amonkhet");

    // Bloodfell Caves: "This land enters tapped."
    let mut runner = created(BLOODFELL_CAVES);
    let caves = only_object_named(&runner, BLOODFELL_CAVES);
    assert!(
        runner.state().objects[&caves].tapped,
        "Bloodfell Caves, before: it entered tapped"
    );
    runner
        .act(GameAction::Debug(DebugAction::SetTapped {
            object_id: caves,
            tapped: false,
        }))
        .expect("the debug untap is accepted");
    assert!(
        !runner.state().objects[&caves].tapped,
        "Bloodfell Caves, before: untapped"
    );
    assert_battlefield_move_is_refused(&mut runner, caves, "Bloodfell Caves");
    assert!(
        !runner.state().objects[&caves].tapped,
        "Bloodfell Caves: it is not tapped again"
    );

    // A raw-placed Spike Feeder: its enters-with replacement was skipped, so it
    // is a 0/0 on the battlefield until state-based actions are checked.
    let row = "raw Spike Feeder";
    let mut runner = board(plain_board);
    create(
        &mut runner,
        SPIKE_FEEDER,
        P0,
        Zone::Battlefield,
        false,
        DebugCardCreationKind::Card,
    );
    let feeder = only_object_named(&runner, SPIKE_FEEDER);
    assert_eq!(
        zone_of(&runner, feeder),
        Zone::Battlefield,
        "{row}: precondition, the raw placement checks no state-based actions"
    );
    assert_eq!(
        counter(&runner, feeder, CounterType::Plus1Plus1),
        None,
        "{row}: precondition"
    );

    let events = move_to(&mut runner, feeder, Zone::Battlefield, true);

    assert!(
        !events.iter().any(is_counter_added),
        "{row}: no counter is put: {events:?}"
    );
    let feeder = only_object_named(&runner, SPIKE_FEEDER);
    assert_eq!(
        zone_of(&runner, feeder),
        Zone::Graveyard,
        "{row}: CR 704.5f, the 0/0 creature is put into its owner's graveyard"
    );
    assert!(
        is_priority(&runner),
        "{row}: {}",
        short(&runner.state().waiting_for)
    );
}

/// CR 111.8: a token that has left the battlefield can't come back onto it;
/// it remains in its zone and ceases to exist the next time state-based
/// actions are checked (CR 704.5d). Move to zone with simulate checks them, so
/// a token raw-moved to the graveyard and then sent onto the battlefield gets
/// no enters-with counter (no event puts it onto the battlefield, CR 603.6d)
/// and ceases to exist, exactly as when it is sent to a hand. Paired positive:
/// a Comet card set up the same way does enter, with five loyalty counters.
///
/// Red at base on the Spike Feeder row: the move put the token's entry
/// counters (+1/+1 2) on it in the graveyard before it ceased to exist. The
/// delivery tail's `entered_battlefield` gate now also keeps those counters
/// off, so reverting the no-op guard alone leaves these rows green; the guard
/// is pinned by the Battlefield -> Battlefield rows (its call) and by
/// `the_zone_pipeline_refuses_battlefield_to_battlefield_and_a_returning_token`
/// (its CR 111.8 half).
#[test]
fn a_token_that_left_the_battlefield_stays_out_of_the_simulated_entry() {
    // (token, the counter it enters with, how many)
    let tokens = [
        (COMET, CounterType::Loyalty, 5),
        (SPIKE_FEEDER, CounterType::Plus1Plus1, 2),
    ];
    for (name, counter_type, count) in tokens {
        for to_zone in [Zone::Battlefield, Zone::Hand] {
            let row = format!("{name} token, simulated move to {to_zone:?}");
            let mut runner = board(plain_board);
            create(
                &mut runner,
                name,
                P0,
                Zone::Battlefield,
                true,
                DebugCardCreationKind::Token,
            );
            let token = only_object_named(&runner, name);
            assert!(
                runner.state().objects[&token].is_token,
                "{row}: precondition"
            );
            assert_eq!(
                counter(&runner, token, counter_type.clone()),
                Some(count),
                "{row}: precondition, it entered with its counters"
            );

            move_to(&mut runner, token, Zone::Graveyard, false);
            let object = &runner.state().objects[&token];
            assert_eq!(
                object.zone,
                Zone::Graveyard,
                "{row}: precondition, the raw move checks no state-based actions"
            );
            assert!(
                object.counters.is_empty(),
                "{row}: precondition, CR 122.2: it left with no counters"
            );

            let events = move_to(&mut runner, token, to_zone, true);

            assert_no_entry_events(&events, &row);
            assert!(
                !runner.state().objects.contains_key(&token),
                "{row}: CR 111.8 + CR 704.5d, the token ceased to exist"
            );
            assert!(
                is_priority(&runner),
                "{row}: {}",
                short(&runner.state().waiting_for)
            );
        }
    }

    // Paired positive: the card twin enters from the graveyard.
    let row = "Comet card, simulated move to Battlefield";
    let mut runner = board(plain_board);
    create(
        &mut runner,
        COMET,
        P0,
        Zone::Battlefield,
        true,
        DebugCardCreationKind::Card,
    );
    let comet = only_object_named(&runner, COMET);
    assert_loyalty(&runner, comet, 5, &format!("{row}, before"));
    move_to(&mut runner, comet, Zone::Graveyard, false);
    assert_eq!(
        zone_of(&runner, comet),
        Zone::Graveyard,
        "{row}: precondition"
    );
    let events = move_to(&mut runner, comet, Zone::Battlefield, true);
    assert!(
        events.iter().any(is_zone_changed),
        "{row}: Comet enters: {events:?}"
    );
    assert_eq!(zone_of(&runner, comet), Zone::Battlefield, "{row}");
    assert_loyalty(&runner, comet, 5, row);
}

/// Move `id` onto the battlefield through the zone pipeline's own entry
/// (`ZoneMoveRequest::effect`), returning whether the move parked a choice and
/// its events.
fn pipeline_move_to_battlefield(runner: &mut GameRunner, id: ObjectId) -> (bool, Vec<GameEvent>) {
    let mut events = Vec::new();
    let parked = move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(id, Zone::Battlefield, id),
        &mut events,
    );
    (parked, events)
}

/// The zone pipeline, the no-op predicate's other caller, still refuses both
/// no-op moves before it consults any replacement: a permanent already on the
/// battlefield (CR 400.6 + CR 603.6d) gets no enters-with counter, and a token
/// in the graveyard stays there (CR 111.8). Paired positive: the same request
/// enters a Spike Feeder card from the graveyard with its two +1/+1 counters.
///
/// Preservation: base reads the same. Red if either half of the shared
/// predicate is dropped (Battlefield -> Battlefield: +1/+1 4 and loyalty 10;
/// CR 111.8: the replacement consult runs, so the event slice is not empty).
#[test]
fn the_zone_pipeline_refuses_battlefield_to_battlefield_and_a_returning_token() {
    for (name, counter_type, count) in [
        (SPIKE_FEEDER, CounterType::Plus1Plus1, 2),
        (COMET, CounterType::Loyalty, 5),
    ] {
        let row = format!("{name} on the battlefield");
        let mut runner = board(plain_board);
        create(
            &mut runner,
            name,
            P0,
            Zone::Battlefield,
            true,
            DebugCardCreationKind::Card,
        );
        let id = only_object_named(&runner, name);
        assert_eq!(
            counter(&runner, id, counter_type.clone()),
            Some(count),
            "{row}: before"
        );
        let before = incarnation(&runner, id);

        let (parked, events) = pipeline_move_to_battlefield(&mut runner, id);

        assert!(!parked, "{row}: nothing is asked");
        assert!(
            events.is_empty(),
            "{row}: refused before the consult: {events:?}"
        );
        assert_eq!(zone_of(&runner, id), Zone::Battlefield, "{row}");
        assert_eq!(incarnation(&runner, id), before, "{row}: the same object");
        assert_eq!(
            counter(&runner, id, counter_type),
            Some(count),
            "{row}: no counter is put on it"
        );
    }

    // CR 111.8: a token that has left the battlefield.
    let row = "Spike Feeder token in the graveyard";
    let mut runner = board(plain_board);
    create(
        &mut runner,
        SPIKE_FEEDER,
        P0,
        Zone::Battlefield,
        true,
        DebugCardCreationKind::Token,
    );
    let token = only_object_named(&runner, SPIKE_FEEDER);
    assert!(
        runner.state().objects[&token].is_token,
        "{row}: precondition"
    );
    move_to(&mut runner, token, Zone::Graveyard, false);
    assert_eq!(
        zone_of(&runner, token),
        Zone::Graveyard,
        "{row}: precondition"
    );

    let (parked, events) = pipeline_move_to_battlefield(&mut runner, token);

    assert!(!parked, "{row}: nothing is asked");
    assert!(
        events.is_empty(),
        "{row}: refused before the consult: {events:?}"
    );
    assert_eq!(
        zone_of(&runner, token),
        Zone::Graveyard,
        "{row}: it remains in its zone"
    );
    assert!(
        runner.state().objects[&token].counters.is_empty(),
        "{row}: no counter is put on it"
    );

    // Paired positive: a Spike Feeder card enters from the graveyard.
    let row = "Spike Feeder card in the graveyard";
    let mut runner = board(plain_board);
    create(
        &mut runner,
        SPIKE_FEEDER,
        P0,
        Zone::Graveyard,
        true,
        DebugCardCreationKind::Card,
    );
    let feeder = only_object_named(&runner, SPIKE_FEEDER);
    assert_eq!(
        zone_of(&runner, feeder),
        Zone::Graveyard,
        "{row}: precondition"
    );

    let (parked, events) = pipeline_move_to_battlefield(&mut runner, feeder);

    assert!(!parked, "{row}: nothing is asked");
    assert!(
        events.iter().any(is_zone_changed),
        "{row}: it enters: {events:?}"
    );
    assert_eq!(zone_of(&runner, feeder), Zone::Battlefield, "{row}");
    assert_eq!(
        counter(&runner, feeder, CounterType::Plus1Plus1),
        Some(2),
        "{row}: it enters with two +1/+1 counters"
    );
}

/// How a graveyard card is sent onto the battlefield.
#[derive(Clone, Copy, Debug)]
enum GraveyardEntry {
    /// `MoveToZone { simulate: true }`, the sandbox consulting entry.
    SimulatedMove,
    /// `MoveToZone { simulate: false }`, the sandbox raw placement.
    RawMove,
    /// `ZoneMoveRequest::effect`, the zone pipeline's effect-driven entry.
    Effect,
}

/// Send `id` onto the battlefield by `route`, returning the move's events.
fn send_to_battlefield(
    runner: &mut GameRunner,
    id: ObjectId,
    route: GraveyardEntry,
) -> Vec<GameEvent> {
    match route {
        GraveyardEntry::SimulatedMove => move_to(runner, id, Zone::Battlefield, true),
        GraveyardEntry::RawMove => move_to(runner, id, Zone::Battlefield, false),
        GraveyardEntry::Effect => {
            let (parked, events) = pipeline_move_to_battlefield(runner, id);
            assert!(!parked, "{route:?}: nothing is asked");
            events
        }
    }
}

/// CR 614.17c + CR 614.17d: an entry a "can't enter the battlefield" static
/// refuses is an event that can't happen, so no enters-with replacement
/// modifies it and the CR 306.5b loyalty counters are not put on the card it
/// leaves in the graveyard. Under P0's Weathered Runestone, a Comet card in
/// the graveyard sent onto the battlefield stays there with no loyalty counter,
/// on the sandbox simulated move, the sandbox raw move and the zone pipeline's
/// effect-driven entry. Paired positive on each route: the same send without
/// the Runestone enters Comet with five loyalty counters.
///
/// Red with the delivery tail's counter block gated on the destination rather
/// than on the entry: the simulated and effect rows then put Loyalty 5 (and a
/// `CounterAdded`) on the graveyard card. The raw row is a preservation
/// sibling: the DebugCommand arm installs only on an arrival.
#[test]
fn a_refused_entry_puts_no_loyalty_counters_on_the_card() {
    for route in [
        GraveyardEntry::SimulatedMove,
        GraveyardEntry::RawMove,
        GraveyardEntry::Effect,
    ] {
        for runestone in [false, true] {
            let row = format!("{route:?}, Weathered Runestone={runestone}");
            let mut runner = board(|scenario| {
                if runestone {
                    with_weathered_runestone(scenario);
                }
            });
            create(
                &mut runner,
                COMET,
                P0,
                Zone::Graveyard,
                true,
                DebugCardCreationKind::Card,
            );
            let comet = only_object_named(&runner, COMET);
            assert_eq!(
                zone_of(&runner, comet),
                Zone::Graveyard,
                "{row}: precondition"
            );
            assert_eq!(
                counter(&runner, comet, CounterType::Loyalty),
                None,
                "{row}: precondition, no counters in the graveyard"
            );

            let events = send_to_battlefield(&mut runner, comet, route);

            if runestone {
                assert_eq!(
                    zone_of(&runner, comet),
                    Zone::Graveyard,
                    "{row}: the Runestone refuses the entry; Comet stays in the graveyard"
                );
                assert_no_entry_events(&events, &row);
                assert_eq!(
                    counter(&runner, comet, CounterType::Loyalty),
                    None,
                    "{row}: no loyalty counter is put on the graveyard card"
                );
            } else {
                assert!(
                    events.iter().any(is_zone_changed),
                    "{row}: Comet enters: {events:?}"
                );
                assert_eq!(zone_of(&runner, comet), Zone::Battlefield, "{row}");
                assert_loyalty(&runner, comet, 5, &row);
            }
            assert!(
                is_priority(&runner),
                "{row}: {}",
                short(&runner.state().waiting_for)
            );
        }
    }
}
