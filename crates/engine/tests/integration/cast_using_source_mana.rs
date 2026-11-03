//! CR 106.3 + CR 601.2h + CR 603.2: "Whenever you cast <spell> using mana produced by ~"
//! (Barracks of the Thousand, Tecutlan, the Searing Rift, The Myriad Pools) is part of the
//! trigger event: the ability triggers when at least one mana spent on that spell came from
//! this permanent. It reads the spell's payment once, at the event, naming the listening
//! permanent, and is not an intervening "if" (CR 603.4).
//!
//! Each board below stages the listener with the verbatim Oracle text of a back face, so these
//! rows run the current parser. Only the transform row reads the printed face from the fixture.
//! The Myriad Pools' copy effect is out of scope here: only its trigger gate is pinned.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TriggerCondition;
use engine::types::actions::{CastChoice, GameAction};
use engine::types::game_state::{CastOfferKind, CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const BARRACKS: &str = "(Transforms from Thousand Moons Smithy.)\n{T}: Add {W}.\nWhenever you cast an artifact or creature spell using mana produced by Barracks of the Thousand, create a white Gnome Soldier artifact creature token with \"This token's power and toughness are each equal to the number of artifacts and/or creatures you control.\"";
const TECUTLAN: &str = "(Transforms from Brass's Tunnel-Grinder.)\n{T}: Add {R}.\nWhenever you cast a permanent spell using mana produced by Tecutlan, discover X, where X is that spell's mana value.";
const MYRIAD_POOLS: &str = "(Transforms from The Everflowing Well.)\n{T}: Add {U}.\nWhenever you cast a permanent spell using mana produced by The Myriad Pools, up to one other target permanent you control becomes a copy of that spell until end of turn.";
const MULTANIS_PRESENCE: &str = "Whenever a spell you've cast is countered, draw a card.";
const KRARK_CLAN_IRONWORKS: &str = "Sacrifice an artifact: Add {C}{C}.";
const MIRROR_GALLERY: &str = "The \"legend rule\" doesn't apply.";
const MIRAGE_MIRROR: &str = "{2}: This artifact becomes a copy of target artifact, creature, enchantment, or land until end of turn.";
const MASTERMIND_PLUM: &str = "Whenever Mastermind Plum attacks, exile up to one target card from a graveyard. If an artifact card was exiled this way, create a Treasure token. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")\nWhenever you cast a spell, if mana from a Treasure was spent to cast it, you draw a card and you lose 1 life.";
const INGA_AND_ESIKA: &str = "Creatures you control have vigilance and \"{T}: Add one mana of any color. Spend this mana only to cast a creature spell.\"\nWhenever you cast a creature spell, if three or more mana from creatures was spent to cast it, draw a card.";

fn db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load")
}

/// P0's precombat main phase, with cards to draw for each player: a draw from an empty library
/// loses the game (CR 704.5b).
fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..4 {
        scenario.add_card_to_library_top(P0, "Library Filler");
        scenario.add_card_to_library_top(P1, "Library Filler");
    }
    scenario
}

fn add_barracks(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_land_from_oracle(P0, "Barracks of the Thousand", BARRACKS)
        .as_artifact_land()
        .as_legendary()
        .id()
}

fn add_tecutlan(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_land_from_oracle(P0, "Tecutlan, the Searing Rift", TECUTLAN)
        .as_legendary()
        .with_subtypes(vec!["Cave"])
        .id()
}

fn add_pools(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_land_from_oracle(P0, "The Myriad Pools", MYRIAD_POOLS)
        .as_artifact_land()
        .as_legendary()
        .id()
}

/// Stage a real card, its mana cost and types included.
fn real(scenario: &mut GameScenario, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    scenario.add_real_card(player, name, zone, db())
}

/// Activate a permanent's first mana ability, leaving the mana floating. The unit's `source_id`
/// is the one a real activation writes (CR 106.3).
fn float(runner: &mut GameRunner, source: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the mana ability must be activatable");
    assert!(
        runner.state().objects[&source].tapped,
        "reach guard: the {} tapped for mana",
        runner.state().objects[&source].name
    );
}

fn trigger_sources(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { source_id, .. } => Some(*source_id),
            _ => None,
        })
        .collect()
}

fn gnome_soldiers(runner: &GameRunner) -> usize {
    runner
        .battlefield_names()
        .iter()
        .filter(|name| name.as_str() == "Gnome Soldier")
        .count()
}

/// The sources of every mana spent on `spell`, read off its payment record.
fn spent_sources(runner: &GameRunner, spell: ObjectId) -> Vec<ObjectId> {
    runner.state().objects[&spell]
        .mana_spent_source_snapshots
        .iter()
        .map(|snapshot| snapshot.source_id)
        .collect()
}

fn library_len(runner: &GameRunner) -> usize {
    runner.state().players[0].library.len()
}

fn hand_len(runner: &GameRunner) -> usize {
    runner.state().players[0].hand.len()
}

/// Pass priority until the stack empties or a prompt opens.
fn pass_until_settled(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().stack.is_empty()
            || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must be accepted");
    }
    panic!("the stack did not settle: {:?}", runner.state().waiting_for);
}

fn assert_finished(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
}

fn resolve_all(runner: &mut GameRunner) {
    pass_until_settled(runner);
    assert_finished(runner);
}

/// Resolve to the discover offer, name what it offers, and put the card in hand.
fn take_discover_offer(runner: &mut GameRunner) -> (ObjectId, u32) {
    pass_until_settled(runner);
    let WaitingFor::CastOffer {
        kind:
            CastOfferKind::Discover {
                hit_card,
                discover_value,
                ..
            },
        ..
    } = runner.state().waiting_for
    else {
        panic!(
            "expected a discover offer: {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::DiscoverChoice {
            choice: CastChoice::Decline,
        })
        .expect("declining the cast must be accepted");
    resolve_all(runner);
    (hit_card, discover_value)
}

/// Blue mana for a response, from no permanent in particular.
fn blue_mana(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]); count]
}

/// Put `cards` on top of P0's library, the first one topmost.
fn stage_library_top(runner: &mut GameRunner, cards: &[ObjectId]) {
    let library = &mut runner.state_mut().players[0].library;
    library.retain(|id| !cards.contains(id));
    for card in cards.iter().rev() {
        library.insert(0, *card);
    }
}

/// One Barracks board, with every spell the rows below need in hand.
struct BarracksBoard {
    runner: GameRunner,
    barracks: ObjectId,
    plains: ObjectId,
    lions: ObjectId,
    walker: ObjectId,
    ornithopter: ObjectId,
    honor: ObjectId,
}

fn barracks_board() -> BarracksBoard {
    barracks_board_with(|_, _| {}).0
}

fn barracks_board_with<T>(
    setup: impl FnOnce(&mut GameScenario, ObjectId) -> T,
) -> (BarracksBoard, T) {
    let mut scenario = main_phase();
    let barracks = add_barracks(&mut scenario);
    let plains = scenario.add_basic_land(P0, ManaColor::White);
    let lions = real(&mut scenario, P0, "Savannah Lions", Zone::Hand);
    let walker = real(&mut scenario, P0, "Hangarback Walker", Zone::Hand);
    let ornithopter = real(&mut scenario, P0, "Ornithopter", Zone::Hand);
    let honor = real(&mut scenario, P0, "Honor of the Pure", Zone::Hand);
    let staged = setup(&mut scenario, barracks);
    let board = BarracksBoard {
        runner: scenario.build(),
        barracks,
        plains,
        lions,
        walker,
        ornithopter,
        honor,
    };
    (board, staged)
}

/// CR 601.2h + CR 603.2: a spell paid only with other mana does not trigger Barracks, and the
/// same spell paid with Barracks' own mana does, once.
#[test]
fn barracks_triggers_only_for_a_spell_its_own_mana_paid_for() {
    let mut board = barracks_board();
    float(&mut board.runner, board.plains);
    board.runner.cast(board.lions).commit();
    assert_eq!(spent_sources(&board.runner, board.lions), [board.plains]);
    assert!(
        !board.runner.state().objects[&board.barracks].tapped,
        "Barracks paid for nothing"
    );
    assert_eq!(board.runner.state().stack.len(), 1);
    assert_eq!(trigger_sources(&board.runner), []);
    resolve_all(&mut board.runner);
    assert_eq!(gnome_soldiers(&board.runner), 0);
    assert_eq!(
        board.runner.state().objects[&board.lions].zone,
        Zone::Battlefield
    );

    let mut board = barracks_board();
    float(&mut board.runner, board.barracks);
    board.runner.cast(board.lions).commit();
    assert_eq!(spent_sources(&board.runner, board.lions), [board.barracks]);
    assert_eq!(trigger_sources(&board.runner), [board.barracks]);
    resolve_all(&mut board.runner);
    assert_eq!(gnome_soldiers(&board.runner), 1);
    assert_eq!(
        board.runner.state().objects[&board.lions].zone,
        Zone::Battlefield
    );
}

/// CR 601.2h: one of several mana is enough: the clause says "using mana produced by", not
/// "using only mana produced by".
#[test]
fn barracks_triggers_when_one_of_several_mana_came_from_it() {
    let mut board = barracks_board();
    float(&mut board.runner, board.barracks);
    float(&mut board.runner, board.plains);
    board.runner.cast(board.walker).x(1).commit();
    let mut spent = spent_sources(&board.runner, board.walker);
    spent.sort();
    let mut expected = vec![board.barracks, board.plains];
    expected.sort();
    assert_eq!(spent, expected);
    assert_eq!(trigger_sources(&board.runner), [board.barracks]);
    resolve_all(&mut board.runner);
    assert_eq!(gnome_soldiers(&board.runner), 1);
}

/// CR 603.2: the event is the cast, not each mana: two of Barracks' mana is still one trigger.
#[test]
fn two_of_barracks_mana_trigger_it_once() {
    let (mut board, ()) = barracks_board_with(|scenario, barracks| {
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::White, barracks, false, vec![]); 2],
        );
    });
    board.runner.cast(board.walker).x(1).commit();
    assert_eq!(
        spent_sources(&board.runner, board.walker),
        [board.barracks, board.barracks]
    );
    assert_eq!(trigger_sources(&board.runner), [board.barracks]);
    resolve_all(&mut board.runner);
    assert_eq!(gnome_soldiers(&board.runner), 1);
}

/// CR 601.2h: a spell that cost nothing spent nothing, so there is no mana to come from Barracks.
#[test]
fn a_free_spell_does_not_trigger_barracks() {
    let mut board = barracks_board();
    board.runner.cast(board.ornithopter).commit();
    assert_eq!(spent_sources(&board.runner, board.ornithopter), []);
    assert_eq!(trigger_sources(&board.runner), []);
    resolve_all(&mut board.runner);
    assert_eq!(gnome_soldiers(&board.runner), 0);
    assert_eq!(
        board.runner.state().objects[&board.ornithopter].zone,
        Zone::Battlefield
    );

    let mut board = barracks_board();
    float(&mut board.runner, board.barracks);
    board.runner.cast(board.lions).commit();
    assert_eq!(trigger_sources(&board.runner), [board.barracks]);
}

/// CR 603.2: the qualifier narrows the spell filter, it does not replace it: Barracks' own mana
/// on a spell of the wrong type triggers nothing.
#[test]
fn barracks_mana_on_a_noncreature_nonartifact_spell_does_not_trigger() {
    let mut board = barracks_board();
    float(&mut board.runner, board.barracks);
    float(&mut board.runner, board.plains);
    board.runner.cast(board.honor).commit();
    assert!(spent_sources(&board.runner, board.honor).contains(&board.barracks));
    assert_eq!(trigger_sources(&board.runner), []);
    resolve_all(&mut board.runner);
    assert_eq!(gnome_soldiers(&board.runner), 0);
    assert_eq!(
        board.runner.state().objects[&board.honor].zone,
        Zone::Battlefield
    );

    let mut board = barracks_board();
    float(&mut board.runner, board.barracks);
    board.runner.cast(board.lions).commit();
    assert_eq!(trigger_sources(&board.runner), [board.barracks]);
}

/// CR 603.2: Tecutlan's own mana on a nonpermanent spell triggers nothing; the same mana on a
/// permanent spell does.
#[test]
fn tecutlan_mana_on_a_nonpermanent_spell_does_not_trigger() {
    let board = || {
        let mut scenario = main_phase();
        let tecutlan = add_tecutlan(&mut scenario);
        let mountain = scenario.add_basic_land(P0, ManaColor::Red);
        let lions = real(&mut scenario, P0, "Savannah Lions", Zone::Library);
        let shock = real(&mut scenario, P0, "Shock", Zone::Hand);
        let piker = real(&mut scenario, P0, "Goblin Piker", Zone::Hand);
        let mut runner = scenario.build();
        stage_library_top(&mut runner, &[lions]);
        (runner, tecutlan, mountain, shock, piker, lions)
    };

    let (mut runner, tecutlan, _, shock, _, _) = board();
    let library = library_len(&runner);
    float(&mut runner, tecutlan);
    runner.cast(shock).target_player(P1).commit();
    assert_eq!(spent_sources(&runner, shock), [tecutlan]);
    assert_eq!(trigger_sources(&runner), []);
    resolve_all(&mut runner);
    assert_eq!(runner.life(P1), 18, "Shock resolved");
    assert_eq!(library_len(&runner), library);

    let (mut runner, tecutlan, mountain, _, piker, lions) = board();
    float(&mut runner, tecutlan);
    float(&mut runner, mountain);
    runner.cast(piker).commit();
    assert_eq!(trigger_sources(&runner), [tecutlan]);
    assert_eq!(take_discover_offer(&mut runner).0, lions);
}

/// CR 603.2: The Myriad Pools' own mana on a nonpermanent spell triggers nothing.
#[test]
fn pools_mana_on_a_nonpermanent_spell_does_not_trigger() {
    let mut scenario = main_phase();
    let pools = add_pools(&mut scenario);
    let opt = real(&mut scenario, P0, "Opt", Zone::Hand);
    let mut runner = scenario.build();
    let hand = hand_len(&runner);
    float(&mut runner, pools);
    runner.cast(opt).commit();
    assert_eq!(spent_sources(&runner, opt), [pools]);
    assert_eq!(trigger_sources(&runner), []);
    assert_eq!(runner.state().stack.len(), 1);
    pass_until_settled(&mut runner);
    let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!("Opt scries first: {:?}", runner.state().waiting_for);
    };
    runner
        .act(GameAction::SelectCards { cards })
        .expect("keeping the card on top must be accepted");
    resolve_all(&mut runner);
    assert_eq!(runner.state().objects[&opt].zone, Zone::Graveyard);
    assert_eq!(hand_len(&runner), hand, "Opt left the hand and drew a card");
}

/// CR 603.2: The Myriad Pools' gate. A permanent spell partly paid by the Pools puts one trigger
/// on the stack and one paid by other Islands puts none. Stops at the commit: what the trigger
/// does is not claimed here.
#[test]
fn pools_trigger_is_gated_on_its_own_mana() {
    let board = || {
        let mut scenario = main_phase();
        let pools = add_pools(&mut scenario);
        let island = scenario.add_basic_land(P0, ManaColor::Blue);
        let second_island = scenario.add_basic_land(P0, ManaColor::Blue);
        let looter = real(&mut scenario, P0, "Merfolk Looter", Zone::Hand);
        (scenario.build(), pools, island, second_island, looter)
    };

    let (mut runner, pools, island, _, looter) = board();
    float(&mut runner, pools);
    float(&mut runner, island);
    runner.cast(looter).commit();
    assert_eq!(trigger_sources(&runner), [pools]);
    assert_eq!(runner.state().stack.len(), 2);

    let (mut runner, _, island, second_island, looter) = board();
    float(&mut runner, island);
    float(&mut runner, second_island);
    runner.cast(looter).commit();
    assert_eq!(trigger_sources(&runner), []);
    assert_eq!(runner.state().stack.len(), 1);
}

/// CR 603.4: the qualifier is not an intervening "if", so nothing rechecks it. Countering the
/// spell in response clears its payment stamps once Multani's Presence adds a trigger to the
/// countering batch, and the token is still made.
#[test]
fn barracks_trigger_survives_countering_the_spell() {
    let (mut board, cancel) = barracks_board_with(|scenario, _| {
        scenario.add_enchantment_from_oracle(P0, "Multani's Presence", MULTANIS_PRESENCE);
        scenario.with_mana_pool(P1, blue_mana(3));
        real(scenario, P1, "Cancel", Zone::Hand)
    });
    let runner = &mut board.runner;
    float(runner, board.barracks);
    runner.cast(board.lions).commit();
    assert_eq!(trigger_sources(runner), [board.barracks]);

    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes with the trigger on the stack");
    runner.cast(cancel).target_object(board.lions).commit();
    assert_eq!(
        trigger_sources(runner),
        [board.barracks],
        "reach guard: the Barracks trigger is on the stack when Cancel resolves"
    );
    let hand = hand_len(runner);

    runner.act(GameAction::PassPriority).expect("P1 passes");
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert_eq!(runner.state().objects[&cancel].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&board.lions].zone, Zone::Graveyard);
    assert!(
        trigger_sources(runner).contains(&board.barracks),
        "reach guard: the Barracks trigger outlived the countered spell"
    );
    assert_eq!(
        spent_sources(runner, board.lions),
        [],
        "reach guard: the spell's payment stamps are gone, so a recheck could only answer no"
    );

    resolve_all(runner);
    assert_eq!(gnome_soldiers(runner), 1);
    assert_eq!(runner.state().objects[&board.lions].zone, Zone::Graveyard);
    assert_eq!(hand_len(runner), hand + 1, "Multani's Presence drew a card");
}

/// CR 113.7a: once triggered the ability exists independently of its source, so destroying
/// Barracks in response does not stop the token.
#[test]
fn barracks_trigger_survives_its_source_leaving() {
    let (mut board, shatter) = barracks_board_with(|scenario, _| {
        scenario.with_mana_pool(
            P1,
            vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]); 2],
        );
        real(scenario, P1, "Shatter", Zone::Hand)
    });
    let runner = &mut board.runner;
    float(runner, board.barracks);
    runner.cast(board.lions).commit();

    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes with the trigger on the stack");
    runner.cast(shatter).target_object(board.barracks).commit();
    assert_eq!(
        trigger_sources(runner),
        [board.barracks],
        "reach guard: the Barracks trigger is on the stack when Shatter resolves"
    );

    resolve_all(runner);
    assert_eq!(
        runner.state().objects[&board.barracks].zone,
        Zone::Graveyard
    );
    assert_eq!(gnome_soldiers(runner), 1);
}

/// One board with two listeners, a Mountain, and the top of the library staged for discover.
struct TwoListeners {
    runner: GameRunner,
    barracks: ObjectId,
    tecutlan: ObjectId,
    mountain: ObjectId,
    piker: ObjectId,
    lions: ObjectId,
    top_of_library: ObjectId,
}

fn two_listeners() -> TwoListeners {
    let mut scenario = main_phase();
    let barracks = add_barracks(&mut scenario);
    let tecutlan = add_tecutlan(&mut scenario);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let piker = real(&mut scenario, P0, "Goblin Piker", Zone::Hand);
    let lions = real(&mut scenario, P0, "Savannah Lions", Zone::Hand);
    let top_of_library = real(&mut scenario, P0, "Savannah Lions", Zone::Library);
    let mut runner = scenario.build();
    stage_library_top(&mut runner, &[top_of_library]);
    TwoListeners {
        runner,
        barracks,
        tecutlan,
        mountain,
        piker,
        lions,
        top_of_library,
    }
}

/// CR 201.5: "~" means that one object. With Barracks and Tecutlan in play, each trigger is
/// gated only by its own mana, and a spell paid by both triggers both.
#[test]
fn each_listener_answers_only_for_its_own_mana() {
    let mut board = two_listeners();
    let library = library_len(&board.runner);
    float(&mut board.runner, board.tecutlan);
    float(&mut board.runner, board.mountain);
    board.runner.cast(board.piker).commit();
    assert_eq!(trigger_sources(&board.runner), [board.tecutlan]);
    assert_eq!(
        take_discover_offer(&mut board.runner).0,
        board.top_of_library
    );
    assert_eq!(gnome_soldiers(&board.runner), 0);
    assert!(library_len(&board.runner) < library);

    let mut board = two_listeners();
    let library = library_len(&board.runner);
    float(&mut board.runner, board.barracks);
    board.runner.cast(board.lions).commit();
    assert_eq!(trigger_sources(&board.runner), [board.barracks]);
    resolve_all(&mut board.runner);
    assert_eq!(gnome_soldiers(&board.runner), 1);
    assert_eq!(library_len(&board.runner), library);

    let mut board = two_listeners();
    float(&mut board.runner, board.tecutlan);
    float(&mut board.runner, board.barracks);
    // The fluent cast driver does not stop at this prompt, so the cast is submitted directly.
    let card_id = board.runner.state().objects[&board.piker].card_id;
    board
        .runner
        .act(GameAction::CastSpell {
            object_id: board.piker,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the floating mana pays for Goblin Piker");
    let WaitingFor::OrderTriggers { triggers, .. } = &board.runner.state().waiting_for else {
        panic!(
            "both triggers are P0's to order: {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(triggers.len(), 2);
    board
        .runner
        .act(GameAction::OrderTriggers { order: vec![1, 0] })
        .expect("ordering the triggers must be accepted");
    let mut sources = trigger_sources(&board.runner);
    sources.sort();
    let mut expected = vec![board.barracks, board.tecutlan];
    expected.sort();
    assert_eq!(sources, expected);
    assert_eq!(
        take_discover_offer(&mut board.runner).0,
        board.top_of_library
    );
    assert_eq!(gnome_soldiers(&board.runner), 1);
}

/// One Barracks and one Mirage Mirror that has become a copy of it, with Mirror Gallery keeping
/// both in play. The Mirror's {2} is paid from the pool, so Barracks stays untapped.
fn barracks_and_its_copy() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = main_phase();
    let barracks = add_barracks(&mut scenario);
    scenario.add_artifact_from_oracle(P0, "Mirror Gallery", MIRROR_GALLERY);
    let mirror = scenario
        .add_artifact_from_oracle(P0, "Mirage Mirror", MIRAGE_MIRROR)
        .id();
    let lions = real(&mut scenario, P0, "Savannah Lions", Zone::Hand);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 2],
    );
    let mut runner = scenario.build();
    runner.activate(mirror, 0).target_object(barracks).resolve();
    assert_finished(&runner);
    assert!(!runner.state().objects[&barracks].tapped);
    assert_eq!(
        runner.state().objects[&mirror].name,
        "Barracks of the Thousand",
        "reach guard: the Mirror became a copy of Barracks"
    );
    assert_eq!(runner.state().objects[&barracks].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&mirror].zone, Zone::Battlefield);
    (runner, barracks, mirror, lions)
}

/// CR 201.5 + CR 707.11: a same-named copy is a different object. Each of the two permanents
/// answers only for the mana it made itself, which an id comparison gets right and a name
/// comparison would not.
#[test]
fn a_same_named_copy_answers_only_for_its_own_mana() {
    let (mut runner, _, mirror, lions) = barracks_and_its_copy();
    float(&mut runner, mirror);
    runner.cast(lions).commit();
    assert_eq!(trigger_sources(&runner), [mirror]);
    resolve_all(&mut runner);
    assert_eq!(gnome_soldiers(&runner), 1);

    let (mut runner, barracks, _, lions) = barracks_and_its_copy();
    float(&mut runner, barracks);
    runner.cast(lions).commit();
    assert_eq!(trigger_sources(&runner), [barracks]);
    resolve_all(&mut runner);
    assert_eq!(gnome_soldiers(&runner), 1);
}

/// CR 712.18: transforming does not make a new object, so the real Thousand Moons Smithy answers
/// for its own mana once it is Barracks, through the back face's printed trigger.
#[test]
fn real_smithy_transforms_and_triggers_from_its_back_face() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    for _ in 0..4 {
        scenario.add_card_to_library_top(P0, "Library Filler");
    }
    let smithy = real(
        &mut scenario,
        P0,
        "Thousand Moons Smithy",
        Zone::Battlefield,
    );
    let crew: Vec<ObjectId> = (0..5).map(|_| scenario.add_vanilla(P0, 1, 1)).collect();
    let lions = real(&mut scenario, P0, "Savannah Lions", Zone::Hand);
    let mut runner = scenario.build();

    let back_face = runner.state().objects[&smithy]
        .back_face
        .as_ref()
        .expect("the real card has a back face");
    assert!(
        back_face
            .trigger_definitions
            .iter_all()
            .any(|trigger| matches!(trigger.condition, Some(TriggerCondition::EventTime { .. }))),
        "the printed back face must carry the qualifier: {:?}",
        back_face.trigger_definitions
    );

    runner.advance_to_phase(Phase::PreCombatMain);
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting must be accepted");
            }
            WaitingFor::PayCost { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: crew.clone(),
                    })
                    .expect("tapping five others must be accepted");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_finished(&runner);
    assert_eq!(
        runner.state().objects[&smithy].name,
        "Barracks of the Thousand",
        "reach guard: the Smithy transformed"
    );

    float(&mut runner, smithy);
    runner.cast(lions).commit();
    assert_eq!(trigger_sources(&runner), [smithy]);
    resolve_all(&mut runner);
    assert_eq!(gnome_soldiers(&runner), 1);
}

/// CR 113.6: a permanent's ability works only on the battlefield. Mana that outlives Barracks
/// still pays for the spell, and nothing is listening.
#[test]
fn barracks_mana_spent_after_it_left_triggers_nothing() {
    let (mut board, ironworks) = barracks_board_with(|scenario, _| {
        scenario
            .add_artifact_from_oracle(P0, "Krark-Clan Ironworks", KRARK_CLAN_IRONWORKS)
            .id()
    });
    let runner = &mut board.runner;
    float(runner, board.barracks);
    runner
        .act(GameAction::ActivateAbility {
            source_id: ironworks,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Krark-Clan Ironworks is activatable");
    runner
        .act(GameAction::SelectCards {
            cards: vec![board.barracks],
        })
        .expect("sacrificing Barracks must be accepted");
    assert_eq!(
        runner.state().objects[&board.barracks].zone,
        Zone::Graveyard
    );

    runner.cast(board.lions).commit();
    assert_eq!(
        spent_sources(runner, board.lions),
        [board.barracks],
        "reach guard: the record names Barracks, so only the zone gate answers"
    );
    assert_eq!(trigger_sources(runner), []);
    resolve_all(runner);
    assert_eq!(gnome_soldiers(runner), 0);
}

/// One Tecutlan board with a Thran Dynamo over a Goblin Piker over a Savannah Lions on top of
/// the library: mana values 4, 2 and 1.
struct TecutlanBoard {
    runner: GameRunner,
    tecutlan: ObjectId,
    mountains: [ObjectId; 2],
    piker: ObjectId,
    mass: ObjectId,
    second_piker: ObjectId,
}

fn tecutlan_board() -> TecutlanBoard {
    let mut scenario = main_phase();
    let tecutlan = add_tecutlan(&mut scenario);
    let mountains = [(); 2].map(|()| scenario.add_basic_land(P0, ManaColor::Red));
    let piker = real(&mut scenario, P0, "Goblin Piker", Zone::Hand);
    let mass = real(&mut scenario, P0, "Chimeric Mass", Zone::Hand);
    let dynamo = real(&mut scenario, P0, "Thran Dynamo", Zone::Library);
    let second_piker = real(&mut scenario, P0, "Goblin Piker", Zone::Library);
    let lions = real(&mut scenario, P0, "Savannah Lions", Zone::Library);
    let mut runner = scenario.build();
    stage_library_top(&mut runner, &[dynamo, second_piker, lions]);
    TecutlanBoard {
        runner,
        tecutlan,
        mountains,
        piker,
        mass,
        second_piker,
    }
}

/// CR 701.57a: X is that spell's mana value, and other mana discovers nothing.
#[test]
fn tecutlan_discovers_for_the_mana_value_of_the_spell() {
    let mut board = tecutlan_board();
    float(&mut board.runner, board.tecutlan);
    float(&mut board.runner, board.mountains[0]);
    board.runner.cast(board.piker).commit();
    assert_eq!(
        take_discover_offer(&mut board.runner),
        (board.second_piker, 2),
        "the spell has mana value 2: the Dynamo is passed over"
    );

    let mut board = tecutlan_board();
    let library = library_len(&board.runner);
    float(&mut board.runner, board.mountains[0]);
    float(&mut board.runner, board.mountains[1]);
    board.runner.cast(board.piker).commit();
    assert_eq!(trigger_sources(&board.runner), []);
    resolve_all(&mut board.runner);
    assert_eq!(
        library_len(&board.runner),
        library,
        "other mana: no discover"
    );
}

/// CR 701.57a + CR 202.3e: on the stack Chimeric Mass {X} has the mana value chosen for X, so
/// that number decides how far discover reaches: past the Dynamo in both rows, and to the Goblin
/// Piker only when X is 2.
#[test]
fn tecutlan_discovers_with_the_value_chosen_for_x() {
    let discover_for = |x: u32| {
        let mut board = tecutlan_board();
        float(&mut board.runner, board.tecutlan);
        for mountain in &board.mountains[..x as usize - 1] {
            float(&mut board.runner, *mountain);
        }
        board.runner.cast(board.mass).x(x).commit();
        let (hit, value) = take_discover_offer(&mut board.runner);
        (board.runner.state().objects[&hit].name.clone(), value)
    };
    assert_eq!(
        [discover_for(1), discover_for(2)],
        [
            ("Savannah Lions".to_string(), 1),
            ("Goblin Piker".to_string(), 2)
        ]
    );
}

/// Preservation: real intervening-if cast triggers that read the payment keep their reading.
/// Treasure mana draws and costs 1 life on Mastermind Plum; other mana does nothing.
#[test]
fn mastermind_plum_still_reads_treasure_mana() {
    let board = |from_treasure: bool| {
        let mut scenario = main_phase();
        scenario.add_creature_from_oracle(P0, "Mastermind Plum", 3, 3, MASTERMIND_PLUM);
        let treasure = scenario
            .add_artifact_from_oracle(
                P0,
                "Treasure",
                "{T}, Sacrifice this token: Add one mana of any color.",
            )
            .with_subtypes(vec!["Treasure"])
            .id();
        let plains = scenario.add_basic_land(P0, ManaColor::White);
        let lions = real(&mut scenario, P0, "Savannah Lions", Zone::Hand);
        let source = if from_treasure { treasure } else { plains };
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::White, source, false, vec![])],
        );
        (scenario.build(), lions, source)
    };

    let (mut runner, lions, treasure) = board(true);
    let hand = hand_len(&runner);
    runner.cast(lions).commit();
    assert_eq!(spent_sources(&runner, lions), [treasure]);
    assert_eq!(trigger_sources(&runner).len(), 1);
    resolve_all(&mut runner);
    assert_eq!(hand_len(&runner), hand, "the spell left, a card was drawn");
    assert_eq!(runner.life(P0), 19);

    let (mut runner, lions, plains) = board(false);
    let hand = hand_len(&runner);
    runner.cast(lions).commit();
    assert_eq!(spent_sources(&runner, lions), [plains]);
    assert_eq!(trigger_sources(&runner), []);
    resolve_all(&mut runner);
    assert_eq!(hand_len(&runner), hand - 1, "the spell left, nothing drawn");
    assert_eq!(runner.life(P0), 20);
}

/// Preservation: Inga and Esika counts mana from creatures: three of it draws, two does not.
#[test]
fn inga_and_esika_still_counts_mana_from_creatures() {
    let board = |creature_mana: usize| {
        let mut scenario = main_phase();
        scenario.add_creature_from_oracle(P0, "Inga and Esika", 2, 3, INGA_AND_ESIKA);
        let creatures: Vec<ObjectId> = (0..3).map(|_| scenario.add_vanilla(P0, 1, 1)).collect();
        let plains = scenario.add_basic_land(P0, ManaColor::White);
        let walker = real(&mut scenario, P0, "Hangarback Walker", Zone::Hand);
        let pool = creatures[..creature_mana]
            .iter()
            .copied()
            .chain(std::iter::repeat(plains))
            .take(4)
            .map(|source| ManaUnit::new(ManaType::White, source, false, vec![]))
            .collect();
        scenario.with_mana_pool(P0, pool);
        (scenario.build(), walker)
    };

    let (mut runner, walker) = board(3);
    let hand = hand_len(&runner);
    runner.cast(walker).x(2).commit();
    assert_eq!(trigger_sources(&runner).len(), 1);
    resolve_all(&mut runner);
    assert_eq!(hand_len(&runner), hand, "the spell left, a card was drawn");

    let (mut runner, walker) = board(2);
    let hand = hand_len(&runner);
    runner.cast(walker).x(2).commit();
    assert_eq!(trigger_sources(&runner), []);
    resolve_all(&mut runner);
    assert_eq!(hand_len(&runner), hand - 1, "the spell left, nothing drawn");
}
