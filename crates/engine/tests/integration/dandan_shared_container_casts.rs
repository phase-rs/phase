//! Dandân shared pile: a card is the caster's to use when it sits in the
//! container the caster reads, whoever owns it, and the player who casts or
//! plays a card owns it from then on. Per-seat formats keep the owner.

use engine::database::card_db::CardDatabase;
use engine::game::casting::{
    for_each_structurally_selectable_alternate_spell_payload,
    StructurallySelectableAlternateSpellPayload,
};
use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{apply_resolved_zone_change, move_to_zone};
use engine::types::ability::{
    AbilityKind, CardPlayMode, CastingPermission, Duration, PlayFromExileProvenance, SpellContext,
};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastingVariant, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{ResolvedRulesCommand, ResolvedZoneChangeCommand};
use engine::types::statics::CastFrequency;
use engine::types::zones::{EtbTapState, Zone};

use super::blitz_em_dash_graveyard_cast::cast_from_graveyard;
use super::dandan_filter_owner_axis::{dandan, plenty_of_mana, scenario, stage, start};
use crate::support::shared_card_db;

const CARDS: &[&str] = &[
    "Dandân",
    "Island",
    "Future Sight",
    "Magus of the Future",
    "Bolas's Citadel",
    "Unsubstantiate",
    "Mental Note",
    "Svella, Ice Shaper",
    "Brainstorm",
    "Crucible of Worlds",
    "Think Twice",
    "Deep Analysis",
    "Faithless Looting",
    "Lurrus of the Dream-Den",
    "Emry, Lurker of the Loch",
    "Sol Ring",
    "Lunarch Veteran",
    "Treasure Cruise",
    "Sadistic Slash",
    "Grizzly Bears",
    "Gravecrawler",
    "Diregraf Ghoul",
    "Oathsworn Vampire",
    "Hundred-Battle Veteran",
    "The Indomitable",
    "Smuggler's Copter",
    "Lightwheel Enhancements",
    "Ebondeath, Dracolich",
    "Undead Sprinter",
    "Surge of Acclaim",
    "Lightning Bolt",
    "Control Magic",
    "Reanimate",
    "Cloudshift",
    "Dregscape Zombie",
];

const TOP_OF_LIBRARY_PRODUCERS: [&str; 3] =
    ["Future Sight", "Magus of the Future", "Bolas's Citadel"];

fn other(seat: PlayerId) -> PlayerId {
    if seat == P0 {
        P1
    } else {
        P0
    }
}

fn formats() -> [FormatConfig; 2] {
    [dandan(), FormatConfig::standard()]
}

fn is_dandan(format: &FormatConfig) -> bool {
    *format == dandan()
}

fn offered(state: &GameState, seat: PlayerId, id: ObjectId) -> bool {
    engine::game::casting::spell_objects_available_to_cast(state, seat).contains(&id)
}

fn hand_holder(state: &GameState, id: ObjectId) -> Option<PlayerId> {
    state
        .players
        .iter()
        .find(|player| player.hand.contains(&id))
        .map(|player| player.id)
}

fn zone_changes_of(state: &GameState, id: ObjectId) -> Vec<ResolvedZoneChangeCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| match entry.command.as_ref()? {
            ResolvedRulesCommand::ZoneChange(command) if command.object.object_id == id => {
                Some((**command).clone())
            }
            _ => None,
        })
        .collect()
}

fn command_round_trip(command: &ResolvedZoneChangeCommand) -> ResolvedZoneChangeCommand {
    let wire = serde_json::to_string(&ResolvedRulesCommand::ZoneChange(Box::new(command.clone())))
        .expect("the command serializes");
    match serde_json::from_str(&wire).expect("the command deserializes") {
        ResolvedRulesCommand::ZoneChange(command) => *command,
        other => panic!("expected a zone change, got {other:?}"),
    }
}

fn state_round_trip(state: &GameState) -> GameState {
    let wire = serde_json::to_string(state).expect("the state serializes");
    serde_json::from_str(&wire).expect("the state deserializes")
}

fn give_priority(runner: &mut GameRunner, seat: PlayerId) {
    let state = runner.state_mut();
    state.active_player = seat;
    state.priority_player = seat;
    state.waiting_for = WaitingFor::Priority { player: seat };
}

fn set_speed(runner: &mut GameRunner, seat: PlayerId, speed: u8) {
    runner.state_mut().players[seat.0 as usize].speed = Some(speed);
}

fn reveal_top(runner: &mut GameRunner) {
    engine::game::derived::sync_continuous_reveals(runner.state_mut());
}

fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .expect("the card has an activated ability")
}

#[test]
fn every_card_the_rows_name_is_in_the_database() {
    let Some(db) = shared_card_db() else { return };
    let missing: Vec<_> = CARDS
        .iter()
        .filter(|name| db.get_face_by_name(name).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "missing from the card database: {missing:?}"
    );
}

/// Stages a pile-top `spell` owned by the other seat (or the caster in a per-seat
/// format) under the caster's top-of-library producer, with Unsubstantiate in hand
/// and an Island on the battlefield so a resolved Dandân survives.
fn top_cast_board(
    db: &CardDatabase,
    format: FormatConfig,
    producer: &str,
    spell: &str,
    caster: PlayerId,
) -> (GameRunner, ObjectId, ObjectId) {
    let pile = if is_dandan(&format) {
        other(caster)
    } else {
        caster
    };
    let mut scenario = scenario(format);
    let card = stage(
        &mut scenario,
        db,
        Zone::Library,
        &[(pile, spell), (pile, "Island")],
    )[0];
    scenario.add_real_card(caster, producer, Zone::Battlefield, db);
    scenario.add_real_card(caster, "Island", Zone::Battlefield, db);
    let unsubstantiate = scenario.add_real_card(caster, "Unsubstantiate", Zone::Hand, db);
    let mut runner = start(scenario, caster);
    reveal_top(&mut runner);
    assert_eq!(
        runner.state().objects[&card].owner,
        pile,
        "reach: staged owner"
    );
    assert_eq!(
        runner.state().library_of(caster).front(),
        Some(&card),
        "reach: on top"
    );
    (runner, card, unsubstantiate)
}

#[test]
fn top_cast_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for producer in TOP_OF_LIBRARY_PRODUCERS {
        for caster in [P0, P1] {
            let (mut runner, card, unsubstantiate) =
                top_cast_board(db, dandan(), producer, "Dandân", caster);
            let commit = runner.cast(card).commit();
            assert_eq!(commit.state().objects[&card].zone, Zone::Stack, "reach");
            assert_eq!(
                commit.state().objects[&card].owner,
                caster,
                "{producer}: spell owner"
            );
            commit.resolve();
            assert_eq!(
                runner.state().objects[&card].zone,
                Zone::Battlefield,
                "reach"
            );
            assert_eq!(
                runner.state().objects[&card].owner,
                caster,
                "{producer}: permanent owner"
            );

            runner.cast(unsubstantiate).target_object(card).resolve();
            assert_eq!(
                hand_holder(runner.state(), card),
                Some(caster),
                "{producer}: bounce"
            );
        }
    }
}

#[test]
fn standard_control_future_sight_own_card() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, card, unsubstantiate) =
        top_cast_board(db, FormatConfig::standard(), "Future Sight", "Dandân", P1);
    runner.cast(card).resolve();
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Battlefield,
        "reach"
    );
    assert_eq!(runner.state().objects[&card].owner, P1);
    runner.cast(unsubstantiate).target_object(card).resolve();
    assert_eq!(hand_holder(runner.state(), card), Some(P1));
}

#[test]
fn stack_spell_returns_to_caster() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let (mut runner, note, unsubstantiate) =
            top_cast_board(db, dandan(), "Future Sight", "Mental Note", caster);
        let mut commit = runner.cast(note).commit();
        assert_eq!(commit.state().objects[&note].zone, Zone::Stack, "reach");
        commit.cast(unsubstantiate).target_object(note).resolve();
        assert_eq!(hand_holder(runner.state(), note), Some(caster));
    }
}

#[test]
fn during_resolution_cast_owner() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let pile = other(caster);
        let mut scenario = scenario(dandan());
        let svella = scenario.add_real_card(caster, "Svella, Ice Shaper", Zone::Battlefield, db);
        let hit = scenario.add_real_card(pile, "Brainstorm", Zone::Library, db);
        for _ in 0..8 {
            scenario.add_real_card(pile, "Island", Zone::Library, db);
        }
        let pool = [
            (ManaType::Colorless, 6),
            (ManaType::Red, 1),
            (ManaType::Green, 1),
        ]
        .into_iter()
        .flat_map(|(color, n)| (0..n).map(move |_| ManaUnit::new(color, svella, false, vec![])))
        .collect();
        scenario.with_mana_pool(caster, pool);
        let mut runner = scenario.build();
        give_priority(&mut runner, caster);
        assert_eq!(
            runner.state().objects[&hit].owner,
            pile,
            "reach: staged owner"
        );

        runner.activate(svella, 1).accept_optional().resolve();
        runner
            .act(GameAction::SelectCards { cards: vec![hit] })
            .expect("the caster chooses the hit");
        assert_eq!(runner.state().objects[&hit].zone, Zone::Stack, "reach");
        assert_eq!(runner.state().objects[&hit].owner, caster);
    }
}

#[test]
fn bounce_command_replay() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let (mut runner, card, unsubstantiate) =
            top_cast_board(db, dandan(), "Future Sight", "Dandân", caster);
        runner.cast(card).resolve();
        let commit = runner.cast(unsubstantiate).target_object(card).commit();
        let pre = commit.state().clone();
        commit.resolve();

        let bounce = zone_changes_of(runner.state(), card)
            .into_iter()
            .rev()
            .find(|command| command.from == Zone::Battlefield && command.to == Zone::Hand)
            .expect("the bounce is journaled");
        let mut replay = pre;
        apply_resolved_zone_change(&mut replay, &command_round_trip(&bounce))
            .expect("the bounce replays on its pre-state");
        assert_eq!(hand_holder(&replay, card), Some(caster));
        assert_eq!(replay.objects[&card].owner, caster);
    }
}

/// Plays the first of two staged Islands from `from` under the caster's `producer`.
fn land_play(
    db: &CardDatabase,
    format: FormatConfig,
    producer: &str,
    from: Zone,
    caster: PlayerId,
    pile: PlayerId,
) -> (GameRunner, ObjectId, GameState) {
    let mut scenario = scenario(format);
    let land = stage(
        &mut scenario,
        db,
        from,
        &[(pile, "Island"), (pile, "Island")],
    )[0];
    scenario.add_real_card(caster, producer, Zone::Battlefield, db);
    let mut runner = start(scenario, caster);
    reveal_top(&mut runner);
    assert_eq!(
        runner.state().objects[&land].owner,
        pile,
        "reach: staged owner"
    );
    assert_eq!(
        runner.state().objects[&land].zone,
        from,
        "reach: staged zone"
    );
    let pre = runner.state().clone();
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("the land play is legal");
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Battlefield,
        "reach"
    );
    (runner, land, pre)
}

#[test]
fn land_play_both_seats_and_replay() {
    let Some(db) = shared_card_db() else { return };
    for format in formats() {
        for caster in [P0, P1] {
            let shared = is_dandan(&format);
            let pile = if shared { other(caster) } else { caster };
            let (runner, land, pre) = land_play(
                db,
                format.clone(),
                "Future Sight",
                Zone::Library,
                caster,
                pile,
            );
            let state = runner.state();
            assert_eq!(state.objects[&land].owner, caster);
            let command = zone_changes_of(state, land)
                .pop()
                .expect("the land play is journaled");
            assert_eq!(command.rebound_from, shared.then_some(pile));

            let mut replay = pre;
            apply_resolved_zone_change(&mut replay, &command_round_trip(&command))
                .expect("the land play replays on its pre-state");
            assert_eq!(replay.objects[&land].owner, caster);
            assert_eq!(replay.objects[&land].zone, Zone::Battlefield);
            assert_eq!(state_round_trip(state).objects[&land].owner, caster);
        }
    }
}

#[test]
fn crucible_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        for (format, pile) in [
            (dandan(), other(caster)),
            (FormatConfig::standard(), caster),
        ] {
            let (runner, land, _) = land_play(
                db,
                format,
                "Crucible of Worlds",
                Zone::Graveyard,
                caster,
                pile,
            );
            assert_eq!(runner.state().objects[&land].owner, caster);
            assert_eq!(
                state_round_trip(runner.state()).objects[&land].owner,
                caster
            );
        }

        let mut scenario = scenario(FormatConfig::standard());
        let theirs = stage(
            &mut scenario,
            db,
            Zone::Graveyard,
            &[(other(caster), "Island")],
        )[0];
        scenario.add_real_card(caster, "Crucible of Worlds", Zone::Battlefield, db);
        let mut runner = start(scenario, caster);
        let card_id = runner.state().objects[&theirs].card_id;
        assert!(runner
            .act(GameAction::PlayLand {
                object_id: theirs,
                card_id,
            })
            .is_err());
        assert_eq!(runner.state().objects[&theirs].zone, Zone::Graveyard);
    }
}

fn graveyard_board(
    db: &CardDatabase,
    format: FormatConfig,
    owner: PlayerId,
    card: &str,
    actor: PlayerId,
) -> (GameRunner, ObjectId) {
    let mut scenario = scenario(format);
    let id = stage(&mut scenario, db, Zone::Graveyard, &[(owner, card)])[0];
    let runner = start(scenario, actor);
    assert_eq!(
        runner.state().objects[&id].owner,
        owner,
        "reach: staged owner"
    );
    (runner, id)
}

fn flashback(
    runner: &mut GameRunner,
    card: ObjectId,
    target: Option<PlayerId>,
) -> Result<(), engine::game::engine::EngineError> {
    let cast = runner.cast(card);
    match target {
        Some(player) => cast.target_player(player).try_resolve(),
        None => cast.try_resolve(),
    }
    .map(|_| ())
}

#[test]
fn flashback_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for (card, targets) in [
        ("Think Twice", false),
        ("Deep Analysis", true),
        ("Faithless Looting", false),
    ] {
        for caster in [P0, P1] {
            let target = targets.then_some(caster);
            let (mut runner, id) = graveyard_board(db, dandan(), other(caster), card, caster);
            assert!(offered(runner.state(), caster, id), "{card}: offered");
            let life = runner.life(caster);
            flashback(&mut runner, id, target).unwrap_or_else(|error| panic!("{card}: {error:?}"));
            assert_eq!(
                runner.state().objects[&id].zone,
                Zone::Exile,
                "{card}: exiled"
            );
            if card == "Deep Analysis" {
                assert_eq!(
                    runner.life(caster),
                    life - 3,
                    "the flashback cost, not the printed one, was paid"
                );
            }

            let (runner, own) = graveyard_board(db, FormatConfig::standard(), caster, card, caster);
            assert!(
                offered(runner.state(), caster, own),
                "reach: {card} own graveyard"
            );
            let (mut runner, theirs) =
                graveyard_board(db, FormatConfig::standard(), other(caster), card, caster);
            assert!(
                !offered(runner.state(), caster, theirs),
                "{card}: Standard offer"
            );
            assert!(
                flashback(&mut runner, theirs, target).is_err(),
                "{card}: Standard direct cast"
            );
        }
    }
}

#[test]
fn lurrus_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let mut scenario = scenario(dandan());
        let card = stage(
            &mut scenario,
            db,
            Zone::Graveyard,
            &[(other(caster), "Dandân")],
        )[0];
        scenario.add_real_card(caster, "Lurrus of the Dream-Den", Zone::Battlefield, db);
        scenario.add_real_card(caster, "Island", Zone::Battlefield, db);
        let mut runner = start(scenario, caster);
        assert!(!offered(runner.state(), other(caster), card));
        assert!(offered(runner.state(), caster, card));
        runner
            .cast(card)
            .try_resolve()
            .expect("the Lurrus cast resolves");
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Battlefield,
            "reach"
        );
        assert_eq!(runner.state().objects[&card].owner, caster);
    }
}

#[test]
fn lurrus_and_disturb_menu_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let mut scenario = scenario(dandan());
        let card = stage(
            &mut scenario,
            db,
            Zone::Graveyard,
            &[(other(caster), "Lunarch Veteran")],
        )[0];
        scenario.add_real_card(caster, "Lurrus of the Dream-Den", Zone::Battlefield, db);
        let mut runner = start(scenario, caster);
        let waiting = cast_from_graveyard(&mut runner, card).expect("the cast starts");
        let WaitingFor::CastingVariantChoice { options, .. } = waiting else {
            panic!("expected the casting menu, got {waiting:?}");
        };
        assert_eq!(options.len(), 2, "{options:?}");
        assert!(
            options
                .iter()
                .any(|option| option.variant == CastingVariant::Disturb),
            "the disturb route is offered beside the Lurrus permission: {options:?}"
        );
    }
}

#[test]
fn emry_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let mut scenario = scenario(dandan());
        let ring = stage(
            &mut scenario,
            db,
            Zone::Graveyard,
            &[(other(caster), "Sol Ring")],
        )[0];
        let emry =
            scenario.add_real_card(caster, "Emry, Lurker of the Loch", Zone::Battlefield, db);
        let mut runner = start(scenario, caster);
        assert!(
            !offered(runner.state(), caster, ring),
            "reach: no grant yet"
        );
        let index = activated_index(&runner, emry);
        runner.activate(emry, index).target_object(ring).resolve();
        assert!(offered(runner.state(), caster, ring));
        assert!(!offered(runner.state(), other(caster), ring));
        runner
            .cast(ring)
            .try_resolve()
            .expect("the granted cast resolves");
        assert_eq!(
            runner.state().objects[&ring].zone,
            Zone::Battlefield,
            "reach"
        );
        assert_eq!(runner.state().objects[&ring].owner, caster);
    }
}

fn back_face_payloads(state: &GameState, player: PlayerId, object: ObjectId) -> Vec<String> {
    let mut names = Vec::new();
    for_each_structurally_selectable_alternate_spell_payload(state, player, object, |payload| {
        if let StructurallySelectableAlternateSpellPayload::BackFace(face) = payload {
            names.push(face.name.clone());
        }
    });
    names
}

#[test]
fn disturb_cross() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        for format in formats() {
            let owner = if is_dandan(&format) {
                other(caster)
            } else {
                caster
            };
            let (mut runner, id) = graveyard_board(db, format, owner, "Lunarch Veteran", caster);
            assert_eq!(
                back_face_payloads(runner.state(), caster, id),
                ["Luminous Phantom"]
            );
            runner
                .cast(id)
                .casting_variant(CastingVariant::Disturb)
                .try_resolve()
                .expect("the disturb cast resolves");
            let permanent = &runner.state().objects[&id];
            assert_eq!(permanent.owner, caster);
            assert_eq!(
                (permanent.zone, permanent.name.as_str()),
                (Zone::Battlefield, "Luminous Phantom"),
                "the card resolves transformed"
            );
        }
        let (mut runner, theirs) = graveyard_board(
            db,
            FormatConfig::standard(),
            other(caster),
            "Lunarch Veteran",
            caster,
        );
        assert!(back_face_payloads(runner.state(), caster, theirs).is_empty());
        assert!(runner
            .cast(theirs)
            .casting_variant(CastingVariant::Disturb)
            .try_resolve()
            .is_err());
    }
}

#[test]
fn delve_cross() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        for format in formats() {
            let shared = is_dandan(&format);
            let mut scenario = scenario(format);
            let cruise = scenario.add_real_card(caster, "Treasure Cruise", Zone::Hand, db);
            let own = stage(&mut scenario, db, Zone::Graveyard, &[(caster, "Island"); 3]);
            let fuel = stage(
                &mut scenario,
                db,
                Zone::Graveyard,
                &[(other(caster), "Island"); 3],
            );
            for _ in 0..3 {
                scenario.add_real_card(caster, "Island", Zone::Library, db);
            }
            let mut runner = start(scenario, caster);
            let cast = runner.cast(cruise).delve_with(&fuel).try_resolve();
            let exiled = |ids: &[ObjectId]| {
                ids.iter()
                    .filter(|id| runner.state().objects[*id].zone == Zone::Exile)
                    .count()
            };
            if shared {
                cast.expect("the delve cast resolves");
                assert_eq!(exiled(&fuel), 3, "the other seat's pile cards fuel delve");
            } else {
                assert_eq!(
                    exiled(&own),
                    0,
                    "reach: the caster's own cards are untouched"
                );
                assert_eq!(
                    exiled(&fuel),
                    0,
                    "the other seat's graveyard cannot fuel delve"
                );
            }
        }
    }
}

#[test]
fn mayhem_discarder() {
    let Some(db) = shared_card_db() else { return };
    for discarder in [P0, P1] {
        for caster in [discarder, other(discarder)] {
            let mut scenario = scenario(dandan());
            let slash = stage(
                &mut scenario,
                db,
                Zone::Graveyard,
                &[(discarder, "Sadistic Slash")],
            )[0];
            let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
            let mut runner = start(scenario, caster);
            engine::game::restrictions::record_card_discarded(runner.state_mut(), slash);
            let may = caster == discarder;
            assert_eq!(offered(runner.state(), caster, slash), may);
            let cast = runner.cast(slash).target_object(bears).try_resolve();
            assert_eq!(cast.is_ok(), may, "{:?}", cast.err());
        }
    }
}

#[test]
fn resident_gravecrawler_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for zombie in [P0, P1] {
        let mut scenario = scenario(dandan());
        let crawler = stage(&mut scenario, db, Zone::Graveyard, &[(P0, "Gravecrawler")])[0];
        let ghoul = scenario.add_real_card(zombie, "Diregraf Ghoul", Zone::Battlefield, db);
        let mut runner = start(scenario, zombie);
        assert_eq!(
            runner.state().objects[&ghoul].controller,
            zombie,
            "reach: the Zombie"
        );
        assert!(!offered(runner.state(), other(zombie), crawler));
        assert!(offered(runner.state(), zombie, crawler));
        runner
            .cast(crawler)
            .try_resolve()
            .expect("the Zombie's seat casts it");
        assert_eq!(
            runner.state().objects[&crawler].zone,
            Zone::Battlefield,
            "reach"
        );
        assert_eq!(runner.state().objects[&crawler].owner, zombie);
    }

    for zombie in [P0, P1] {
        let mut scenario = scenario(FormatConfig::standard());
        let crawler = stage(&mut scenario, db, Zone::Graveyard, &[(P0, "Gravecrawler")])[0];
        let ghoul = scenario.add_real_card(zombie, "Diregraf Ghoul", Zone::Battlefield, db);
        let runner = start(scenario, zombie);
        assert_eq!(
            runner.state().objects[&ghoul].zone,
            Zone::Battlefield,
            "reach"
        );
        assert_eq!(offered(runner.state(), zombie, crawler), zombie == P0);
    }
}

#[test]
fn resident_life_gained_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for gainer in [P0, P1] {
        let (mut runner, vampire) = graveyard_board(db, dandan(), P0, "Oathsworn Vampire", gainer);
        assert!(
            !offered(runner.state(), gainer, vampire),
            "reach: no life gained yet"
        );
        runner.state_mut().players[gainer.0 as usize].life_gained_this_turn = 3;
        assert!(offered(runner.state(), gainer, vampire));
        assert!(!offered(runner.state(), other(gainer), vampire));
    }
}

#[test]
fn resident_unconditional_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        for format in formats() {
            let owner = if is_dandan(&format) {
                other(caster)
            } else {
                caster
            };
            let (mut runner, veteran) =
                graveyard_board(db, format, owner, "Hundred-Battle Veteran", caster);
            assert!(offered(runner.state(), caster, veteran));
            runner
                .cast(veteran)
                .try_resolve()
                .expect("the resident cast resolves");
            assert_eq!(runner.state().objects[&veteran].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&veteran].owner, caster);
        }
        let (runner, theirs) = graveyard_board(
            db,
            FormatConfig::standard(),
            other(caster),
            "Hundred-Battle Veteran",
            caster,
        );
        assert!(!offered(runner.state(), caster, theirs));
    }
}

#[test]
fn resident_controller_count_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for holder in [P0, P1] {
        for tapped in [3, 2] {
            let mut scenario = scenario(dandan());
            let indomitable = stage(
                &mut scenario,
                db,
                Zone::Graveyard,
                &[(P0, "The Indomitable")],
            )[0];
            let copters = stage(
                &mut scenario,
                db,
                Zone::Battlefield,
                &vec![(holder, "Smuggler's Copter"); tapped],
            );
            let mut runner = start(scenario, holder);
            for copter in &copters {
                runner.state_mut().objects.get_mut(copter).unwrap().tapped = true;
            }
            assert_eq!(offered(runner.state(), holder, indomitable), tapped == 3);
            assert!(!offered(runner.state(), other(holder), indomitable));
        }
    }
}

#[test]
fn resident_max_speed_reads_caster() {
    let Some(db) = shared_card_db() else { return };
    let mut differing = 0;
    for format in formats() {
        for owner in [P0, P1] {
            for (s0, s1) in [(4, 1), (1, 4), (4, 4), (1, 1)] {
                let mut scenario = scenario(format.clone());
                let cards = stage(
                    &mut scenario,
                    db,
                    Zone::Graveyard,
                    &[
                        (owner, "Lightwheel Enhancements"),
                        (owner, "Hundred-Battle Veteran"),
                    ],
                );
                let (lightwheel, veteran) = (cards[0], cards[1]);
                let mut runner = start(scenario, P0);
                set_speed(&mut runner, P0, s0);
                set_speed(&mut runner, P1, s1);
                for seat in [P0, P1] {
                    let speed = if seat == P0 { s0 } else { s1 };
                    let owner_speed = if owner == P0 { s0 } else { s1 };
                    let reads = is_dandan(&format) || seat == owner;
                    assert_eq!(offered(runner.state(), seat, veteran), reads, "reach");
                    assert_eq!(
                        offered(runner.state(), seat, lightwheel),
                        reads && speed == 4,
                        "owner {owner:?}, speeds ({s0}, {s1}), seat {seat:?}"
                    );
                    if is_dandan(&format) && speed != owner_speed {
                        differing += 1;
                    }
                }
            }
        }
    }
    assert!(
        differing > 0,
        "reach: cells where the seat's and the owner's speed differ"
    );

    for (owner_speed, caster_speed) in [(1, 4), (4, 1)] {
        let mut scenario = scenario(dandan());
        let lightwheel = stage(
            &mut scenario,
            db,
            Zone::Graveyard,
            &[(P0, "Lightwheel Enhancements")],
        )[0];
        let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
        let mut runner = start(scenario, P1);
        set_speed(&mut runner, P0, owner_speed);
        set_speed(&mut runner, P1, caster_speed);
        assert_eq!(
            offered(runner.state(), P0, lightwheel),
            owner_speed == 4,
            "reach"
        );
        let may = caster_speed == 4;
        assert_eq!(offered(runner.state(), P1, lightwheel), may);
        let cast = runner.cast(lightwheel).target_object(bears).try_resolve();
        assert_eq!(cast.is_ok(), may);
        let expected = if may {
            Zone::Battlefield
        } else {
            Zone::Graveyard
        };
        assert_eq!(runner.state().objects[&lightwheel].zone, expected);
    }
}

fn modal_cap(runner: &GameRunner, seat: PlayerId, card: ObjectId) -> usize {
    let modal = runner.state().objects[&card]
        .modal
        .clone()
        .expect("a modal spell");
    engine::game::ability_utils::modal_choice_for_player(
        runner.state(),
        seat,
        card,
        &modal,
        &SpellContext::default(),
    )
    .max_choices
}

#[test]
fn surge_modal_cap_reads_caster() {
    let Some(db) = shared_card_db() else { return };
    for format in formats() {
        for zone in [Zone::Graveyard, Zone::Exile] {
            for owner in [P0, P1] {
                let mut scenario = scenario(format.clone());
                let surge = stage(&mut scenario, db, zone, &[(owner, "Surge of Acclaim")])[0];
                let mut runner = start(scenario, P0);
                set_speed(&mut runner, owner, 1);
                set_speed(&mut runner, other(owner), 4);
                assert_eq!(modal_cap(&runner, owner, surge), 1);
                assert_eq!(modal_cap(&runner, other(owner), surge), 2);
            }
        }
    }

    for (owner_speed, caster_speed) in [(1, 4), (4, 1)] {
        let mut scenario = scenario(dandan());
        let surge = stage(
            &mut scenario,
            db,
            Zone::Graveyard,
            &[(P0, "Surge of Acclaim")],
        )[0];
        let fodder = scenario.add_real_card(P1, "Grizzly Bears", Zone::Hand, db);
        let mut runner = start(scenario, P1);
        set_speed(&mut runner, P0, owner_speed);
        set_speed(&mut runner, P1, caster_speed);
        assert!(
            offered(runner.state(), P1, surge),
            "reach: jump-start offers the pile card"
        );
        let cast = runner
            .cast(surge)
            .modes(&[0, 1])
            .pay_cost_with(&[fodder])
            .try_resolve();
        assert_eq!(cast.is_ok(), caster_speed == 4);
    }
}

#[test]
fn resident_seatless_condition_both_seats() {
    let Some(db) = shared_card_db() else { return };
    for format in formats() {
        for dier in [P0, P1] {
            let mut scenario = scenario(format.clone());
            let cards = stage(
                &mut scenario,
                db,
                Zone::Graveyard,
                &[(P0, "Ebondeath, Dracolich"), (P0, "Undead Sprinter")],
            );
            let bears = scenario.add_real_card(dier, "Grizzly Bears", Zone::Battlefield, db);
            let mut runner = start(scenario, P0);
            for &card in &cards {
                for seat in [P0, P1] {
                    assert!(!offered(runner.state(), seat, card), "no death yet");
                }
            }
            move_to_zone(runner.state_mut(), bears, Zone::Graveyard, &mut Vec::new());
            assert_eq!(
                runner.state().objects[&bears].zone,
                Zone::Graveyard,
                "reach: died"
            );
            for &card in &cards {
                for seat in [P0, P1] {
                    let reads = is_dandan(&format) || seat == P0;
                    assert_eq!(offered(runner.state(), seat, card), reads, "{seat:?}");
                }
            }
        }
    }
}

fn impulse_grant(to: PlayerId) -> CastingPermission {
    CastingPermission::PlayFromExile {
        provenance: PlayFromExileProvenance::Impulse,
        duration: Duration::UntilEndOfTurn,
        granted_to: to,
        mode: CardPlayMode::Cast,
        frequency: CastFrequency::Unlimited,
        source_id: None,
        invalidation: None,
        exiled_by_ability_controller: None,
        mana_spend_permission: None,
        card_filter: None,
        single_use_group: None,
        single_use: false,
        cast_cost_modifier: None,
        alt_ability_cost: None,
        land_enter_tapped: EtbTapState::Unspecified,
    }
}

#[test]
fn exile_cast_dandan_vs_standard() {
    let Some(db) = shared_card_db() else { return };
    for format in formats() {
        let shared = is_dandan(&format);
        let mut scenario = scenario(format);
        let bolt = stage(&mut scenario, db, Zone::Exile, &[(P0, "Lightning Bolt")])[0];
        let mut runner = start(scenario, P1);
        runner
            .state_mut()
            .objects
            .get_mut(&bolt)
            .unwrap()
            .casting_permissions
            .push(impulse_grant(P1));
        let commit = runner.cast(bolt).target_player(P0).commit();
        assert_eq!(commit.state().objects[&bolt].zone, Zone::Stack, "reach");
        assert_eq!(
            commit.state().objects[&bolt].owner,
            if shared { P1 } else { P0 }
        );
    }
}

#[test]
fn control_magic_and_serde() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = scenario(dandan());
    let bears = stage(
        &mut scenario,
        db,
        Zone::Library,
        &[(P0, "Grizzly Bears"), (P0, "Island")],
    )[0];
    scenario.add_real_card(P1, "Future Sight", Zone::Battlefield, db);
    let control_magic = scenario.add_real_card(P0, "Control Magic", Zone::Hand, db);
    let unsubstantiate = scenario.add_real_card(P0, "Unsubstantiate", Zone::Hand, db);
    scenario.with_mana_pool(P0, plenty_of_mana());
    let mut runner = start(scenario, P1);
    reveal_top(&mut runner);
    assert_eq!(
        runner.state().objects[&bears].owner,
        P0,
        "reach: staged owner"
    );

    let commit = runner.cast(bears).commit();
    let restored = state_round_trip(commit.state());
    assert_eq!(restored.objects[&bears].zone, Zone::Stack);
    assert_eq!(restored.objects[&bears].owner, P1);
    commit.resolve();

    give_priority(&mut runner, P0);
    runner.cast(control_magic).target_object(bears).resolve();
    let stolen = &runner.state().objects[&bears];
    assert_eq!((stolen.owner, stolen.controller), (P1, P0));

    runner.cast(unsubstantiate).target_object(bears).resolve();
    assert_eq!(hand_holder(runner.state(), bears), Some(P1));
}

#[test]
fn reanimate_keeps_owner() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let mut scenario = scenario(dandan());
        let bears = stage(
            &mut scenario,
            db,
            Zone::Graveyard,
            &[(other(caster), "Grizzly Bears")],
        )[0];
        let reanimate = scenario.add_real_card(caster, "Reanimate", Zone::Hand, db);
        let mut runner = start(scenario, caster);
        runner.cast(reanimate).target_object(bears).resolve();
        let object = &runner.state().objects[&bears];
        assert_eq!(object.zone, Zone::Battlefield, "reach");
        assert_eq!((object.owner, object.controller), (other(caster), caster));
    }
}

#[test]
fn blink_keeps_owner() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let mut scenario = scenario(dandan());
        let bears = scenario.add_real_card(other(caster), "Grizzly Bears", Zone::Battlefield, db);
        let control_magic = scenario.add_real_card(caster, "Control Magic", Zone::Hand, db);
        let cloudshift = scenario.add_real_card(caster, "Cloudshift", Zone::Hand, db);
        let mut runner = start(scenario, caster);
        runner.cast(control_magic).target_object(bears).resolve();
        assert_eq!(
            runner.state().objects[&bears].controller,
            caster,
            "reach: stolen"
        );
        runner.cast(cloudshift).target_object(bears).resolve();
        let object = &runner.state().objects[&bears];
        assert_eq!(object.zone, Zone::Battlefield, "reach");
        assert_eq!((object.owner, object.controller), (other(caster), caster));
    }
}

#[test]
fn graveyard_activation_stays_owner() {
    let Some(db) = shared_card_db() else { return };
    for actor in [P0, P1] {
        let (mut runner, zombie) = graveyard_board(db, dandan(), P0, "Dregscape Zombie", actor);
        let ability_index = activated_index(&runner, zombie);
        let activation = runner.act(GameAction::ActivateAbility {
            source_id: zombie,
            ability_index,
        });
        assert_eq!(activation.is_ok(), actor == P0, "{actor:?}");
    }
}
