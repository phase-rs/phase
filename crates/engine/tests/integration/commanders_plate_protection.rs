//! Commander's Plate: "Equipped creature gets +3/+3 and has protection from each
//! color that's not in your commander's color identity. / Equip commander {3} /
//! Equip {5}".
//!
//! CR 702.16i: "protection from each [set]" is shorthand for one separate
//! protection ability per member of the set. The set here is the colors in
//! `ManaColor::ALL` that are not in the Plate controller's commander color
//! identity (CR 903.4 + CR 702.124c for partners), and it is undefined, so the
//! grant does nothing, when that player has no commander (CR 903.4f). "Your"
//! is the Plate's controller (CR 109.5), re-evaluated on every layer pass
//! (CR 611.3a; R5b re-equips, R10b changes the equipped creature's controller).
//!
//! The Plate's own behavior goes through production entry points: Equip is
//! activated with `GameRunner::activate`, sweepers are cast with
//! `GameRunner::cast`, and targeting legality is read from `find_legal_targets`
//! (a protected target is never offered, and a lone legal target would
//! auto-resolve in casting, so `WaitingFor::TargetSelection` cannot show it).
//! Attachment moves (R10b's Control Magic, R15's CR 701.3b leg) call the attach
//! primitive `attach_to` directly. Every negative assertion
//! ("the protected Bear is not a legal target") is paired with a positive
//! reach guard on the same source (the unprotected Foe, or an unprotected
//! color's source, IS a legal target).
//!
//! Fixture note: `GameScenario::with_commander` moves the object to the command
//! zone, which still feeds the color identity (R5-R7, R10, R13-R16) but cannot be an Equip
//! target, so rows that need a BATTLEFIELD commander (R9a, R9b, R9c, R11; R9b'
//! has only a registered pool and no commander object) mark it in place with
//! `CardBuilder::commander()`.

use engine::game::deck_loading::DeckEntry;
use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{ChosenAttribute, TargetFilter, TargetRef, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::game_state::{GameState, PlayerDeckPool, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::sync::Arc;

use ManaColor::{Black, Blue, Green, Red, White};

/// Verbatim Oracle text (MTGJSON AtomicCards).
const PLATE: &str = "Equipped creature gets +3/+3 and has protection from each color that's not in your commander's color identity.\nEquip commander {3}\nEquip {5}";
const FLICKERING_WARD: &str = "Enchant creature\nAs this Aura enters, choose a color.\nEnchanted creature has protection from the chosen color. This effect doesn't remove this Aura.\n{W}: Return this Aura to its owner's hand.";
const PACIFISM: &str = "Enchant creature\nEnchanted creature can't attack or block.";
const DARKEST_HOUR: &str = "All creatures are black.";
const CONTROL_MAGIC: &str = "Enchant creature\nYou control enchanted creature.";

const EQUIP_COMMANDER: &str = "Equip commander {3}";
const EQUIP: &str = "Equip {5}";

fn color_word(color: ManaColor) -> &'static str {
    match color {
        White => "White",
        Blue => "Blue",
        Black => "Black",
        Red => "Red",
        Green => "Green",
    }
}

/// A two-player Commander-style board at precombat main: P0 owns the Plate and
/// Bear (1/10); P1 owns Foe (10/10) plus, in hand, five colored one-damage
/// "zaps" (used only as colored sources for `find_legal_targets`) and five
/// colored one-damage sweepers (cast through `GameRunner::cast`).
struct Board {
    runner: GameRunner,
    plate: ObjectId,
    bear: ObjectId,
    foe: ObjectId,
    zaps: [ObjectId; 5],
    sweeps: [ObjectId; 5],
}

impl Board {
    fn new(setup: impl FnOnce(&mut GameScenario)) -> Self {
        Self::with_plate_controller(P0, setup)
    }

    /// `controller` controls the Plate while P0 keeps owning it (CR 301.5d:
    /// the Equipment's controller is independent of the equipped creature's).
    fn with_plate_controller(controller: PlayerId, setup: impl FnOnce(&mut GameScenario)) -> Self {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let plate = {
            let mut builder = scenario.add_artifact_from_oracle(P0, "Commander's Plate", PLATE);
            builder.with_subtypes(vec!["Equipment"]);
            if controller != P0 {
                builder.controlled_by(controller);
            }
            builder.id()
        };
        let bear = scenario.add_creature(P0, "Bear", 1, 10).id();
        let foe = scenario.add_creature(P1, "Foe", 10, 10).id();
        let zaps = ManaColor::ALL.map(|color| {
            let name = format!("{} Zap", color_word(color));
            scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    &name,
                    false,
                    &format!("{name} deals 1 damage to target creature."),
                )
                .with_color(vec![color])
                .id()
        });
        let sweeps = ManaColor::ALL.map(|color| {
            let name = format!("{} Sweep", color_word(color));
            scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    &name,
                    false,
                    &format!("{name} deals 1 damage to each creature."),
                )
                .with_color(vec![color])
                .id()
        });
        setup(&mut scenario);
        let mut runner = scenario.build();
        fund(&mut runner, controller, 5);
        Board {
            runner,
            plate,
            bear,
            foe,
            zaps,
            sweeps,
        }
    }

    fn state(&self) -> &GameState {
        self.runner.state()
    }
}

fn fund(runner: &mut GameRunner, player: PlayerId, amount: usize) {
    for _ in 0..amount {
        let _ = runner.state_mut().add_mana_to_pool(
            player,
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        );
    }
}

/// Force a full layer re-derive (CR 613) before reading effective characteristics.
fn refresh(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

/// Fixture: make `player` the active, priority-holding player so they can
/// activate an Equip (CR 702.6a, sorcery speed) or cast a sorcery (CR 307.1).
fn give_turn_to(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn ability_index(runner: &GameRunner, source: ObjectId, description: &str) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| a.description.as_deref() == Some(description))
        .unwrap_or_else(|| panic!("{source:?} must expose the ability '{description}'"))
}

/// Activate the Plate's Equip ability (by description) onto `host` through the
/// production activation pipeline and assert it attached (CR 702.6a).
fn equip(runner: &mut GameRunner, plate: ObjectId, description: &str, host: ObjectId) {
    let index = ability_index(runner, plate, description);
    let outcome = runner.activate(plate, index).target_object(host).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "equip must finish at priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        runner.state().objects[&plate].attached_to,
        Some(AttachTarget::Object(host)),
        "{description} must attach the Plate to the chosen host"
    );
    assert_eq!(
        runner.state().stack.len(),
        0,
        "the equip ability must have fully resolved"
    );
    refresh(runner);
}

/// Every `Protection` keyword on `id`, as the concrete qualities the layer
/// applier produced.
fn protection_keywords(state: &GameState, id: ObjectId) -> Vec<ProtectionTarget> {
    state.objects[&id]
        .keywords
        .iter()
        .filter_map(|keyword| match keyword {
            Keyword::Protection(quality) => Some(quality.clone()),
            _ => None,
        })
        .collect()
}

/// The colors `id` has protection from, in WUBRG order. Panics if any
/// protection keyword is not an expanded `Color` quality: CR 702.16i
/// expands the set into one `Color` ability per member, so an unexpanded set
/// (or the old untyped `CardType` string) on a recipient is itself a defect.
fn protected_colors(state: &GameState, id: ObjectId) -> Vec<ManaColor> {
    let qualities = protection_keywords(state, id);
    let colors: Vec<ManaColor> = ManaColor::ALL
        .iter()
        .copied()
        .filter(|color| qualities.contains(&ProtectionTarget::Color(*color)))
        .collect();
    assert_eq!(
        qualities.len(),
        colors.len(),
        "every protection keyword must be an expanded Color quality, got {qualities:?}"
    );
    colors
}

fn colors_except(identity: &[ManaColor]) -> Vec<ManaColor> {
    ManaColor::ALL
        .iter()
        .copied()
        .filter(|color| !identity.contains(color))
        .collect()
}

fn creature_targets(state: &GameState, source: ObjectId) -> Vec<TargetRef> {
    find_legal_targets(
        state,
        &TargetFilter::Typed(TypedFilter::creature()),
        P1,
        source,
    )
}

/// CR 702.16b: for each color, a P1 source of that color may target Bear iff
/// Bear is not protected from that color. The Foe is a legal target for every
/// source (positive reach guard: the source can target creatures at all).
fn assert_zap_legality(board: &Board, protected: &[ManaColor]) {
    for (zap, color) in board.zaps.iter().zip(ManaColor::ALL) {
        let targets = creature_targets(board.state(), *zap);
        assert!(
            targets.contains(&TargetRef::Object(board.foe)),
            "reach guard: the unprotected Foe must be a legal target of the {color:?} zap, got {targets:?}"
        );
        let bear_legal = targets.contains(&TargetRef::Object(board.bear));
        if protected.contains(&color) {
            assert!(
                !bear_legal,
                "CR 702.16b: Bear is protected from {color:?} and must not be targetable by the {color:?} zap, got {targets:?}"
            );
        } else {
            assert!(
                bear_legal,
                "Bear is NOT protected from {color:?} and must be targetable by the {color:?} zap, got {targets:?}"
            );
        }
    }
}

/// CR 702.16e: cast each colored sweeper from P1 (through the production cast
/// pipeline) and compare each one's damage DELTA: protected colors deal nothing
/// to Bear, every sweeper deals 1 to the Foe (reach guard), and unprotected
/// colors deal 1 to Bear. Marked damage accumulates, so only deltas are compared.
fn assert_sweep_damage(board: &mut Board, protected: &[ManaColor]) {
    give_turn_to(&mut board.runner, P1);
    for (sweep, color) in board.sweeps.into_iter().zip(ManaColor::ALL) {
        let bear_before = board.state().objects[&board.bear].damage_marked;
        let foe_before = board.state().objects[&board.foe].damage_marked;
        let outcome = board.runner.cast(sweep).resolve();
        assert!(
            matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
            "the {color:?} sweep must finish at priority, got {:?}",
            outcome.final_waiting_for()
        );
        let bear_delta = board.state().objects[&board.bear].damage_marked - bear_before;
        let foe_delta = board.state().objects[&board.foe].damage_marked - foe_before;
        assert_eq!(
            foe_delta, 1,
            "reach guard: the {color:?} sweep must deal 1 damage to the unprotected Foe"
        );
        let expected = if protected.contains(&color) { 0 } else { 1 };
        assert_eq!(
            bear_delta,
            expected,
            "CR 702.16e: the {color:?} sweep's damage to Bear (protected: {})",
            protected.contains(&color)
        );
    }
}

fn assert_plus_three(board: &Board) {
    let bear = &board.state().objects[&board.bear];
    assert_eq!(
        (bear.power, bear.toughness),
        (Some(4), Some(13)),
        "the Plate's +3/+3 must apply (1/10 -> 4/13)"
    );
}

/// Add a colored commander creature owned by P0 and move it to the command
/// zone (it still feeds the color identity, CR 903.4).
fn add_command_zone_commander(scenario: &mut GameScenario, name: &str, colors: Vec<ManaColor>) {
    let id = scenario
        .add_creature(P0, name, 2, 2)
        .with_color(colors)
        .id();
    scenario.with_commander(id);
}

/// A registered deck pool commander with an explicit color identity and no
/// live commander object (CR 903.4a: the identity established as the game
/// began is authoritative).
fn register_pool_commander(runner: &mut GameRunner, player: PlayerId, identity: Vec<ManaColor>) {
    runner.state_mut().deck_pools.push(PlayerDeckPool {
        player,
        current_commander: Arc::new(vec![DeckEntry {
            card: CardFace {
                name: "Registered Commander".to_string(),
                color_identity: identity,
                ..Default::default()
            },
            count: 1,
        }]),
        ..Default::default()
    });
}

fn attach_fixture(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    state.objects.get_mut(&attachment).unwrap().attached_to = Some(AttachTarget::Object(host));
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(attachment);
    state.layers_dirty.mark_full();
}

// ---------------------------------------------------------------------------
// R5 / R5b: one commander
// ---------------------------------------------------------------------------

/// R5. CR 702.16i + CR 903.4: a {U,R} commander gives the equipped creature
/// protection from exactly white, black and green; +3/+3 applies.
#[test]
fn one_commander_gives_protection_from_each_color_outside_its_identity() {
    let mut board =
        Board::new(|s| add_command_zone_commander(s, "Izzet Commander", vec![Blue, Red]));
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_plus_three(&board);
    assert_eq!(
        protected_colors(board.state(), board.bear),
        vec![White, Black, Green],
        "CR 702.16i: one protection ability per color outside {{U,R}}"
    );
    assert_zap_legality(&board, &[White, Black, Green]);
    assert_sweep_damage(&mut board, &[White, Black, Green]);
}

/// R5b. CR 611.3a: the grant is not locked in. Re-equipping moves the
/// protection to the new host and the old host loses it.
#[test]
fn re_equipping_moves_the_protection_to_the_new_host() {
    let mut scenario_wolf = None;
    let mut board = Board::new(|s| {
        add_command_zone_commander(s, "Izzet Commander", vec![Blue, Red]);
        scenario_wolf = Some(s.add_creature(P0, "Wolf", 1, 10).id());
    });
    let wolf = scenario_wolf.unwrap();
    equip(&mut board.runner, board.plate, EQUIP, board.bear);
    assert_eq!(
        protected_colors(board.state(), board.bear),
        vec![White, Black, Green],
        "reach guard: the first host is protected"
    );

    fund(&mut board.runner, P0, 5);
    equip(&mut board.runner, board.plate, EQUIP, wolf);
    assert_eq!(
        protected_colors(board.state(), wolf),
        vec![White, Black, Green],
        "the new host gains the protection"
    );
    assert!(
        protection_keywords(board.state(), board.bear).is_empty(),
        "CR 611.3a: the previous host no longer has the protection"
    );
    let bear = &board.state().objects[&board.bear];
    assert_eq!(
        (bear.power, bear.toughness),
        (Some(1), Some(10)),
        "the previous host loses the +3/+3"
    );
}

// ---------------------------------------------------------------------------
// R6 / R7 / R8: identity cardinality
// ---------------------------------------------------------------------------

/// R6. CR 702.124c: partner commanders combine their identities, so {W} + {B}
/// leaves protection from blue, red and green.
#[test]
fn partner_commanders_combine_identities() {
    let mut board = Board::new(|s| {
        add_command_zone_commander(s, "White Partner", vec![White]);
        add_command_zone_commander(s, "Black Partner", vec![Black]);
    });
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_eq!(
        protected_colors(board.state(), board.bear),
        colors_except(&[White, Black]),
        "CR 702.124c: combined identity {{W,B}}"
    );
    assert_eq!(
        protected_colors(board.state(), board.bear),
        vec![Blue, Red, Green]
    );
    assert_zap_legality(&board, &[Blue, Red, Green]);
}

/// R7. CR 903.4: a colorless commander has a defined, empty identity, so the
/// Plate gives protection from all five colors (per its 2020-11-10 ruling).
#[test]
fn colorless_commander_gives_protection_from_all_five_colors() {
    let mut board = Board::new(|s| add_command_zone_commander(s, "Colorless Commander", vec![]));
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_plus_three(&board);
    assert_eq!(
        protected_colors(board.state(), board.bear),
        ManaColor::ALL.to_vec()
    );
    assert_zap_legality(&board, &ManaColor::ALL);
    assert_sweep_damage(&mut board, &ManaColor::ALL);
}

/// R8. PRESERVATION / reach guard. CR 903.4f: with no commander the quality is
/// undefined and "that part of the ability won't do anything": no protection,
/// but the +3/+3 still applies (2020-11-10 ruling). Its +3/+3 and targeting legs
/// also hold before the fix; only the keyword-count leg differs (the pre-fix inert
/// CardType keyword). It proves the +3/+3 and Equip are intact and that the
/// no-commander case does not over-grant protection from all five.
#[test]
fn no_commander_grants_the_pump_but_no_protection() {
    let mut board = Board::new(|_| {});
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_plus_three(&board);
    assert!(
        protection_keywords(board.state(), board.bear).is_empty(),
        "CR 903.4f: no commander -> no protection abilities, got {:?}",
        protection_keywords(board.state(), board.bear)
    );
    assert_zap_legality(&board, &[]);
    let white_zap_targets = creature_targets(board.state(), board.zaps[0]);
    assert!(
        white_zap_targets.contains(&TargetRef::Object(board.bear)),
        "the equipped Bear stays targetable by a white source"
    );
}

/// R8b. CR 903.4f in the shape production uses: deck loading registers a
/// `PlayerDeckPool` for EVERY seat, so in a game with no commanders each pool
/// exists but its `current_commander` is empty. That is still "no commander"
/// (the quality is undefined), not a colorless commander: the Plate pumps and
/// grants no protection. Discriminates the non-empty-pool guard in
/// `defined_commander_color_identity`; dropping it would read the empty pool as a
/// defined empty identity and protect the Bear from all five colors.
#[test]
fn no_commander_with_empty_registered_pools_grants_the_pump_but_no_protection() {
    let mut board = Board::new(|_| {});
    for player in [P0, P1] {
        board.runner.state_mut().deck_pools.push(PlayerDeckPool {
            player,
            ..Default::default()
        });
    }
    assert!(
        board
            .state()
            .deck_pools
            .iter()
            .all(|pool| pool.current_commander.is_empty()),
        "premise: pools are registered but hold no commander"
    );
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_plus_three(&board);
    assert!(
        protection_keywords(board.state(), board.bear).is_empty(),
        "CR 903.4f: empty registered pool -> no commander -> no protection, got {:?}",
        protection_keywords(board.state(), board.bear)
    );
    assert_zap_legality(&board, &[]);
}

// ---------------------------------------------------------------------------
// R9: identity authority (pool vs painted object)
// ---------------------------------------------------------------------------

/// Darkest Hour (layer 5, CR 613.1e) paints every creature black, including a
/// commander on the battlefield.
fn add_darkest_hour(scenario: &mut GameScenario) {
    scenario.add_enchantment_from_oracle(P1, "Darkest Hour", DARKEST_HOUR);
}

/// R9a. CR 903.4a: the registered pool identity {U,R} wins over a battlefield
/// commander painted black by Darkest Hour. Black stays PROTECTED-from (black
/// is outside {U,R}); the painted object must not add black to the identity.
#[test]
fn registered_identity_wins_over_a_painted_battlefield_commander() {
    let mut commander = None;
    let mut board = Board::new(|s| {
        add_darkest_hour(s);
        commander = Some(
            s.add_creature(P0, "Painted Commander", 2, 2)
                .commander()
                .id(),
        );
    });
    let commander = commander.unwrap();
    register_pool_commander(&mut board.runner, P0, vec![Blue, Red]);
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    let painted = &board.state().objects[&commander];
    assert!(
        painted.is_commander && painted.zone == Zone::Battlefield,
        "premise: battlefield commander"
    );
    assert_eq!(
        painted.color,
        vec![Black],
        "reach guard: Darkest Hour (a real layer-5 effect) painted the commander black"
    );
    assert_eq!(
        protected_colors(board.state(), board.bear),
        vec![White, Black, Green]
    );
    assert_zap_legality(&board, &[White, Black, Green]);
}

/// R9b. CR 903.4a: a registered COLORLESS commander (empty pool identity)
/// painted black on the battlefield still has an empty identity, so the Plate
/// gives protection from all five colors including black. Fails only if BOTH the
/// pool branch's defined-empty return and the printed-color fallback are
/// reverted; each alone is discriminated by R9b' (pool branch) and R9c
/// (printed-color fallback).
#[test]
fn registered_colorless_commander_painted_black_still_protects_from_black() {
    let mut commander = None;
    let mut board = Board::new(|s| {
        add_darkest_hour(s);
        commander = Some(s.add_creature(P0, "Painted Kozilek", 2, 2).commander().id());
    });
    let commander = commander.unwrap();
    register_pool_commander(&mut board.runner, P0, vec![]);
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_eq!(
        board.state().objects[&commander].color,
        vec![Black],
        "reach guard: the commander really is painted black"
    );
    assert_eq!(
        protected_colors(board.state(), board.bear),
        ManaColor::ALL.to_vec()
    );
    assert_zap_legality(&board, &ManaColor::ALL);
}

/// R9b'. CR 903.4a + CR 903.4f: a registered colorless commander with NO live
/// commander object is a defined, empty identity (not "no commander"), so the
/// Plate gives protection from all five. Discriminates the pool branch of the
/// identity authority: without it the lookup finds no commander and the
/// quality would be undefined.
#[test]
fn registered_colorless_commander_without_an_object_protects_from_all_five() {
    let mut board = Board::new(|_| {});
    register_pool_commander(&mut board.runner, P0, vec![]);
    assert!(
        !board
            .state()
            .objects
            .values()
            .any(|obj| obj.is_commander && obj.owner == P0),
        "premise: no commander OBJECT exists, only the pool registration"
    );
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_plus_three(&board);
    assert_eq!(
        protected_colors(board.state(), board.bear),
        ManaColor::ALL.to_vec()
    );
    assert_zap_legality(&board, &ManaColor::ALL);
}

/// R9c. CR 903.4a + CR 613.1e: with no pool, a colorless commander on the
/// battlefield painted black by Darkest Hour still has an empty identity (the
/// fallback reads the printed `base_color`, never the layer-5 result), so the
/// Plate gives protection from all five colors including black.
#[test]
fn poolless_painted_colorless_commander_protects_from_black() {
    let mut commander = None;
    let mut board = Board::new(|s| {
        add_darkest_hour(s);
        commander = Some(s.add_creature(P0, "Painted Kozilek", 2, 2).commander().id());
    });
    let commander = commander.unwrap();
    assert!(
        board
            .state()
            .deck_pools
            .iter()
            .all(|pool| pool.player != P0 || pool.current_commander.is_empty()),
        "premise: no registered commander pool"
    );
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    let painted = &board.state().objects[&commander];
    assert!(
        painted.is_commander && painted.zone == Zone::Battlefield && painted.base_color.is_empty(),
        "premise: battlefield commander with a printed colorless identity"
    );
    assert_eq!(
        painted.color,
        vec![Black],
        "reach guard: Darkest Hour (a real layer-5 effect) painted it black"
    );
    assert_eq!(
        protected_colors(board.state(), board.bear),
        ManaColor::ALL.to_vec()
    );
    assert_zap_legality(&board, &ManaColor::ALL);
}

// ---------------------------------------------------------------------------
// R10: "your" is the Plate's controller
// ---------------------------------------------------------------------------

/// R10a. CR 109.5: "your commander's color identity" is the Plate controller's.
/// P0 owns the Plate, P1 controls it and equips P1's Foe. P0's commander is
/// {U,R}; P1's is {W}. The Foe is protected from the colors outside {W}.
#[test]
fn your_is_the_plate_controller_not_its_owner() {
    let mut board = Board::with_plate_controller(P1, |s| {
        add_command_zone_commander(s, "P0 Izzet Commander", vec![Blue, Red]);
        let p1_commander = s
            .add_creature(P1, "P1 White Commander", 2, 2)
            .with_color(vec![White])
            .id();
        s.with_commander(p1_commander);
    });
    assert_eq!(
        board.state().objects[&board.plate].owner,
        P0,
        "premise: P0 owns the Plate"
    );
    assert_eq!(
        board.state().objects[&board.plate].controller,
        P1,
        "premise: P1 controls the Plate"
    );
    give_turn_to(&mut board.runner, P1);
    equip(&mut board.runner, board.plate, EQUIP, board.foe);

    assert_eq!(
        protected_colors(board.state(), board.foe),
        colors_except(&[White]),
        "CR 109.5: 'your' is the Plate's controller (P1, identity {{W}}), not its owner (P0, {{U,R}})"
    );
    assert_eq!(
        protected_colors(board.state(), board.foe),
        vec![Blue, Black, Red, Green]
    );
}

/// R10b. CR 301.5d + CR 611.3a: the Plate stays under P0's control when the
/// equipped creature changes controllers, so "your" remains P0 (identity
/// {U,R}), not the creature's new controller P1 (identity {W}). The control
/// change is a real layer-2 effect (Control Magic).
#[test]
fn your_stays_the_plate_controller_when_the_equipped_creature_is_stolen() {
    let mut control_magic = None;
    let mut board = Board::new(|s| {
        add_command_zone_commander(s, "P0 Izzet Commander", vec![Blue, Red]);
        let p1_commander = s
            .add_creature(P1, "P1 White Commander", 2, 2)
            .with_color(vec![White])
            .id();
        s.with_commander(p1_commander);
        control_magic = Some(
            s.add_enchantment_from_oracle(P1, "Control Magic", CONTROL_MAGIC)
                .with_subtypes(vec!["Aura"])
                .with_color(vec![Blue])
                .id(),
        );
    });
    let control_magic = control_magic.unwrap();
    // Fixture: an Aura must be attached to survive SBAs (CR 704.5m), so Control
    // Magic starts on P1's own Foe (a no-op control change) and is moved later.
    attach_fixture(&mut board.runner, control_magic, board.foe);
    equip(&mut board.runner, board.plate, EQUIP, board.bear);
    assert_eq!(
        protected_colors(board.state(), board.bear),
        vec![White, Black, Green],
        "reach guard: protection before the control change"
    );

    // Control Magic is blue; Bear is protected from W, B, G only, so it can attach.
    attach_to(board.runner.state_mut(), control_magic, board.bear);
    refresh(&mut board.runner);

    let state = board.state();
    assert_eq!(
        state.objects[&board.bear].controller, P1,
        "reach guard: Control Magic (layer 2) moved Bear to P1"
    );
    assert_eq!(
        state.objects[&board.plate].controller, P0,
        "CR 301.5d: changing control of the creature does not change control of the Equipment"
    );
    assert_eq!(
        state.objects[&board.plate].attached_to,
        Some(AttachTarget::Object(board.bear)),
        "the Plate stays attached"
    );
    assert_eq!(
        protected_colors(state, board.bear),
        vec![White, Black, Green],
        "CR 109.5: 'your' stays the Plate's controller P0, not the new controller P1"
    );
}

// ---------------------------------------------------------------------------
// R11: Equip commander targets (PRESERVATION)
// ---------------------------------------------------------------------------

struct CommanderBoard {
    board: Board,
    own: ObjectId,
    stolen: ObjectId,
    opposing: ObjectId,
}

fn commander_board() -> CommanderBoard {
    let mut ids = None;
    let mut board = Board::new(|s| {
        let own = s.add_creature(P0, "Own Commander", 2, 2).commander().id();
        let stolen = s
            .add_creature(P1, "Stolen Commander", 2, 2)
            .commander()
            .controlled_by(P0)
            .id();
        let opposing = s
            .add_creature(P1, "Opposing Commander", 2, 2)
            .commander()
            .id();
        ids = Some((own, stolen, opposing));
    });
    let (own, stolen, opposing) = ids.unwrap();
    fund(&mut board.runner, P0, 5);
    // Assert zone and controller of each commander BEFORE querying targets.
    for (id, controller) in [(own, P0), (stolen, P0), (opposing, P1)] {
        let obj = &board.state().objects[&id];
        assert!(obj.is_commander, "{id:?} is a commander");
        assert_eq!(obj.zone, Zone::Battlefield, "{id:?} is on the battlefield");
        assert_eq!(obj.controller, controller, "{id:?} controller");
    }
    assert_eq!(board.state().objects[&stolen].owner, P1);
    CommanderBoard {
        board,
        own,
        stolen,
        opposing,
    }
}

/// Activate `description` and return the legal targets of its first target
/// slot (the Equip target prompt), leaving the prompt unanswered.
fn equip_prompt_targets(board: &mut Board, description: &str) -> Vec<ObjectId> {
    let index = ability_index(&board.runner, board.plate, description);
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: board.plate,
            ability_index: index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .unwrap_or_else(|e| panic!("{description} must be activatable: {e:?}"));
    let WaitingFor::TargetSelection { target_slots, .. } = &board.state().waiting_for else {
        panic!(
            "{description} with several legal hosts must prompt for a target, got {:?}",
            board.state().waiting_for
        );
    };
    let mut ids: Vec<ObjectId> = target_slots[0]
        .legal_targets
        .iter()
        .map(|target| match target {
            TargetRef::Object(id) => *id,
            other => panic!("Equip targets objects, got {other:?}"),
        })
        .collect();
    ids.sort();
    ids
}

/// R11. CR 702.6c: "Equip commander {3}" may target only a creature you control
/// that is a commander (any owner: a stolen commander qualifies); "Equip {5}"
/// may target any creature you control. PRESERVATION row: no engine change.
#[test]
fn equip_commander_targets_only_commanders_you_control() {
    let CommanderBoard {
        mut board,
        own,
        stolen,
        ..
    } = commander_board();
    let mut expected = vec![own, stolen];
    expected.sort();
    assert_eq!(
        equip_prompt_targets(&mut board, EQUIP_COMMANDER),
        expected,
        "CR 702.6c: Equip commander {{3}} targets only your commanders (own + stolen)"
    );

    let CommanderBoard {
        board: mut board_any,
        own,
        stolen,
        opposing,
    } = commander_board();
    let mut expected = vec![own, stolen, board_any.bear];
    expected.sort();
    let targets = equip_prompt_targets(&mut board_any, EQUIP);
    assert_eq!(
        targets, expected,
        "reach guard: Equip {{5}} also offers non-commander Bear"
    );
    assert!(
        !targets.contains(&opposing) && !targets.contains(&board_any.foe),
        "creatures an opponent controls are never Equip targets (CR 702.6a)"
    );
}

/// R11 (completion). Equip commander {3} resolves onto a commander through the
/// production activation pipeline.
#[test]
fn equip_commander_completes_onto_a_commander() {
    let CommanderBoard {
        mut board, stolen, ..
    } = commander_board();
    equip(&mut board.runner, board.plate, EQUIP_COMMANDER, stolen);
    assert!(
        board.state().objects[&stolen]
            .attachments
            .contains(&board.plate),
        "the stolen commander is equipped"
    );
}

// ---------------------------------------------------------------------------
// R13 / R14 / R15 / R16: attachment legality (CR 702.16c/d/n, CR 704.5m)
// ---------------------------------------------------------------------------

struct WardBoard {
    board: Board,
    ward: ObjectId,
}

/// A white Flickering Ward attached to Bear with `chosen` as its chosen color,
/// before the Plate is equipped. P0's commander has `identity`.
fn ward_board(identity: Vec<ManaColor>, chosen: ManaColor) -> WardBoard {
    let mut ward = None;
    let mut board = Board::new(|s| {
        add_command_zone_commander(s, "Commander", identity);
        ward = Some(
            s.add_enchantment_from_oracle(P0, "Flickering Ward", FLICKERING_WARD)
                .with_subtypes(vec!["Aura"])
                .with_color(vec![White])
                .id(),
        );
    });
    let ward = ward.unwrap();
    attach_fixture(&mut board.runner, ward, board.bear);
    board
        .runner
        .state_mut()
        .objects
        .get_mut(&ward)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(chosen));
    refresh(&mut board.runner);
    // Reach guard: before the equip the Ward is attached and stays on the
    // battlefield through SBAs, whatever color it chose (CR 702.16n for white).
    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(board.runner.state_mut(), &mut events);
    assert_eq!(board.state().objects[&ward].zone, Zone::Battlefield);
    assert_eq!(
        board.state().objects[&ward].attached_to,
        Some(AttachTarget::Object(board.bear))
    );
    WardBoard { board, ward }
}

/// R13. CR 702.16c + CR 702.16n + CR 704.5m: Ward's own protection grant has a
/// "doesn't remove this Aura" rider, which exempts the Aura only from THAT
/// ability. The Plate's separate protection-from-white ability (CR 702.16i) is
/// not exempted, so the white Aura is put into its owner's graveyard when the
/// Plate is equipped. Discriminates attachment legality resolving the Plate's
/// set grant (the legality scan reads grant definitions, not `obj.keywords`).
#[test]
fn plates_protection_removes_a_white_aura_despite_another_grants_rider() {
    let WardBoard { mut board, ward } = ward_board(vec![Blue, Red], White);
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_eq!(
        protected_colors(board.state(), board.bear),
        vec![White, Black, Green],
        "reach guard: the Plate granted protection from white"
    );
    assert_eq!(
        board.state().objects[&ward].zone,
        Zone::Graveyard,
        "CR 704.5m + CR 702.16c: the white Aura is now attached to a creature with protection from white"
    );
    assert_eq!(board.state().objects[&ward].owner, P0);
}

/// R14. Ward chose blue, so its own grant does not match the white Aura; the
/// Plate's expanded protection-from-white grant (resolved from its static
/// definition by attachment legality) removes it. With attachment legality's set
/// expansion reverted, the `obj.keywords` fallback still removes it, so this row
/// is a preservation guard, not a revert discriminator.
#[test]
fn plates_protection_removes_a_white_aura_when_ward_chose_another_color() {
    let WardBoard { mut board, ward } = ward_board(vec![Blue, Red], Blue);
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_eq!(
        protected_colors(board.state(), board.bear),
        vec![White, Black, Green],
        "reach guard: the Plate granted protection from white"
    );
    assert_eq!(
        board.state().objects[&ward].zone,
        Zone::Graveyard,
        "CR 704.5m: the white Aura is removed by the Plate's protection from white"
    );
}

/// R15. CR 701.3b + CR 702.16c: once the Plate gives protection from white, an
/// existing white Pacifism is removed and a white Aura cannot be moved onto the
/// protected creature. The control board (no Plate equipped) shows the same
/// `attach_to` call succeeds when Bear is not protected.
#[test]
fn plates_protection_blocks_and_removes_white_pacifisms() {
    // Control: without the Plate equipped, attach_to moves Pacifism onto Bear.
    {
        let mut mover = None;
        let mut board = Board::new(|s| {
            add_command_zone_commander(s, "Izzet Commander", vec![Blue, Red]);
            mover = Some(
                s.add_enchantment_from_oracle(P1, "Pacifism", PACIFISM)
                    .with_subtypes(vec!["Aura"])
                    .with_color(vec![White])
                    .id(),
            );
        });
        let mover = mover.unwrap();
        attach_fixture(&mut board.runner, mover, board.foe);
        attach_to(board.runner.state_mut(), mover, board.bear);
        assert_eq!(
            board.state().objects[&mover].attached_to,
            Some(AttachTarget::Object(board.bear)),
            "reach guard: an unprotected Bear accepts Pacifism through attach_to"
        );
    }

    let mut existing = None;
    let mut mover = None;
    let mut board = Board::new(|s| {
        add_command_zone_commander(s, "Izzet Commander", vec![Blue, Red]);
        existing = Some(
            s.add_enchantment_from_oracle(P1, "Pacifism", PACIFISM)
                .with_subtypes(vec!["Aura"])
                .with_color(vec![White])
                .id(),
        );
        mover = Some(
            s.add_enchantment_from_oracle(P1, "Pacifism", PACIFISM)
                .with_subtypes(vec!["Aura"])
                .with_color(vec![White])
                .id(),
        );
    });
    let (existing, mover) = (existing.unwrap(), mover.unwrap());
    attach_fixture(&mut board.runner, existing, board.bear);
    attach_fixture(&mut board.runner, mover, board.foe);
    refresh(&mut board.runner);
    assert_eq!(
        board.state().objects[&existing].attached_to,
        Some(AttachTarget::Object(board.bear)),
        "reach guard: the existing Pacifism is attached to Bear before the equip"
    );

    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_eq!(
        board.state().objects[&existing].zone,
        Zone::Graveyard,
        "CR 704.5m: the Pacifism already on Bear goes to the graveyard once Bear has protection from white"
    );
    attach_to(board.runner.state_mut(), mover, board.bear);
    assert_eq!(
        board.state().objects[&mover].attached_to,
        Some(AttachTarget::Object(board.foe)),
        "CR 701.3b: a white Aura cannot be moved onto a creature with protection from white"
    );
    assert!(
        !board.state().objects[&board.bear]
            .attachments
            .contains(&mover),
        "Bear must not list the refused Aura"
    );
}

/// R16. Hostile sibling, no over-blocking: with a {W,U} commander the Plate
/// grants protection from black, red and green only. Bear's protection from
/// white comes solely from the white Ward's own chosen-color grant (CR 702.16n:
/// that Aura is exempt from it), and the identity color blue is not granted, so
/// the white Ward stays attached. Over-expansion (granting white or blue from
/// the Plate) would send the Ward to the graveyard or add blue.
#[test]
fn plates_protection_does_not_remove_an_aura_of_an_identity_color() {
    let WardBoard { mut board, ward } = ward_board(vec![White, Blue], White);
    equip(&mut board.runner, board.plate, EQUIP, board.bear);

    assert_eq!(
        protected_colors(board.state(), board.bear),
        vec![White, Black, Red, Green],
        "reach guard: the Plate granted B/R/G (the colors outside {{W,U}}); \
         white is Ward's own chosen-color grant; blue (an identity color) is absent"
    );
    assert_eq!(
        board.state().objects[&ward].zone,
        Zone::Battlefield,
        "the white Ward is not removed by the Plate's protection from black, red and green"
    );
    assert_eq!(
        board.state().objects[&ward].attached_to,
        Some(AttachTarget::Object(board.bear))
    );
}
