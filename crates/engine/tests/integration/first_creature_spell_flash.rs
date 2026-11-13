//! CR 601.3b + CR 702.8a + CR 302.1: Radagast of Rhosgobel ("The first creature spell you cast
//! each turn costs {2} less to cast and can be cast as though it had flash.") and The Blue Spirit
//! ("You may cast the first creature spell you cast each turn as though it had flash.") let only
//! the first creature spell each turn be cast at instant speed. Both sources are built from
//! their Oracle text so the permission comes from the parser at test time.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
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
    runner.resolve_top();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P0 accepts the permission");
    assert!(runner.state().stack.is_empty());
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

// CR 601.3: Azula's unconditional as-though-flash permission alone allows casting a card exiled
// with it, so Timely Ward's own "if it targets a commander" option (CR 601.3b) does not limit
// its targets.
#[test]
fn azula_lets_an_exiled_timely_ward_target_a_noncommander_in_the_upkeep() {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let azula = scenario.add_real_card(P0, "Azula, Cunning Usurper", Zone::Hand, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
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
    for _ in 0..3 {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::White,
                ObjectId(9_902),
                false,
                vec![],
            ));
    }

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
    assert!(on_stack(&runner, ward));
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
