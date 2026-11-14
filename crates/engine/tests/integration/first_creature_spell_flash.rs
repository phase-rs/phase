//! CR 601.3b + CR 702.8a + CR 302.1: Radagast of Rhosgobel ("The first creature spell you cast
//! each turn costs {2} less to cast and can be cast as though it had flash.") and The Blue Spirit
//! ("You may cast the first creature spell you cast each turn as though it had flash.") let only
//! the first creature spell each turn be cast at instant speed. Both sources are built from
//! their Oracle text so the permission comes from the parser at test time.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityCost, AdditionalCost, QuantityExpr, TargetRef};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{
    AlternativeCastKeyword, CastPaymentMode, ManaChoice, PayCostKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const RADAGAST_OF_RHOSGOBEL: &str = "The first creature spell you cast each turn costs {2} less \
to cast and can be cast as though it had flash.";

const THE_BLUE_SPIRIT: &str = "You may cast the first creature spell you cast each turn as \
though it had flash.\nWhenever a nontoken creature you control enters during combat, draw a card.";

const OPT: &str = "Scry 1. (Look at the top card of your library. You may put that card on the \
bottom.)\nDraw a card.";

struct Board {
    runner: GameRunner,
    bears: [ObjectId; 2],
    opt: ObjectId,
}

/// `source` (2/`toughness`), two Grizzly Bears and Opt for `owner`, with six green and one blue
/// mana floating, during P0's turn at `phase`.
fn board(phase: Phase, owner: PlayerId, source: (&str, i32, &str)) -> Board {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    let (name, toughness, oracle) = source;
    scenario.add_creature_from_oracle(owner, name, 2, toughness, oracle);
    let bears = [
        scenario.add_real_card(owner, "Grizzly Bears", Zone::Hand, db),
        scenario.add_real_card(owner, "Grizzly Bears", Zone::Hand, db),
    ];
    let opt = scenario
        .add_spell_to_hand_from_oracle(owner, "Opt", true, OPT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.with_library_top(owner, &["Library A", "Library B"]);
    let mut mana = vec![ManaUnit::new(ManaType::Green, ObjectId(9_900), false, vec![]); 6];
    mana.push(ManaUnit::new(
        ManaType::Blue,
        ObjectId(9_901),
        false,
        vec![],
    ));
    scenario.with_mana_pool(owner, mana);
    Board {
        runner: scenario.build(),
        bears,
        opt,
    }
}

fn radagast(phase: Phase, owner: PlayerId) -> Board {
    board(
        phase,
        owner,
        ("Radagast of Rhosgobel", 5, RADAGAST_OF_RHOSGOBEL),
    )
}

fn blue_spirit(phase: Phase) -> Board {
    board(phase, P0, ("The Blue Spirit", 4, THE_BLUE_SPIRIT))
}

fn cast(runner: &mut GameRunner, spell: ObjectId) -> bool {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: Default::default(),
        })
        .is_ok()
}

fn pool(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].mana_pool.total()
}

fn on_stack(runner: &GameRunner, spell: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| entry.id == spell)
}

/// Casts both Bears in P0's upkeep, asserting the first is legal at instant speed with the
/// expected mana spent and the second is refused.
fn assert_first_creature_only_in_upkeep(mut b: Board, first_cost: usize) {
    let before = pool(&b.runner, P0);
    assert!(
        cast(&mut b.runner, b.bears[0]),
        "first creature spell has flash"
    );
    assert!(on_stack(&b.runner, b.bears[0]));
    assert_eq!(pool(&b.runner, P0), before - first_cost);
    b.runner.advance_until_stack_empty();
    assert_eq!(b.runner.state().phase, Phase::Upkeep);
    assert!(
        !cast(&mut b.runner, b.bears[1]),
        "second creature spell this turn needs sorcery timing"
    );
}

/// Reach guard: the same board at sorcery timing casts both Bears, so an upkeep refusal is the
/// timing rule rather than mana or permission.
fn assert_both_castable_in_main(mut b: Board) {
    assert!(cast(&mut b.runner, b.bears[0]));
    b.runner.advance_until_stack_empty();
    assert!(cast(&mut b.runner, b.bears[1]));
}

#[test]
fn radagast_first_creature_spell_has_flash_second_does_not() {
    assert_both_castable_in_main(radagast(Phase::PreCombatMain, P0));
    // CR 601.2f: Grizzly Bears' {1}{G} less {2} costs only {G}.
    assert_first_creature_only_in_upkeep(radagast(Phase::Upkeep, P0), 1);
}

#[test]
fn the_blue_spirit_first_creature_spell_has_flash_second_does_not() {
    assert_both_castable_in_main(blue_spirit(Phase::PreCombatMain));
    assert_first_creature_only_in_upkeep(blue_spirit(Phase::Upkeep), 2);
}

#[test]
fn radagast_noncreature_spell_does_not_spend_the_first_creature_window() {
    let Board {
        mut runner,
        bears,
        opt,
    } = radagast(Phase::Upkeep, P0);
    runner
        .cast(opt)
        .try_resolve()
        .expect("Opt is an instant, castable in the upkeep");
    assert!(!on_stack(&runner, opt));
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert!(
        cast(&mut runner, bears[0]),
        "Opt is not a creature spell, so the first creature spell still has flash"
    );
}

#[test]
fn radagast_grants_flash_on_an_opponents_turn() {
    let Board {
        mut runner, bears, ..
    } = radagast(Phase::Upkeep, P1);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority in its upkeep");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 holds priority during P0's upkeep: {:?}",
        runner.state().waiting_for
    );
    assert!(
        cast(&mut runner, bears[0]),
        "first creature spell P1 casts during P0's turn has flash"
    );
    assert!(on_stack(&runner, bears[0]));
}

const LEYLINE_OF_ANTICIPATION: &str = "If this card is in your opening hand, you may begin the \
game with it on the battlefield.\nYou may cast spells as though they had flash.";

const BORNE_UPON_A_WIND: &str =
    "You may cast spells this turn as though they had flash.\nDraw a card.";

const SAVAGE_SUMMONING: &str = "This spell can't be countered.\nThe next creature spell you cast \
this turn can be cast as though it had flash. That spell can't be countered. That creature \
enters with an additional +1/+1 counter on it.";

const CHEA: &str = "Familiar spells you cast have flash. (Bats, Birds, Cats, Dragons, Faeries, \
Foxes, Frogs, Imps, Lizards, and Spiders are familiars, as well as any creature with \"Familiar\" \
in its name.)\n{T}: Add X {G} and each opponent loses X life, where X is the number of familiars \
you control.";

const TEFERI_MAGE_OF_ZHALFIR: &str = "Flash\nCreature cards you own that aren't on the \
battlefield have flash.\nEach opponent can cast spells only any time they could cast a sorcery.";

/// P0's upkeep with four each of white, blue and green mana floating; `setup` adds the cards and
/// returns their ids.
fn upkeep<T>(setup: impl FnOnce(&mut GameScenario, &CardDatabase) -> T) -> (GameRunner, T) {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let ids = setup(&mut scenario, db);
    scenario.with_library_top(P0, &["Library A", "Library B"]);
    let mana = [ManaType::White, ManaType::Blue, ManaType::Green]
        .into_iter()
        .flat_map(|color| vec![ManaUnit::new(color, ObjectId(9_900), false, vec![]); 4])
        .collect();
    scenario.with_mana_pool(P0, mana);
    (scenario.build(), ids)
}

fn instant(
    scenario: &mut GameScenario,
    name: &str,
    oracle: &str,
    shards: Vec<ManaCostShard>,
    generic: u32,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, true, oracle)
        .with_mana_cost(ManaCost::Cost { shards, generic })
        .id()
}

/// Casts `spell` in the upkeep and asserts whether its cast-time snapshot has the Flash keyword.
fn assert_upkeep_cast_snapshot_flash(runner: &mut GameRunner, spell: ObjectId, has_flash: bool) {
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert!(cast(runner, spell), "the spell is castable in the upkeep");
    assert!(on_stack(runner, spell));
    assert_eq!(
        runner.state().objects[&spell]
            .cast_spell_keywords
            .contains(&Keyword::Flash),
        has_flash,
        "cast-time keywords: {:?}",
        runner.state().objects[&spell].cast_spell_keywords
    );
}

// CR 609.4: "as though it had flash" changes only when the spell may be cast; the spell does not
// gain Flash, so "spell that has flash" triggers (Slitherwisp) do not see it.
#[test]
fn radagast_first_creature_spell_does_not_gain_the_flash_keyword() {
    let mut b = radagast(Phase::Upkeep, P0);
    let before = pool(&b.runner, P0);
    assert_upkeep_cast_snapshot_flash(&mut b.runner, b.bears[0], false);
    assert_eq!(pool(&b.runner, P0), before - 1);
}

#[test]
fn the_blue_spirit_first_creature_spell_does_not_gain_the_flash_keyword() {
    let mut b = blue_spirit(Phase::Upkeep);
    assert_upkeep_cast_snapshot_flash(&mut b.runner, b.bears[0], false);
}

#[test]
fn printed_flash_survives_under_an_as_though_flash_permission() {
    let (mut runner, viper) = upkeep(|scenario, db| {
        scenario.add_creature_from_oracle(P0, "Radagast of Rhosgobel", 2, 5, RADAGAST_OF_RHOSGOBEL);
        scenario.add_real_card(P0, "Ambush Viper", Zone::Hand, db)
    });
    assert_upkeep_cast_snapshot_flash(&mut runner, viper, true);
}

#[test]
fn leyline_of_anticipation_spell_does_not_gain_the_flash_keyword() {
    let (mut runner, bears) = upkeep(|scenario, db| {
        scenario.add_enchantment_from_oracle(
            P0,
            "Leyline of Anticipation",
            LEYLINE_OF_ANTICIPATION,
        );
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db)
    });
    assert_upkeep_cast_snapshot_flash(&mut runner, bears, false);
}

#[test]
fn borne_upon_a_wind_spell_does_not_gain_the_flash_keyword() {
    let (mut runner, (borne, bears)) = upkeep(|scenario, db| {
        let borne = instant(
            scenario,
            "Borne Upon a Wind",
            BORNE_UPON_A_WIND,
            vec![ManaCostShard::Blue],
            1,
        );
        (
            borne,
            scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db),
        )
    });
    assert!(cast(&mut runner, borne));
    let hand_after_cast = runner.state().players[P0.0 as usize].hand.len();
    runner.resolve_top();
    // CR 608.2d + CR 611.2a: resolution installs the permission and draws without a prompt.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_after_cast + 1
    );
    assert_upkeep_cast_snapshot_flash(&mut runner, bears, false);
}

#[test]
fn savage_summoning_next_creature_spell_does_not_gain_the_flash_keyword() {
    let (mut runner, (summoning, bears)) = upkeep(|scenario, db| {
        let summoning = instant(
            scenario,
            "Savage Summoning",
            SAVAGE_SUMMONING,
            vec![ManaCostShard::Green],
            0,
        );
        (
            summoning,
            scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db),
        )
    });
    assert!(cast(&mut runner, summoning));
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert_upkeep_cast_snapshot_flash(&mut runner, bears, false);
}

// CR 601.3: Leyline's unconditional as-though-flash permission alone allows the cast, so Timely
// Ward's own "if it targets a commander" option (CR 601.3b) does not limit its targets.
#[test]
fn leyline_of_anticipation_lets_timely_ward_target_a_noncommander_in_the_upkeep() {
    let (mut runner, (ward, bears)) = upkeep(|scenario, db| {
        scenario.add_enchantment_from_oracle(
            P0,
            "Leyline of Anticipation",
            LEYLINE_OF_ANTICIPATION,
        );
        // A second legal target keeps the engine from choosing for P0.
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
        (
            scenario.add_real_card(P0, "Timely Ward", Zone::Hand, db),
            scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db),
        )
    });
    assert!(
        cast(&mut runner, ward),
        "Leyline lets P0 announce Timely Ward"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(bears)],
        })
        .expect("a non-commander target keeps Leyline's permission");
    assert!(on_stack(&runner, ward));
}

struct AzulaBoard {
    runner: GameRunner,
    azula: ObjectId,
    ward: ObjectId,
    bears: ObjectId,
    altar: Option<ObjectId>,
}

/// Azula exiles P1's Timely Ward, then play reaches P0's next upkeep with `white` {W} floating.
fn azula_upkeep(white: usize, with_altar: bool) -> AzulaBoard {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let azula = scenario.add_real_card(P0, "Azula, Cunning Usurper", Zone::Hand, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let altar =
        with_altar.then(|| scenario.add_real_card(P0, "Phyrexian Altar", Zone::Battlefield, db));
    scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let ward = scenario.add_real_card(P1, "Timely Ward", Zone::Graveyard, db);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Library A", "Library B"]);
    }
    let mut mana = vec![ManaUnit::new(ManaType::Black, ObjectId(9_900), false, vec![]); 4];
    mana.push(ManaUnit::new(
        ManaType::Blue,
        ObjectId(9_901),
        false,
        vec![],
    ));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();

    assert!(cast(&mut runner, azula), "P0 casts Azula");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&ward].zone,
        Zone::Exile,
        "Azula's enters trigger exiles Timely Ward; waiting for {:?}",
        runner.state().waiting_for
    );
    // Through P0's end step and P1's turn to P0's next upkeep.
    for _ in 0..2 {
        runner.advance_to_phase(Phase::End);
        runner.advance_to_phase(Phase::Upkeep);
    }
    assert_eq!(
        (runner.state().active_player, runner.state().phase),
        (P0, Phase::Upkeep)
    );
    for _ in 0..white {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::White,
                ObjectId(9_902),
                false,
                vec![],
            ));
    }
    AzulaBoard {
        runner,
        azula,
        ward,
        bears,
        altar,
    }
}

fn select_ward_target(runner: &mut GameRunner, bears: ObjectId) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(bears)],
        })
        .expect("a non-commander target keeps Azula's permission");
}

// CR 601.3: Azula's unconditional as-though-flash permission alone allows casting a card exiled
// with it, so Timely Ward's own "if it targets a commander" option (CR 601.3b) does not limit
// its targets.
#[test]
fn azula_lets_an_exiled_timely_ward_target_a_noncommander_in_the_upkeep() {
    let AzulaBoard {
        mut runner,
        ward,
        bears,
        ..
    } = azula_upkeep(3, false);
    assert!(
        engine::ai_support::legal_actions(runner.state())
            .iter()
            .any(|action| matches!(
                action,
                GameAction::CastSpell { object_id, .. } if *object_id == ward
            )),
        "Timely Ward exiled with Azula is a legal cast in P0's upkeep"
    );
    assert!(
        cast(&mut runner, ward),
        "Azula lets P0 announce Timely Ward"
    );
    select_ward_target(&mut runner, bears);
    assert!(on_stack(&runner, ward));
}

// CR 601.6 + CR 605.3a: the cast was proposed under Azula's permission, so sacrificing Azula to
// Phyrexian Altar ("Sacrifice a creature: Add one mana of any color.") during mana payment does
// not make Timely Ward's commander condition apply to the noncommander target.
#[test]
fn azula_permission_holds_when_azula_is_sacrificed_for_mana_mid_payment() {
    let AzulaBoard {
        mut runner,
        azula,
        ward,
        bears,
        altar,
    } = azula_upkeep(2, true);
    let altar = altar.expect("Phyrexian Altar is on the battlefield");
    let card_id = runner.state().objects[&ward].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ward,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("Azula lets P0 announce Timely Ward");
    select_ward_target(&mut runner, bears);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ActivateAbility {
            source_id: altar,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Phyrexian Altar is a mana ability usable during payment");
    runner
        .act(GameAction::SelectCards { cards: vec![azula] })
        .expect("P0 sacrifices Azula");
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::White),
            count: 1,
        })
        .expect("Phyrexian Altar adds {W}");
    assert_eq!(runner.state().objects[&azula].zone, Zone::Graveyard);
    runner
        .act(GameAction::PassPriority)
        .expect("Timely Ward finishes casting");
    assert!(on_stack(&runner, ward));
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
}

const TEGWYLLS_SCOURING: &str = "You may cast this spell as though it had flash by tapping three \
untapped creatures you control with flying in addition to paying its other costs.\nDestroy all \
creatures. Create three 1/1 black Faerie Rogue creature tokens with flying.";

/// P0's upkeep with three Storm Crows, six {B} floating and Tegwyll's Scouring in hand; Leyline
/// of Anticipation on the battlefield when `with_leyline`.
fn tegwyll_upkeep(with_leyline: bool) -> (GameRunner, ObjectId, [ObjectId; 3]) {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    if with_leyline {
        scenario.add_enchantment_from_oracle(
            P0,
            "Leyline of Anticipation",
            LEYLINE_OF_ANTICIPATION,
        );
    }
    let crows = [0; 3].map(|_| scenario.add_real_card(P0, "Storm Crow", Zone::Battlefield, db));
    let scouring = scenario
        .add_spell_to_hand_from_oracle(P0, "Tegwyll's Scouring", false, TEGWYLLS_SCOURING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 4,
        })
        .id();
    scenario.with_library_top(P0, &["Library A", "Library B"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(9_900), false, vec![]); 6],
    );
    (scenario.build(), scouring, crows)
}

fn tapped_count(runner: &GameRunner, ids: &[ObjectId]) -> usize {
    ids.iter()
        .filter(|id| runner.state().objects[*id].tapped)
        .count()
}

// CR 601.3b + CR 601.3c: Leyline alone lets Tegwyll's Scouring be cast in the upkeep, so its own
// "by tapping three untapped creatures" option is not the route and its cost is not imposed.
#[test]
fn leyline_lets_tegwylls_scouring_be_cast_without_tapping_fliers() {
    let (mut runner, scouring, crows) = tegwyll_upkeep(true);
    assert!(cast(&mut runner, scouring), "Leyline lets P0 cast it");
    assert!(on_stack(&runner, scouring));
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(tapped_count(&runner, &crows), 0);
    assert_eq!(pool(&runner, P0), 0);
}

// CR 601.3c: with no other permission, the spell's own option is the route and its tap cost is
// paid.
#[test]
fn tegwylls_scouring_own_flash_option_requires_tapping_three_fliers() {
    let (mut runner, scouring, crows) = tegwyll_upkeep(false);
    assert!(
        cast(&mut runner, scouring),
        "its own option allows the cast"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                min_count: 3,
                ..
            }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: crows.to_vec(),
        })
        .expect("P0 taps the three Storm Crows");
    assert!(on_stack(&runner, scouring));
    assert_eq!(tapped_count(&runner, &crows), 3);
}

/// P0's upkeep with Primal Prayers, two energy, Grizzly Bears in hand and Radagast when
/// `with_radagast`.
fn primal_prayers_upkeep(with_radagast: bool) -> (GameRunner, ObjectId) {
    let (mut runner, bears) = upkeep(|scenario, db| {
        if with_radagast {
            scenario.add_creature_from_oracle(
                P0,
                "Radagast of Rhosgobel",
                2,
                5,
                RADAGAST_OF_RHOSGOBEL,
            );
        }
        scenario.add_real_card(P0, "Primal Prayers", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db)
    });
    runner.state_mut().players[P0.0 as usize].energy = 2;
    (runner, bears)
}

fn energy(runner: &GameRunner) -> u32 {
    runner.state().players[P0.0 as usize].energy
}

// CR 118.9b + CR 601.3c: Radagast alone lets the first Grizzly Bears be cast in the upkeep, so
// Primal Prayers' {E} is an optional alternative cost rather than a timing requirement.
#[test]
fn radagast_leaves_primal_prayers_energy_cost_optional() {
    let (mut runner, bears) = primal_prayers_upkeep(true);
    let before = pool(&runner, P0);
    assert!(cast(&mut runner, bears), "Radagast lets P0 cast it");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(energy(&runner), 2, "no energy is taken before the choice");

    let mut paid_energy = GameRunner::from_state(runner.state().clone());
    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("P0 pays the reduced mana cost");
    assert!(on_stack(&runner, bears));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(energy(&runner), 2);
    assert_eq!(pool(&runner, P0), before - 1);

    paid_energy
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("P0 pays {E}");
    assert!(on_stack(&paid_energy, bears));
    assert!(matches!(
        paid_energy.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(energy(&paid_energy), 1);
    assert_eq!(pool(&paid_energy, P0), before);
}

// CR 118.9a + CR 601.3c: Primal Prayers alone allows the upkeep cast only by paying {E}, which is
// paid once and not offered again.
#[test]
fn primal_prayers_alone_requires_its_energy_cost_once() {
    let (mut runner, bears) = primal_prayers_upkeep(false);
    let before = pool(&runner, P0);
    assert!(cast(&mut runner, bears), "Primal Prayers lets P0 cast it");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(on_stack(&runner, bears));
    assert_eq!(energy(&runner), 1);
    assert_eq!(pool(&runner, P0), before);
}

const WISPMARE: &str = "Flying\nWhen this creature enters, destroy target enchantment.\nEvoke \
{W} (You may cast this spell for its evoke cost. If you do, it's sacrificed when it enters.)";

const MARDU_SHADOWSPEAR: &str = "Whenever this creature attacks, each opponent loses 1 life.\n\
Dash {1}{B} (You may cast this spell for its dash cost. If you do, it gains haste, and it's \
returned from the battlefield to its owner's hand at the beginning of the next end step.)";

fn wispmare(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Wispmare", 1, 3, WISPMARE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        })
        .id()
}

fn mardu_shadowspear(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Mardu Shadowspear", 1, 1, MARDU_SHADOWSPEAR)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id()
}

/// P0's upkeep with Primal Prayers, two energy, `spell` in hand, four {B} added to the floating
/// mana, and Leyline of Anticipation when `with_leyline`.
fn prayers_upkeep(
    with_leyline: bool,
    spell: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> (GameRunner, ObjectId) {
    let (mut runner, spell) = upkeep(|scenario, db| {
        if with_leyline {
            scenario.add_enchantment_from_oracle(
                P0,
                "Leyline of Anticipation",
                LEYLINE_OF_ANTICIPATION,
            );
        }
        scenario.add_real_card(P0, "Primal Prayers", Zone::Battlefield, db);
        spell(scenario)
    });
    let player = &mut runner.state_mut().players[P0.0 as usize];
    player.energy = 2;
    for _ in 0..4 {
        player.mana_pool.add(ManaUnit::new(
            ManaType::Black,
            ObjectId(9_903),
            false,
            vec![],
        ));
    }
    (runner, spell)
}

/// Casts `spell` in the upkeep and asserts it went on the stack through Primal Prayers' {E}.
fn assert_cast_only_through_prayers(mut runner: GameRunner, spell: ObjectId) {
    let before = pool(&runner, P0);
    assert!(cast(&mut runner, spell), "Primal Prayers lets P0 cast it");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(on_stack(&runner, spell));
    assert_eq!(energy(&runner), 1);
    assert_eq!(pool(&runner, P0), before);
}

// CR 118.9a + CR 601.6: Primal Prayers grants flash timing only with its own alternative cost, so
// in the upkeep Wispmare's evoke cost is not offered.
#[test]
fn primal_prayers_alone_does_not_offer_evoke_in_the_upkeep() {
    let (runner, spell) = prayers_upkeep(false, wispmare);
    assert_cast_only_through_prayers(runner, spell);
}

#[test]
fn primal_prayers_alone_does_not_offer_dash_in_the_upkeep() {
    let (runner, spell) = prayers_upkeep(false, mardu_shadowspear);
    assert_cast_only_through_prayers(runner, spell);
}

// CR 702.74a: Leyline's permission does not depend on a cost, so Wispmare may be evoked for {W}.
#[test]
fn leyline_lets_wispmare_be_evoked_beside_primal_prayers() {
    let (mut runner, spell) = prayers_upkeep(true, wispmare);
    let before = pool(&runner, P0);
    assert!(cast(&mut runner, spell), "Leyline lets P0 cast it");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Evoke,
                ..
            }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("P0 pays the evoke cost");
    assert!(on_stack(&runner, spell));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(energy(&runner), 2);
    assert_eq!(pool(&runner, P0), before - 1);
}

// CR 702.8a: "have flash" grants the keyword itself, unlike an "as though" permission.
#[test]
fn chea_familiar_spell_gains_the_flash_keyword() {
    let (mut runner, lions) = upkeep(|scenario, db| {
        scenario.add_creature_from_oracle(P0, "Chea, Friend to Maybe Too Many", 2, 4, CHEA);
        scenario.add_real_card(P0, "Savannah Lions", Zone::Hand, db)
    });
    assert_upkeep_cast_snapshot_flash(&mut runner, lions, true);
}

#[test]
fn teferi_mage_of_zhalfir_creature_spell_gains_the_flash_keyword() {
    let (mut runner, bears) = upkeep(|scenario, db| {
        scenario.add_creature_from_oracle(
            P0,
            "Teferi, Mage of Zhalfir",
            3,
            4,
            TEFERI_MAGE_OF_ZHALFIR,
        );
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db)
    });
    assert_upkeep_cast_snapshot_flash(&mut runner, bears, true);
}

fn cast_with(runner: &mut GameRunner, spell: ObjectId, payment_mode: CastPaymentMode) -> bool {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode,
        })
        .is_ok()
}

/// The file's upkeep board at `phase`, with two energy and exactly `mana` floating.
fn energy_board<T>(
    phase: Phase,
    mana: &[ManaType],
    setup: impl FnOnce(&mut GameScenario, &CardDatabase) -> T,
) -> (GameRunner, T) {
    let (mut runner, ids) = upkeep(setup);
    runner.state_mut().phase = phase;
    let player = &mut runner.state_mut().players[P0.0 as usize];
    player.energy = 2;
    player.mana_pool = Default::default();
    for color in mana {
        player
            .mana_pool
            .add(ManaUnit::new(*color, ObjectId(9_905), false, vec![]));
    }
    (runner, ids)
}

fn prayers_and_fleshgorger(phase: Phase) -> (GameRunner, ObjectId) {
    energy_board(phase, &[], |scenario, db| {
        scenario.add_real_card(P0, "Primal Prayers", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Phyrexian Fleshgorger", Zone::Hand, db)
    })
}

fn assert_priority(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{:?}",
        runner.state().waiting_for
    );
}

/// CR 718.3b: the prototyped spell has only its alternative characteristics, and Primal
/// Prayers' {E} paid for it.
fn assert_prototyped_through_prayers(runner: &GameRunner, fleshgorger: ObjectId) {
    assert!(on_stack(runner, fleshgorger));
    let object = &runner.state().objects[&fleshgorger];
    assert_eq!((object.power, object.toughness), (Some(3), Some(3)));
    assert_eq!(
        object.mana_cost,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        }
    );
    assert_eq!(energy(runner), 1);
    assert_eq!(pool(runner, P0), 0);
}

fn assert_upkeep_prototype_cast_through_prayers(payment_mode: CastPaymentMode) {
    let (mut runner, fleshgorger) = prayers_and_fleshgorger(Phase::Upkeep);
    assert!(engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        fleshgorger
    ));
    assert!(cast_with(&mut runner, fleshgorger, payment_mode));
    assert_priority(&runner);
    assert_prototyped_through_prayers(&runner, fleshgorger);
}

// CR 718.3a + CR 118.9a + CR 601.3c: the prototyped Phyrexian Fleshgorger is a creature spell
// with mana value 3, so Primal Prayers' {E} casts it in the upkeep.
#[test]
fn primal_prayers_casts_a_prototyped_spell_in_the_upkeep() {
    assert_upkeep_prototype_cast_through_prayers(CastPaymentMode::Manual);
    assert_upkeep_prototype_cast_through_prayers(CastPaymentMode::Auto);
}

// CR 702.160a + CR 118.9a: at sorcery timing Primal Prayers' {E} is offered for the prototyped
// cast.
#[test]
fn primal_prayers_is_offered_for_a_prototyped_spell() {
    let (mut runner, fleshgorger) = prayers_and_fleshgorger(Phase::PreCombatMain);
    assert!(cast_with(&mut runner, fleshgorger, CastPaymentMode::Manual));
    let prototype_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 1,
    };
    match &runner.state().waiting_for {
        WaitingFor::OptionalCostChoice {
            cost: AdditionalCost::Choice(alt, AbilityCost::Mana { cost }),
            ..
        } => {
            assert_eq!(
                alt,
                &AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 1 },
                }
            );
            assert_eq!(cost, &prototype_cost);
        }
        other => panic!("expected Primal Prayers' offer, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("P0 pays {E}");
    assert_priority(&runner);
    assert_prototyped_through_prayers(&runner, fleshgorger);
}

/// Casts Bonecrusher Giant as Stomp targeting P1 with manual payment.
fn announce_stomp(runner: &mut GameRunner, giant: ObjectId) {
    assert!(cast_with(runner, giant, CastPaymentMode::Manual));
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .expect("P0 casts Stomp");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        })
        .expect("Stomp targets P1");
}

fn finish_paying(runner: &mut GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::PassPriority)
        .expect("P0 finishes paying");
    assert_priority(runner);
}

// CR 715.3a + CR 118.9a: Stomp is cast with its own characteristics, and Fist of Suns'
// alternative cost may pay for it.
#[test]
fn fist_of_suns_is_offered_for_an_adventure() {
    use ManaType::{Black, Blue, Green, Red, White};
    let (mut runner, giant) = energy_board(
        Phase::PreCombatMain,
        &[White, Blue, Black, Red, Green],
        |scenario, db| {
            scenario.add_real_card(P0, "Fist of Suns", Zone::Battlefield, db);
            scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
            scenario.add_real_card(P0, "Bonecrusher Giant", Zone::Hand, db)
        },
    );
    announce_stomp(&mut runner, giant);
    let wubrg = ManaCost::Cost {
        shards: vec![
            ManaCostShard::White,
            ManaCostShard::Blue,
            ManaCostShard::Black,
            ManaCostShard::Red,
            ManaCostShard::Green,
        ],
        generic: 0,
    };
    let stomp = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 1,
    };
    match &runner.state().waiting_for {
        WaitingFor::OptionalCostChoice {
            cost:
                AdditionalCost::Choice(AbilityCost::Mana { cost: alt }, AbilityCost::Mana { cost }),
            ..
        } => assert_eq!((alt, cost), (&wubrg, &stomp)),
        other => panic!("expected Fist of Suns' offer, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("P0 accepts Fist of Suns' cost");
    finish_paying(&mut runner);
    assert!(on_stack(&runner, giant));
    assert_eq!(pool(&runner, P0), 0);
}

// CR 702.37c + CR 118.9a: a face-down cast is itself an alternative cost, so Primal Prayers'
// timing doesn't come with it.
#[test]
fn primal_prayers_does_not_let_a_face_down_spell_be_cast_in_the_upkeep() {
    let stalker_board = |phase| {
        energy_board(phase, &[ManaType::Blue; 4], |scenario, db| {
            scenario.add_real_card(P0, "Primal Prayers", Zone::Battlefield, db);
            scenario.add_real_card(P0, "Glacial Stalker", Zone::Hand, db)
        })
    };
    let (mut runner, stalker) = stalker_board(Phase::PreCombatMain);
    assert!(cast_with(&mut runner, stalker, CastPaymentMode::Manual));
    finish_paying(&mut runner);
    assert!(on_stack(&runner, stalker));
    assert!(runner.state().objects[&stalker].face_down);
    assert_eq!(pool(&runner, P0), 1);

    let (mut runner, stalker) = stalker_board(Phase::Upkeep);
    assert!(!cast_with(&mut runner, stalker, CastPaymentMode::Manual));
    assert_eq!(runner.state().objects[&stalker].zone, Zone::Hand);
    assert_eq!(energy(&runner), 2);
    assert_eq!(pool(&runner, P0), 4);
}

// CR 715.3a: Stomp is an instant, outside Primal Prayers' creature spells.
#[test]
fn primal_prayers_is_not_offered_for_an_instant_adventure() {
    let (mut runner, giant) =
        energy_board(Phase::PreCombatMain, &[ManaType::Red; 2], |scenario, db| {
            scenario.add_real_card(P0, "Primal Prayers", Zone::Battlefield, db);
            scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
            scenario.add_real_card(P0, "Bonecrusher Giant", Zone::Hand, db)
        });
    announce_stomp(&mut runner, giant);
    finish_paying(&mut runner);
    assert!(on_stack(&runner, giant));
    assert_eq!(pool(&runner, P0), 0);
    assert_eq!(energy(&runner), 2);
}

const TLINCALLI_HUNTER: &str = "Trample\nOnce each turn, you may pay {0} rather than pay the mana \
cost for a creature spell you cast from exile.";

/// P0's precombat main with Tlincalli Hunter and `mana` floating; `setup` adds the other cards.
fn hunter_board<T>(
    mana: Vec<ManaUnit>,
    setup: impl FnOnce(&mut GameScenario, &CardDatabase) -> T,
) -> (GameRunner, ObjectId, T) {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hunter = scenario
        .add_creature_from_oracle(P0, "Tlincalli Hunter", 7, 7, TLINCALLI_HUNTER)
        .id();
    let ids = setup(&mut scenario, db);
    scenario.with_mana_pool(P0, mana);
    (scenario.build(), hunter, ids)
}

fn floating(color: ManaType, count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(color, ObjectId(9_900), false, vec![]); count]
}

/// Passes priority until the stack is empty, choosing P1 for each target and keeping a scried
/// card on top.
fn resolve_choosing_p1(runner: &mut GameRunner) {
    for _ in 0..40 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                }
            }
            WaitingFor::ScryChoice { .. } => GameAction::SelectCards { cards: Vec::new() },
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the action is legal");
    }
    panic!("the stack did not empty");
}

fn assert_cast_from_exile_without_hunter(
    runner: &mut GameRunner,
    spell: ObjectId,
    hunter: ObjectId,
) {
    assert!(cast_with(runner, spell, CastPaymentMode::Auto));
    assert_priority(runner);
    assert!(on_stack(runner, spell));
    assert!(!runner
        .state()
        .alt_cost_grant_permissions_used
        .contains(&hunter));
}

// CR 118.9a: Maralen's "without paying its mana cost" is the cast's one alternative cost, so
// Tlincalli Hunter's {0} isn't offered over it.
#[test]
fn maralen_free_cast_is_not_offered_tlincalli_hunters_cost() {
    let (mut runner, hunter, (maralen, elves, pile)) =
        hunter_board(floating(ManaType::Green, 1), |scenario, db| {
            scenario.with_library_top(P0, &["Library A", "Library B"]);
            (
                scenario.add_real_card(P0, "Maralen, Fae Ascendant", Zone::Battlefield, db),
                scenario.add_real_card(P0, "Llanowar Elves", Zone::Hand, db),
                [
                    scenario.add_real_card(P1, "Grizzly Bears", Zone::Library, db),
                    scenario.add_real_card(P1, "Grizzly Bears", Zone::Library, db),
                ],
            )
        });
    assert!(cast(&mut runner, elves));
    resolve_choosing_p1(&mut runner);
    assert_eq!(
        pile.map(|bears| runner.state().objects[&bears].zone),
        [Zone::Exile; 2]
    );
    assert_eq!(pool(&runner, P0), 0);
    assert!(engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        pile[0]
    ));
    assert_cast_from_exile_without_hunter(&mut runner, pile[0], hunter);
    assert!(runner
        .state()
        .exile_cast_permissions_used
        .contains(&maralen));
}

// CR 118.9a: Uba Mask leaves the mana cost to pay, so Tlincalli Hunter's {0} may replace it.
#[test]
fn uba_mask_cast_is_offered_tlincalli_hunters_cost() {
    let (mut runner, hunter, (opt, bears)) =
        hunter_board(floating(ManaType::Blue, 1), |scenario, db| {
            scenario.add_real_card(P0, "Uba Mask", Zone::Battlefield, db);
            (
                instant(scenario, "Opt", OPT, vec![ManaCostShard::Blue], 0),
                scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db),
            )
        });
    assert!(cast(&mut runner, opt));
    resolve_choosing_p1(&mut runner);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(pool(&runner, P0), 0);
    assert!(cast_with(&mut runner, bears, CastPaymentMode::Auto));
    let printed = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 1,
    };
    match &runner.state().waiting_for {
        WaitingFor::OptionalCostChoice {
            cost:
                AdditionalCost::Choice(AbilityCost::Mana { cost: alt }, AbilityCost::Mana { cost }),
            ..
        } => assert_eq!((alt, cost), (&ManaCost::generic(0), &printed)),
        other => panic!("expected Tlincalli Hunter's offer, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("P0 pays {0}");
    assert_priority(&runner);
    assert!(on_stack(&runner, bears));
    assert!(runner
        .state()
        .alt_cost_grant_permissions_used
        .contains(&hunter));
}

/// Valgavoth with P0 at `life` after Thought Scour mills two of P1's Grizzly Bears into exile.
fn valgavoth_pile(life: i32) -> (GameRunner, ObjectId, [ObjectId; 2]) {
    let (mut runner, hunter, (scour, pile)) =
        hunter_board(floating(ManaType::Blue, 1), |scenario, db| {
            scenario.with_library_top(P0, &["Library A", "Library B"]);
            scenario.with_life(P0, life);
            scenario.add_real_card(P0, "Valgavoth, Terror Eater", Zone::Battlefield, db);
            (
                scenario.add_real_card(P0, "Thought Scour", Zone::Hand, db),
                [
                    scenario.add_real_card(P1, "Grizzly Bears", Zone::Library, db),
                    scenario.add_real_card(P1, "Grizzly Bears", Zone::Library, db),
                ],
            )
        });
    assert!(cast(&mut runner, scour));
    resolve_choosing_p1(&mut runner);
    assert_eq!(
        pile.map(|bears| runner.state().objects[&bears].zone),
        [Zone::Exile; 2]
    );
    assert_eq!(pool(&runner, P0), 0);
    assert_eq!(runner.state().players[P0.0 as usize].life, life);
    (runner, hunter, pile)
}

// CR 118.9a: Valgavoth's "pay life equal to its mana value rather than pay its mana cost" is the
// cast's one alternative cost, so Tlincalli Hunter's {0} isn't offered over it.
#[test]
fn valgavoth_life_cast_is_not_offered_tlincalli_hunters_cost() {
    let (mut runner, hunter, pile) = valgavoth_pile(20);
    assert_cast_from_exile_without_hunter(&mut runner, pile[0], hunter);
    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
}

// CR 118.9a + CR 119.4: at 1 life Valgavoth's cost can't be paid, and Tlincalli Hunter's {0}
// can't replace it.
#[test]
fn valgavoth_life_cast_is_refused_when_the_life_cannot_be_paid() {
    let (mut runner, _, pile) = valgavoth_pile(1);
    assert!(!engine::game::casting::can_cast_object_now(
        runner.state(),
        P0,
        pile[0]
    ));
    assert!(!cast_with(&mut runner, pile[0], CastPaymentMode::Auto));
    assert_eq!(runner.state().objects[&pile[0]].zone, Zone::Exile);
    assert_eq!(runner.state().players[P0.0 as usize].life, 1);
}

// CR 118.9: Fire Lord Ozai's "you may play one of those cards without paying its mana cost" is
// the cast's one alternative cost, so Tlincalli Hunter's {0} isn't offered over it.
#[test]
fn fire_lord_ozai_free_cast_is_not_offered_tlincalli_hunters_cost() {
    let (mut runner, hunter, (ozai, bears)) =
        hunter_board(floating(ManaType::Red, 6), |scenario, db| {
            scenario.with_library_top(P0, &["Library A", "Library B"]);
            (
                scenario.add_real_card(P0, "Fire Lord Ozai", Zone::Battlefield, db),
                scenario.add_real_card(P1, "Grizzly Bears", Zone::Library, db),
            )
        });
    runner
        .act(GameAction::ActivateAbility {
            source_id: ozai,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 activates Fire Lord Ozai");
    resolve_choosing_p1(&mut runner);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(pool(&runner, P0), 0);
    assert_cast_from_exile_without_hunter(&mut runner, bears, hunter);
}

// CR 118.9a: Inside Information's "pay life equal to its mana value rather than pay its mana
// cost" is the cast's one alternative cost, so Tlincalli Hunter's {0} isn't offered over it.
#[test]
fn inside_information_life_cast_is_not_offered_tlincalli_hunters_cost() {
    let (mut runner, hunter, (inside, bears)) =
        hunter_board(floating(ManaType::Black, 3), |scenario, db| {
            (
                scenario.add_real_card(P0, "Inside Information", Zone::Hand, db),
                scenario.add_real_card(P1, "Grizzly Bears", Zone::Library, db),
            )
        });
    assert!(cast(&mut runner, inside));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { .. }
    ));
    runner
        .act(GameAction::ChooseX { value: 1 })
        .expect("P0 chooses X = 1");
    resolve_choosing_p1(&mut runner);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(pool(&runner, P0), 0);
    assert_eq!(runner.state().players[P0.0 as usize].life, 20);
    assert_cast_from_exile_without_hunter(&mut runner, bears, hunter);
    assert_eq!(runner.state().players[P0.0 as usize].life, 18);
}

/// P0's precombat main with `mana` floating and P1's Grizzly Bears on the battlefield above a
/// three-card library; `setup` adds P0's cards.
fn mastery_board<T>(
    mana: Vec<ManaUnit>,
    setup: impl FnOnce(&mut GameScenario, &CardDatabase) -> T,
) -> (GameRunner, ObjectId, T) {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ids = setup(&mut scenario, db);
    let target = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    for _ in 0..3 {
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Library, db);
    }
    scenario.with_mana_pool(P0, mana);
    (scenario.build(), target, ids)
}

/// Passes priority until the stack is empty, targeting `target`, choosing P1 as the opponent and
/// answering an alternative cost offer with `pay`.
fn resolve_targeting(runner: &mut GameRunner, target: ObjectId, pay: bool) {
    for _ in 0..40 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            WaitingFor::OptionalCostChoice { .. } => GameAction::DecideOptionalCost { pay },
            WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            },
            WaitingFor::NamedChoice { .. } => GameAction::ChooseOption {
                choice: P1.0.to_string(),
            },
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the action is legal");
    }
    panic!("the stack did not empty");
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

// CR 118.9a: a free cast through Fire Lord Ozai's permission is the cast's one alternative cost,
// not Baleful Mastery's {1}{B}, so the opponent doesn't draw.
#[test]
fn fire_lord_ozai_free_cast_of_baleful_mastery_is_not_its_alternative_cost() {
    let (mut runner, target, (ozai, mastery)) =
        mastery_board(floating(ManaType::Red, 6), |scenario, db| {
            (
                scenario.add_real_card(P0, "Fire Lord Ozai", Zone::Battlefield, db),
                scenario.add_real_card(P1, "Baleful Mastery", Zone::Library, db),
            )
        });
    runner
        .act(GameAction::ActivateAbility {
            source_id: ozai,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 activates Fire Lord Ozai");
    resolve_targeting(&mut runner, target, false);
    assert_eq!(runner.state().objects[&mastery].zone, Zone::Exile);
    let p1_hand = hand_size(&runner, P1);
    assert!(cast_with(&mut runner, mastery, CastPaymentMode::Auto));
    resolve_targeting(&mut runner, target, false);
    assert_eq!(runner.state().objects[&mastery].zone, Zone::Graveyard);
    assert_eq!(hand_size(&runner, P1), p1_hand);
}

// CR 118.9: paying Baleful Mastery's own {1}{B} makes the opponent draw.
#[test]
fn baleful_mastery_alternative_cost_makes_an_opponent_draw() {
    let (mut runner, target, mastery) =
        mastery_board(floating(ManaType::Black, 2), |scenario, db| {
            scenario.add_real_card(P0, "Baleful Mastery", Zone::Hand, db)
        });
    let p1_hand = hand_size(&runner, P1);
    assert!(cast_with(&mut runner, mastery, CastPaymentMode::Auto));
    resolve_targeting(&mut runner, target, true);
    assert_eq!(runner.state().objects[&mastery].zone, Zone::Graveyard);
    assert_eq!(pool(&runner, P0), 0);
    assert_eq!(hand_size(&runner, P1), p1_hand + 1);
}
