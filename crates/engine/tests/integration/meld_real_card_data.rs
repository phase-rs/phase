//! CR 701.42a + CR 712.4: meld resolves against the real card-data export.
//!
//! MTGJSON publishes a meld pair as three single-face groups; the loader
//! regroups each front with its combined back so the export carries the
//! `meld` layout the meld-pair registry is derived from. Nothing here is
//! hand-seeded — both `meld_pair_registry` and `card_face_registry` come from
//! production rehydration, so a data-pipeline regression that leaves the
//! registry empty (both cards stranded in exile) fails this test.

use engine::game::log::resolve_log_entries;
use engine::game::meld::perform_meld;
use engine::game::printed_cards::rehydrate_game_from_card_db;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::{create_debug_cards, debug_card_entry_source, DebugCardCreateRequest};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::DebugCardCreationKind;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::log::LogSegment;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const GISELA: &str = "Gisela, the Broken Blade";
const BRUNA: &str = "Bruna, the Fading Light";
const BRISELA: &str = "Brisela, Voice of Nightmares";

/// A meld pair debug-spawned into a game that started without it must still
/// meld: the combined back face has to reach the game's outside-the-game face
/// registry with the spawned card, not only from the decks at game start.
#[test]
fn debug_spawned_meld_pair_melds_mid_game() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut state = GameState::new(FormatConfig::standard().with_sandbox(), 2, 42);
    state.debug_mode = true;
    rehydrate_game_from_card_db(&mut state, db);
    assert!(
        !state
            .card_face_registry
            .contains_key(&BRISELA.to_lowercase()),
        "precondition: nothing in the starting game reaches the combined face"
    );

    let mut spawned = Vec::new();
    for name in [GISELA, BRUNA] {
        let face = db
            .get_face_by_name(name)
            .expect("meld card is in the card data");
        create_debug_cards(
            &mut state,
            DebugCardCreateRequest {
                actor: P0,
                source: debug_card_entry_source(db, face),
                owner: P0,
                zone: Zone::Battlefield,
                count: 1,
                attach_to: None,
                run_etb: false,
                nonlegendary: false,
                creation_kind: DebugCardCreationKind::Card,
            },
        )
        .expect("debug spawn succeeds");
        spawned.push(
            *state
                .battlefield
                .last()
                .expect("spawned onto the battlefield"),
        );
    }
    let [gisela, bruna] = spawned[..] else {
        unreachable!("two cards were spawned");
    };

    let mut events = Vec::new();
    perform_meld(
        &mut state,
        &ResolvedAbility::new(gisela_meld_effect(db), Vec::new(), gisela, P0),
        &mut events,
    )
    .expect("meld resolves");

    assert_eq!(state.objects[&gisela].zone, Zone::Battlefield);
    assert_eq!(state.objects[&gisela].name, BRISELA);
    assert_eq!(
        state.objects[&gisela].merged_components,
        vec![gisela, bruna]
    );
}

fn gisela_meld_effect(db: &engine::database::card_db::CardDatabase) -> Effect {
    db.get_face_by_name(GISELA)
        .expect("Gisela is in the card data")
        .triggers
        .iter()
        .filter_map(|trigger| trigger.execute.as_deref())
        .find(|execute| matches!(execute.effect.as_ref(), Effect::Meld { .. }))
        .map(|execute| (*execute.effect).clone())
        .expect("Gisela's end-step trigger parses to Effect::Meld")
}

#[test]
fn gisela_and_bruna_meld_into_brisela_with_real_card_data() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let meld_effect = gisela_meld_effect(db);

    let mut sc = GameScenario::new();
    let gisela = sc.add_real_card(P0, GISELA, Zone::Battlefield, db);
    let bruna = sc.add_real_card(P0, BRUNA, Zone::Battlefield, db);
    let mut runner = sc.build();
    let state = runner.state_mut();
    rehydrate_game_from_card_db(state, db);
    let before = state.clone();

    let mut events = Vec::new();
    perform_meld(
        state,
        &ResolvedAbility::new(meld_effect, Vec::new(), gisela, P0),
        &mut events,
    )
    .expect("meld resolves");

    // CR 701.42a: one permanent, represented by both cards, with the combined
    // back face's characteristics.
    let melded = &state.objects[&gisela];
    assert_eq!(melded.zone, Zone::Battlefield);
    assert_eq!(melded.name, BRISELA);
    assert_eq!(melded.merged_components, vec![gisela, bruna]);
    assert!(state.battlefield.contains(&gisela));
    assert!(
        !state.exile.contains(&gisela) && !state.exile.contains(&bruna),
        "neither card is left behind in exile"
    );

    // CR 712.4b: the melded permanent presents the combined card's own printed
    // identity — not a front's copy of it — which is what its image resolves from.
    let combined_oracle = db
        .get_face_by_name(BRISELA)
        .and_then(|face| face.scryfall_oracle_id.clone());
    let printed = melded
        .printed_ref
        .as_ref()
        .expect("the melded permanent has a printed identity");
    assert_eq!(printed.face_name, BRISELA);
    assert_eq!(Some(&printed.oracle_id), combined_oracle.as_ref());

    assert!(
        events.contains(&GameEvent::Melded {
            object_id: gisela,
            partner_id: bruna,
            controller: P0,
        }),
        "the meld is announced once the melded permanent is on the battlefield"
    );

    // The log names both physical cards by their printed fronts and the
    // melded permanent by its combined face.
    let card_names = |segments: &[LogSegment]| -> Vec<String> {
        segments
            .iter()
            .filter_map(|segment| match segment {
                LogSegment::CardName { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    };
    let meld_line = resolve_log_entries(&events, &before, state)
        .into_iter()
        .find(|entry| {
            entry
                .segments
                .iter()
                .any(|segment| matches!(segment, LogSegment::Text(text) if text == " meld into "))
        })
        .expect("the meld is logged");
    assert_eq!(card_names(&meld_line.segments), [GISELA, BRUNA, BRISELA]);
}
