//! Abdel Adrian + Animate Dead, with Altar of the Brood as the enabler.
//!
//! A reusable four-seat board built from cards the committed integration card
//! fixture carries, placed with `GameScenarioDbExt::add_real_card` and driven
//! only through `game::engine::apply()`.
//!
//! | Card | Seat / zone | Why it is here |
//! |---|---|---|
//! | Abdel Adrian, Gorion's Ward | P0 graveyard | the creature Animate Dead returns; its enters trigger is what the row below drives |
//! | Animate Dead | P0 hand | returns Abdel Adrian, and is itself one of the nonland permanents the enters trigger offers |
//! | Altar of the Brood | P0 battlefield | the enabler, and the other nonland permanent Abdel Adrian may exile |
//! | two basic Swamps | P0 battlefield | P0's lands; the enters trigger names *nonland* permanents, so they stay out of its choice |
//! | seeded `{B}{B}` | P0's mana pool | what pays Animate Dead's `{1}{B}`; the drive taps nothing and activates no mana ability |
//! | basic Swamps, `LIBRARY_PER_SEAT` each | every seat's library | sized so no seat decks out under the decline policy below |
//!
//! Verbatim Oracle text, as the fixture stores it:
//!   Abdel Adrian, Gorion's Ward: "When Abdel Adrian enters, exile any number
//!     of other nonland permanents you control until Abdel Adrian leaves the
//!     battlefield. Create a 1/1 white Soldier creature token for each permanent
//!     exiled this way."
//!   Animate Dead: "Enchant creature card in a graveyard / When this Aura
//!     enters, if it's on the battlefield, it loses "enchant creature card in a
//!     graveyard" and gains "enchant creature put onto the battlefield with this
//!     Aura." Return enchanted creature card to the battlefield under your
//!     control and attach this Aura to it. When this Aura leaves the
//!     battlefield, that creature's controller sacrifices it. / Enchanted
//!     creature gets -1/-0."
//!   Altar of the Brood: "Whenever another permanent you control enters, each
//!     opponent mills a card."
//!
//! **The decline policy.** Abdel Adrian's exile is voluntary, since zero is a
//! number. The drive answers the FIRST resolution of the enters trigger with
//! both other nonland permanents P0 controls, and answers every later choice of
//! that kind with an empty selection. Answering every choice in full instead
//! does not reach a fixed point: it hands out successive choices of the same
//! kind and drains a seat's library on the way. The per-seat library is sized
//! for the declared policy and for nothing else.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
const SEATS: [PlayerId; 4] = [P0, P1, P2, P3];

/// Basic lands per seat. Read by no assertion here; it exists so the settle
/// below cannot end on a draw from an empty library.
const LIBRARY_PER_SEAT: usize = 12;

/// The built board, with the ids a driver needs.
pub(super) struct AbdelAnimateAltarBoard {
    pub(super) runner: GameRunner,
    pub(super) abdel: ObjectId,
    pub(super) animate_dead: ObjectId,
    pub(super) altar: ObjectId,
}

/// Build the board. `None` when neither the committed fixture nor a full card
/// export is available, which is the only condition under which a driver skips.
pub(super) fn build() -> Option<AbdelAnimateAltarBoard> {
    let db = load_db()?;
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let abdel = scenario.add_real_card(P0, "Abdel Adrian, Gorion's Ward", Zone::Graveyard, db);
    let animate_dead = scenario.add_real_card(P0, "Animate Dead", Zone::Hand, db);
    let altar = scenario.add_real_card(P0, "Altar of the Brood", Zone::Battlefield, db);
    for _ in 0..2 {
        scenario.add_real_card(P0, "Swamp", Zone::Battlefield, db);
    }
    for seat in SEATS {
        for _ in 0..LIBRARY_PER_SEAT {
            scenario.add_real_card(seat, "Swamp", Zone::Library, db);
        }
    }
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );

    Some(AbdelAnimateAltarBoard {
        runner: scenario.build(),
        abdel,
        animate_dead,
        altar,
    })
}

/// The permanents the enters trigger is offering, or `None` when the drive is
/// not parked on that choice.
fn offered_zone_choice(runner: &GameRunner) -> Option<Vec<ObjectId>> {
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => Some(cards.clone()),
        _ => None,
    }
}

fn soldier_tokens(runner: &GameRunner) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|object| object.name == "Soldier")
        })
        .count()
}

/// Answer every later choice of the enters trigger's kind with an empty
/// selection (the declared decline) until the drive leaves that state.
fn settle_under_decline(runner: &mut GameRunner) {
    for _ in 0..64 {
        if offered_zone_choice(runner).is_none() {
            break;
        }
        runner
            .act(GameAction::SelectCards { cards: vec![] })
            .expect("declining the voluntary exile is a legal answer");
    }
    runner.advance_until_stack_empty();
}

/// CR 608.2c + CR 608.2h: "exile any number of other nonland permanents you
/// control ... Create a 1/1 white Soldier creature token for each permanent
/// exiled this way" — the count names what the earlier instruction exiled, read
/// by last known information once those permanents have left the battlefield.
#[test]
fn abdel_adrian_mints_one_soldier_per_permanent_the_choice_exiled() {
    let Some(mut board) = build() else {
        return;
    };

    let outcome = board
        .runner
        .cast(board.animate_dead)
        .target_objects(&[board.abdel])
        .resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::EffectZoneChoice { .. }
        ),
        "returning Abdel Adrian must park on its enters trigger's exile choice"
    );

    let offered = offered_zone_choice(&board.runner).expect("parked on the exile choice");
    let mut offered_sorted = offered.clone();
    offered_sorted.sort_unstable_by_key(|id| id.0);
    let mut nonland_permanents = vec![board.altar, board.animate_dead];
    nonland_permanents.sort_unstable_by_key(|id| id.0);
    assert_eq!(
        offered_sorted, nonland_permanents,
        "the choice offers exactly the two other nonland permanents P0 controls"
    );

    board
        .runner
        .act(GameAction::SelectCards {
            cards: offered.clone(),
        })
        .expect("selecting every offered permanent is a legal answer");

    // Reach guard, taken at the beat the answer returns and before the board
    // settles: both legs stop holding once exiling Animate Dead severs the Aura
    // and the "until Abdel Adrian leaves the battlefield" exile ends.
    let published = {
        let state = board.runner.state();
        for id in &offered {
            assert_eq!(
                state.objects.get(id).map(|object| object.zone),
                Some(Zone::Exile),
                "each selected permanent is in exile at the publication beat"
            );
        }
        let chain = state
            .chain_tracked_set_id
            .expect("the choice published a chain tracked set");
        let mut members = state
            .tracked_object_sets
            .get(&chain)
            .cloned()
            .unwrap_or_default();
        members.sort_unstable_by_key(|id| id.0);
        let mut expected = offered.clone();
        expected.sort_unstable_by_key(|id| id.0);
        assert_eq!(
            members, expected,
            "the published set holds exactly the selected permanents"
        );
        chain
    };

    // CR 608.2c: the count reads the members this choice exiled. Reverting the
    // routing at the zone-choice publication leaves the side map empty for this
    // set and the count reads zero.
    assert_eq!(
        board
            .runner
            .state()
            .tracked_set_member_causes
            .get(&published)
            .map(|by_object| by_object.len()),
        Some(offered.len()),
        "every member carries the action the choice named"
    );
    assert_eq!(
        soldier_tokens(&board.runner),
        offered.len(),
        "one Soldier per permanent exiled this way"
    );

    settle_under_decline(&mut board.runner);
}
