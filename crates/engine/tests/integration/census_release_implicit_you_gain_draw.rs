//! Runtime witnesses for the six `wrong_player` rows released from `confirmed-misparses.toml`
//! for the implicit-"you" life-gain and draw replacement class. Every cited clause is a
//! replacement whose parsed player scope is left implicit (`valid_player: None`), which the
//! census read as "no player scope". At runtime the implicit scope is the replacement's
//! controller (CR 109.5), so each test pins both halves: the controller's event is replaced
//! and another player's event is not.
//!
//! Released faces and their cited clauses (MTGJSON `oracle_text`, checked against Scryfall):
//! - Alhammarret's Archive: [`ARCHIVE`] (both clauses).
//! - The Wind Crystal: "If you would gain life, you gain twice that much life instead."
//! - Bilbo, Birthday Celebrant; Cleric Class (level 1); Knight of Dawn's Light:
//!   "If you would gain life, you gain that much life plus 1 instead."
//! - Mr. Spock, Science Officer: [`SPOCK`].
//!
//! Each face's full printed text is asserted against the card database in [`card`], so a
//! paraphrased or drifted face fails before any behavior is measured.
//!
//! One real defect sat in the Archive's draw clause: on a shared team turn (Two-Headed Giant,
//! CR 805.4) the teammate who is not `state.active_player` also draws in that team's draw step
//! (CR 805.4b) and is an active player of it (CR 805.9), yet the replacement exemption for "the
//! first one you draw in each of your draw steps" only excused `state.active_player`. The
//! 2HG tests below pin the fix for Archive and for Notion Thief, the they/their sibling.
//!
//! Every card, including the spells and library fillers, is the printed card loaded through
//! `add_real_card`; the scenarios supply boards, libraries and mana pools and drive the cards
//! through public `GameRunner` actions.

use engine::game::players::team_life_total;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const ARCHIVE: &str = "If you would gain life, you gain twice that much life instead.\nIf you would draw a card except the first one you draw in each of your draw steps, draw two cards instead.";
const WIND_CRYSTAL: &str = "White spells you cast cost {1} less to cast.\nIf you would gain life, you gain twice that much life instead.\n{4}{W}{W}, {T}: Creatures you control gain flying and lifelink until end of turn.";
const BILBO: &str = "If you would gain life, you gain that much life plus 1 instead.\n{2}{W}{B}{G}, {T}, Exile Bilbo: Search your library for any number of creature cards, put them onto the battlefield, then shuffle. Activate only if you have 111 or more life.";
const CLERIC_CLASS: &str = "(Gain the next level as a sorcery to add its ability.)\nIf you would gain life, you gain that much life plus 1 instead.\n{3}{W}: Level 2\nWhenever you gain life, put a +1/+1 counter on target creature you control.\n{4}{W}: Level 3\nWhen this Class becomes level 3, return target creature card from your graveyard to the battlefield. You gain life equal to that creature's toughness.";
const KNIGHT: &str = "First strike\nIf you would gain life, you gain that much life plus 1 instead.\n{1}{W}: This creature gets +1/+1 until end of turn.";
const SPOCK: &str = "If you would draw a card, draw two cards instead.";
const NOTION_THIEF: &str = "Flash\nIf an opponent would draw a card except the first one they draw in each of their draw steps, instead that player skips that draw and you draw a card.";

/// A printed card and the Oracle text the test relies on.
#[derive(Clone, Copy)]
struct Printed {
    name: &'static str,
    oracle: &'static str,
}

const ALHAMMARRETS_ARCHIVE: Printed = Printed {
    name: "Alhammarret's Archive",
    oracle: ARCHIVE,
};
const THE_WIND_CRYSTAL: Printed = Printed {
    name: "The Wind Crystal",
    oracle: WIND_CRYSTAL,
};
const DOUBLERS: [Printed; 2] = [ALHAMMARRETS_ARCHIVE, THE_WIND_CRYSTAL];
const PLUS_ONE: [Printed; 3] = [
    Printed {
        name: "Bilbo, Birthday Celebrant",
        oracle: BILBO,
    },
    Printed {
        name: "Cleric Class",
        oracle: CLERIC_CLASS,
    },
    Printed {
        name: "Knight of Dawn's Light",
        oracle: KNIGHT,
    },
];
const MR_SPOCK: Printed = Printed {
    name: "Mr. Spock, Science Officer",
    oracle: SPOCK,
};
const DRAW_DOUBLERS: [Printed; 2] = [ALHAMMARRETS_ARCHIVE, MR_SPOCK];

const SOOTHING_BALM: &str = "Soothing Balm";
const THINK_TWICE: &str = "Think Twice";
const INSPIRATION: &str = "Inspiration";

/// Places a printed card after checking its database face carries the Oracle text the test
/// relies on.
fn card(s: &mut GameScenario, player: PlayerId, printed: Printed, zone: Zone) -> ObjectId {
    let db = shared_card_db().expect("printed fixture");
    let face = db
        .get_face_by_name(printed.name)
        .unwrap_or_else(|| panic!("{} missing from the fixture", printed.name));
    assert_eq!(
        face.oracle_text.as_deref(),
        Some(printed.oracle),
        "{} Oracle text",
        printed.name
    );
    s.add_real_card(player, printed.name, zone, db)
}

/// Places a printed spell or filler whose text the test does not quote.
fn spell(s: &mut GameScenario, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    s.add_real_card(
        player,
        name,
        zone,
        shared_card_db().expect("printed fixture"),
    )
}

fn mana() -> Vec<ManaUnit> {
    [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ]
    .into_iter()
    .flat_map(|kind| (0..12).map(move |_| ManaUnit::new(kind, ObjectId(0), false, vec![])))
    .collect()
}

// CR 106.4: Seed a mana pool to pay printed costs.
fn fund(s: &mut GameScenario, player: PlayerId) {
    s.with_mana_pool(player, mana());
}

// CR 106.4 + CR 500.5: Pools empty as each step ends, so a draw-step cast is funded after the
// step begins.
fn fund_now(r: &mut GameRunner, player: PlayerId) {
    for unit in mana() {
        r.state_mut().add_mana_to_pool(player, unit);
    }
}

fn stock(s: &mut GameScenario, player: PlayerId) {
    for _ in 0..10 {
        spell(s, player, "Island", Zone::Library);
    }
}

/// A two-player game in P0's precombat main phase with funded pools and stocked libraries.
fn table() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        fund(&mut s, player);
        stock(&mut s, player);
    }
    s
}

/// A Two-Headed Giant game (P0+P1 vs P2+P3) in P0's precombat main phase.
fn giant_table() -> GameScenario {
    let mut s = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    s.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2, P3] {
        fund(&mut s, player);
        stock(&mut s, player);
    }
    s
}

/// Puts `active` at the start of a turn after the first, so the draw step is not skipped
/// (CR 103.8a / CR 103.8b).
fn at_untap(r: &mut GameRunner, active: PlayerId) {
    let s = r.state_mut();
    s.turn_number = 2;
    s.phase = Phase::Untap;
    s.active_player = active;
    s.priority_player = active;
    s.waiting_for = WaitingFor::Priority { player: active };
}

fn hand(r: &GameRunner, player: PlayerId) -> usize {
    r.state().players[player.0 as usize].hand.len()
}

fn hands(r: &GameRunner, players: &[PlayerId]) -> Vec<usize> {
    players.iter().map(|&p| hand(r, p)).collect()
}

// CR 504.1 + CR 805.4b: Enter the draw step, whose turn-based draws happen on entry.
fn enter_draw_step(r: &mut GameRunner, active: PlayerId) {
    at_untap(r, active);
    r.advance_to_phase(Phase::Draw);
    assert_eq!(r.state().phase, Phase::Draw);
    assert_eq!(r.state().active_player, active);
}

fn debug_draw(r: &mut GameRunner, player: PlayerId) {
    r.state_mut().debug_mode = true;
    r.act(GameAction::Debug(DebugAction::DrawCards {
        player_id: player,
        count: 1,
    }))
    .expect("debug draw");
    r.advance_until_stack_empty();
}

// CR 117.3b + CR 608.2: Resolution finished and returned priority with an empty stack.
fn settled(out: &CastOutcome) {
    assert!(out.state().stack.is_empty(), "{:?}", out.state().stack);
    assert!(
        matches!(out.final_waiting_for(), WaitingFor::Priority { .. }),
        "{:?}",
        out.final_waiting_for()
    );
}

fn balm(r: &mut GameRunner, spell_id: ObjectId, target: PlayerId) -> CastOutcome {
    let out = r.cast(spell_id).target_player(target).resolve();
    settled(&out);
    out
}

/// CR 109.5 + CR 614.1a + CR 614.6: "If you would gain life, you gain twice that much life
/// instead" doubles its controller's gain and leaves another player's gain alone. The P1 row
/// fails if the implicit player scope regresses to any player.
#[test]
fn gain_doublers_double_only_their_controllers_gain() {
    for doubler in DOUBLERS {
        let mut s = table();
        card(&mut s, P0, doubler, Zone::Battlefield);
        let mine = spell(&mut s, P0, SOOTHING_BALM, Zone::Hand);
        let theirs = spell(&mut s, P0, SOOTHING_BALM, Zone::Hand);
        let mut r = s.build();

        let out = balm(&mut r, mine, P0);
        assert_eq!(out.life_delta(P0), 10, "{}: P0 gains 5 twice", doubler.name);

        let out = balm(&mut r, theirs, P1);
        assert_eq!(
            out.life_delta(P1),
            5,
            "{}: P1's gain is not P0's",
            doubler.name
        );
        assert_eq!(out.life_delta(P0), 0, "{}", doubler.name);
    }
}

/// CR 109.5 + CR 614.1a: "you gain that much life plus 1 instead" adds one to its
/// controller's life-gain event, once per event, and not to another player's. Cleric Class
/// sits at level 1, so its level-2 trigger never goes on the stack.
#[test]
fn plus_one_gainers_add_one_only_to_their_controllers_gain() {
    for gainer in PLUS_ONE {
        let mut s = table();
        card(&mut s, P0, gainer, Zone::Battlefield);
        let mine = spell(&mut s, P0, SOOTHING_BALM, Zone::Hand);
        let theirs = spell(&mut s, P0, SOOTHING_BALM, Zone::Hand);
        let mut r = s.build();

        let out = balm(&mut r, mine, P0);
        assert_eq!(out.life_delta(P0), 6, "{}: 5 plus 1", gainer.name);

        let out = balm(&mut r, theirs, P1);
        assert_eq!(
            out.life_delta(P1),
            5,
            "{}: P1's gain is not P0's",
            gainer.name
        );
        assert_eq!(out.life_delta(P0), 0, "{}", gainer.name);
    }
}

/// CR 121.2 + CR 614.11a + CR 109.5: each of the controller's individual draws becomes two,
/// outside any draw step; another player's draws are untouched.
#[test]
fn draw_doublers_double_each_of_their_controllers_draws_only() {
    for doubler in DRAW_DOUBLERS {
        let mut s = table();
        card(&mut s, P0, doubler, Zone::Battlefield);
        let think = spell(&mut s, P0, THINK_TWICE, Zone::Hand);
        let at_p1 = spell(&mut s, P0, INSPIRATION, Zone::Hand);
        let at_p0 = spell(&mut s, P0, INSPIRATION, Zone::Hand);
        let mut r = s.build();

        let out = r.cast(think).resolve();
        settled(&out);
        out.assert_hand_drawn(P0, 2);

        let out = r.cast(at_p1).target_player(P1).resolve();
        settled(&out);
        out.assert_hand_drawn(P1, 2);
        out.assert_hand_drawn(P0, 0);

        let out = r.cast(at_p0).target_player(P0).resolve();
        settled(&out);
        out.assert_hand_drawn(P0, 4);
    }
}

/// CR 504.1 + CR 121.1 + CR 614.6: the Archive leaves its controller's turn-based first draw
/// of their own draw step alone and doubles a second draw in that step.
#[test]
fn archive_exempts_only_the_first_draw_of_its_controllers_draw_step() {
    let mut s = table();
    card(&mut s, P0, ALHAMMARRETS_ARCHIVE, Zone::Battlefield);
    let think = spell(&mut s, P0, THINK_TWICE, Zone::Hand);
    let mut r = s.build();

    let before = hand(&r, P0);
    enter_draw_step(&mut r, P0);
    assert_eq!(hand(&r, P0), before + 1, "the turn draw stays one card");

    fund_now(&mut r, P0);
    let out = r.cast(think).resolve();
    settled(&out);
    out.assert_hand_drawn(P0, 2);
}

/// CR 504.1: the exemption is "each of your draw steps". The Archive controller's first
/// draw during an opponent's draw step is doubled. Fails if the exemption covers any
/// player's draw step.
#[test]
fn archive_doubles_its_controllers_first_draw_in_an_opponents_draw_step() {
    let mut s = table();
    card(&mut s, P0, ALHAMMARRETS_ARCHIVE, Zone::Battlefield);
    let think = spell(&mut s, P0, THINK_TWICE, Zone::Hand);
    let mut r = s.build();

    let before = hands(&r, &[P0, P1]);
    enter_draw_step(&mut r, P1);
    assert_eq!(hands(&r, &[P0, P1]), vec![before[0], before[1] + 1]);

    r.act(GameAction::PassPriority).expect("P1 passes");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{:?}",
        r.state().waiting_for
    );
    assert_eq!(r.state().players[P0.0 as usize].cards_drawn_this_step, 0);

    fund_now(&mut r, P0);
    let out = r.cast(think).resolve();
    settled(&out);
    assert_eq!(out.state().phase, Phase::Draw);
    out.assert_hand_drawn(P0, 2);
}

/// CR 504.1 + CR 614.1a: Mr. Spock has no draw-step exemption, so even the turn draw is two
/// cards — the contrast that shows the Archive's exemption is its own condition.
#[test]
fn spock_doubles_the_turn_draw() {
    let mut s = table();
    card(&mut s, P0, MR_SPOCK, Zone::Battlefield);
    let mut r = s.build();

    let before = hand(&r, P0);
    enter_draw_step(&mut r, P0);
    assert_eq!(hand(&r, P0), before + 2);
}

/// CR 805.4 + CR 805.4b + CR 805.9: on a shared team turn each player on the active team
/// draws in, and is an active player of, that team's draw step. The Archive controlled by
/// P1, the teammate who is not `state.active_player`, must leave P1's turn draw alone and
/// double P1's next draw in that step.
#[test]
fn two_headed_giant_archive_exempts_its_controllers_shared_turn_draw() {
    let mut s = giant_table();
    card(&mut s, P1, ALHAMMARRETS_ARCHIVE, Zone::Battlefield);
    let mut r = s.build();

    let before = hands(&r, &[P0, P1, P2, P3]);
    enter_draw_step(&mut r, P0);
    assert_eq!(
        hands(&r, &[P0, P1, P2, P3]),
        vec![before[0] + 1, before[1] + 1, before[2], before[3]],
        "each player on the active team draws one card"
    );

    debug_draw(&mut r, P1);
    assert_eq!(hand(&r, P1), before[1] + 3, "P1's second draw is doubled");
}

/// CR 805.4b + CR 805.9 + CR 614.1a: Notion Thief exempts "the first one they draw in each of
/// their draw steps" for every player on the opposing active team, then redirects later
/// draws.
#[test]
fn two_headed_giant_notion_thief_exempts_each_opposing_shared_turn_draw() {
    let mut s = giant_table();
    card(
        &mut s,
        P0,
        Printed {
            name: "Notion Thief",
            oracle: NOTION_THIEF,
        },
        Zone::Battlefield,
    );
    let mut r = s.build();

    let before = hands(&r, &[P0, P1, P2, P3]);
    enter_draw_step(&mut r, P2);
    assert_eq!(
        hands(&r, &[P0, P1, P2, P3]),
        vec![before[0], before[1], before[2] + 1, before[3] + 1],
        "both P2 and P3 keep their turn draws"
    );

    debug_draw(&mut r, P3);
    assert_eq!(hand(&r, P3), before[3] + 1, "P3's second draw is skipped");
    assert_eq!(hand(&r, P0), before[0] + 1, "P0 draws instead");
}

/// CR 810.9 + CR 109.5: in Two-Headed Giant life is gained by each player individually, so a
/// gain doubler affects only its controller and not that player's teammate.
#[test]
fn two_headed_giant_gain_doublers_ignore_the_teammates_gain() {
    for doubler in DOUBLERS {
        let mut s = giant_table();
        card(&mut s, P0, doubler, Zone::Battlefield);
        let teammate = spell(&mut s, P0, SOOTHING_BALM, Zone::Hand);
        let mine = spell(&mut s, P0, SOOTHING_BALM, Zone::Hand);
        let mut r = s.build();

        let team = team_life_total(r.state(), P0);
        let out = balm(&mut r, teammate, P1);
        assert_eq!(out.life_delta(P1), 5, "{}", doubler.name);
        assert_eq!(
            team_life_total(out.state(), P0),
            team + 5,
            "{}",
            doubler.name
        );

        let out = balm(&mut r, mine, P0);
        assert_eq!(out.life_delta(P0), 10, "{}", doubler.name);
    }
}

/// CR 119.5 + CR 614.1a: setting a life total is a life gain of the difference, which the
/// Archive doubles: 3 life "becomes 20" gains 17, doubled to 34, for 37 (Archive ruling).
#[test]
fn archive_doubles_the_gain_from_setting_a_life_total() {
    let mut s = table();
    s.with_life(P0, 3);
    card(&mut s, P0, ALHAMMARRETS_ARCHIVE, Zone::Battlefield);
    let wind = spell(&mut s, P0, "Blessed Wind", Zone::Hand);
    let mut r = s.build();

    let out = r.cast(wind).target_player(P0).resolve();
    settled(&out);
    assert_eq!(out.state().players[P0.0 as usize].life, 37);
}

/// CR 616.1 + CR 616.1f + CR 614.5: two doublers each apply once to the event, so 5 life
/// becomes 20 (Archive ruling: two Archives multiply by four). The affected player orders
/// them; each order gives the same total.
#[test]
fn archive_and_wind_crystal_each_double_the_same_gain() {
    for first in [0, 1] {
        let mut s = table();
        card(&mut s, P0, ALHAMMARRETS_ARCHIVE, Zone::Battlefield);
        card(&mut s, P0, THE_WIND_CRYSTAL, Zone::Battlefield);
        let gain = spell(&mut s, P0, SOOTHING_BALM, Zone::Hand);
        let mut r = s.build();

        let life = r.life(P0);
        let out = r.cast(gain).target_player(P0).resolve();
        assert!(
            matches!(
                out.final_waiting_for(),
                WaitingFor::ReplacementChoice { player, candidate_count: 2, .. } if *player == P0
            ),
            "{:?}",
            out.final_waiting_for()
        );
        r.act(GameAction::ChooseReplacement { index: first })
            .expect("P0 orders the doublers");
        r.advance_until_stack_empty();
        assert!(r.state().stack.is_empty());
        assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(r.life(P0), life + 20, "first choice {first}");
    }
}

/// CR 109.5: "you" is the replacement's current controller. Once P0 controls P1's Archive,
/// P0's gain is doubled and P1's is not.
#[test]
fn archive_follows_its_controller() {
    let mut s = table();
    let archive = card(&mut s, P1, ALHAMMARRETS_ARCHIVE, Zone::Battlefield);
    let steal = spell(&mut s, P0, "Steal Artifact", Zone::Hand);
    let mine = spell(&mut s, P0, SOOTHING_BALM, Zone::Hand);
    let theirs = spell(&mut s, P0, SOOTHING_BALM, Zone::Hand);
    let mut r = s.build();

    let out = r.cast(steal).target_object(archive).resolve();
    settled(&out);
    assert_eq!(out.state().objects[&archive].controller, P0);

    let out = balm(&mut r, mine, P0);
    assert_eq!(out.life_delta(P0), 10);

    let out = balm(&mut r, theirs, P1);
    assert_eq!(out.life_delta(P1), 5);
}
