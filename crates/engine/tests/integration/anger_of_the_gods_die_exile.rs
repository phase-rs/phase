//! "[noun] dealt damage this way would die this turn, exile it instead" after a
//! MASS damage instruction (phase-rs/phase#10962): Anger of the Gods, Crush the
//! Weak, Yamabushi's Storm, Underworld Fires, Gallifrey Falls and Draconic
//! Intervention.
//!
//! CR 608.2c: "a creature dealt damage this way" names the set the preceding
//! damage instruction published, so the rider installs its one-shot, end-of-turn
//! replacement (CR 614.1a, CR 514.2) on every object that was actually dealt
//! damage, whether or not that damage was lethal (the Anger of the Gods ruling:
//! "After being dealt damage, if they would die for any reason that turn, they'll
//! be exiled instead.").
//!
//! CR 109.2 + CR 614.1 (phase-rs/phase#11021 review): "a creature dealt damage
//! this way" names creatures only, so the rider's replacement carries the creature
//! noun and a planeswalker that took redirected damage (Gideon's Sacrifice) is put
//! into the graveyard at 0 loyalty rather than exiled. Underworld Fires' "a
//! permanent dealt damage this way" has no such restriction.
//!
//! The single-target sibling (Pillar of Flame, Demonfire, ...) installs its rider
//! even when no damage was dealt: that is the separate phase-rs/phase#10965 and is
//! not covered here. Only its creature-noun gate is (Touch of the Void on a
//! planeswalker).

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, CastingVariantFace, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

// Verbatim Oracle text (MTGJSON 5.3.0+20261116).
const ANGER: &str = "Anger of the Gods deals 3 damage to each creature. If a creature dealt damage this way would die this turn, exile it instead.";
const CRUSH_THE_WEAK: &str = "Crush the Weak deals 2 damage to each creature. If a creature dealt damage this way would die this turn, exile it instead.\nForetell {R} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";
const YAMABUSHIS_STORM: &str = "Yamabushi's Storm deals 1 damage to each creature. If a creature dealt damage this way would die this turn, exile it instead.";
const UNDERWORLD_FIRES: &str = "Underworld Fires deals 1 damage to each creature and each planeswalker. If a permanent dealt damage this way would die this turn, exile it instead.";
const TOUCH_OF_THE_VOID: &str = "Devoid (This card has no color.)\nTouch of the Void deals 3 damage to any target. If a creature dealt damage this way would die this turn, exile it instead.";
const LILIANA_OF_THE_VEIL: &str = "[+1]: Each player discards a card.\n[−2]: Target player sacrifices a creature.\n[−6]: Separate all permanents target player controls into two piles. That player sacrifices all permanents in the pile of their choice.";

fn pool(mana: &[ManaType]) -> Vec<ManaUnit> {
    mana.iter()
        .map(|mana| ManaUnit::new(*mana, ObjectId(0), false, vec![]))
        .collect()
}

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    for unit in pool(mana) {
        runner
            .state_mut()
            .players
            .iter_mut()
            .find(|player| player.id == P0)
            .unwrap()
            .mana_pool
            .add(unit);
    }
}

fn cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

/// One printed mass-damage sorcery: its name, printed mana cost, verbatim text,
/// keyword hints, the damage it deals and the mana that pays for it.
struct MassDamageSpell {
    name: &'static str,
    mana_cost: ManaCost,
    oracle: &'static str,
    keywords: &'static [&'static str],
    damage: u32,
    mana: Vec<ManaType>,
}

fn anger() -> MassDamageSpell {
    MassDamageSpell {
        name: "Anger of the Gods",
        mana_cost: cost(1, &[ManaCostShard::Red, ManaCostShard::Red]),
        oracle: ANGER,
        keywords: &[],
        damage: 3,
        mana: vec![ManaType::Red, ManaType::Red, ManaType::Colorless],
    }
}

fn crush_the_weak() -> MassDamageSpell {
    MassDamageSpell {
        name: "Crush the Weak",
        mana_cost: cost(2, &[ManaCostShard::Red]),
        oracle: CRUSH_THE_WEAK,
        keywords: &["Foretell"],
        damage: 2,
        mana: vec![ManaType::Red, ManaType::Colorless, ManaType::Colorless],
    }
}

fn yamabushis_storm() -> MassDamageSpell {
    MassDamageSpell {
        name: "Yamabushi's Storm",
        mana_cost: cost(1, &[ManaCostShard::Red]),
        oracle: YAMABUSHIS_STORM,
        keywords: &[],
        damage: 1,
        mana: vec![ManaType::Red, ManaType::Colorless],
    }
}

/// CR 608.2c + CR 614.1a: a board with one lethally damaged creature, one
/// survivor and a Murder in P0's hand, after the spell resolved.
struct ResolvedMassDamage {
    runner: GameRunner,
    lethal: ObjectId,
    survivor: ObjectId,
    murder: ObjectId,
}

/// Cast `spell` (P0, from hand, through the production cast pipeline) with a
/// toughness-`damage` creature controlled by P1 and a toughness-`damage + 1`
/// survivor controlled by P0.
fn cast_mass_damage(spell: &MassDamageSpell) -> ResolvedMassDamage {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lethal = scenario
        .add_creature(P1, "Lethal Victim", 1, spell.damage as i32)
        .id();
    let survivor = scenario
        .add_creature(P0, "Sturdy Survivor", 1, spell.damage as i32 + 1)
        .id();
    let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    scenario.with_mana_pool(P0, pool(&spell.mana));
    let card = scenario
        .add_spell_to_hand(P0, spell.name, false)
        .with_mana_cost(spell.mana_cost.clone())
        .from_oracle_text_with_keywords(spell.keywords, spell.oracle)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(card).resolve();
    let events = outcome.events().to_vec();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "{} must finish resolving: {:?}",
        spell.name,
        outcome.final_waiting_for()
    );
    assert!(runner.state().stack.is_empty());
    // Reach-guard: the damage happened to both creatures before the rider is
    // consulted, so the survivor assertions below are not vacuous.
    for victim in [lethal, survivor] {
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::DamageDealt {
                    source_id,
                    target: TargetRef::Object(target),
                    amount,
                    ..
                } if *source_id == card && *target == victim && *amount == spell.damage
            )),
            "{} must deal {} damage to {victim:?}",
            spell.name,
            spell.damage
        );
    }
    ResolvedMassDamage {
        runner,
        lethal,
        survivor,
        murder,
    }
}

/// CR 608.2c + CR 120.3e + CR 704.5g + CR 614.1a: Anger of the Gods exiles every
/// creature it dealt lethal damage to, whoever controls it.
#[test]
fn anger_exiles_each_lethally_damaged_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opposing = scenario.add_creature(P1, "Opposing Three", 1, 3).id();
    let own = scenario.add_creature(P0, "Own Two", 1, 2).id();
    let bystander = scenario.add_creature(P1, "Sturdy Four", 1, 4).id();
    scenario.with_mana_pool(P0, pool(&anger().mana));
    let card = scenario
        .add_spell_to_hand(P0, "Anger of the Gods", false)
        .with_mana_cost(anger().mana_cost)
        .from_oracle_text(ANGER)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(card).resolve();

    for victim in [opposing, own, bystander] {
        assert!(
            outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::DamageDealt {
                    target: TargetRef::Object(target),
                    amount: 3,
                    ..
                } if *target == victim
            )),
            "reach-guard: Anger must deal 3 damage to {victim:?}"
        );
    }
    outcome.assert_zone(&[opposing, own], Zone::Exile);
    assert_eq!(outcome.state().objects[&bystander].zone, Zone::Battlefield);
    assert_eq!(outcome.state().objects[&bystander].damage_marked, 3);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
}

/// CR 608.2c: an empty population completes cleanly. A COMPLETION row (it also
/// passes at base): the empty damaged set must not leave the resolution hanging
/// or shadow a later creature, which is a new object (CR 400.7).
#[test]
fn anger_on_an_empty_battlefield_completes_cleanly() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let late = scenario
        .add_creature_to_hand(P1, "Late Creature", 2, 2)
        .id();
    let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    scenario.with_mana_pool(P0, pool(&anger().mana));
    let card = scenario
        .add_spell_to_hand(P0, "Anger of the Gods", false)
        .with_mana_cost(anger().mana_cost)
        .from_oracle_text(ANGER)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(card).resolve();
    assert!(!outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::DamageDealt { .. })));
    outcome.assert_zone(&[card], Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), late, Zone::Battlefield, &mut events);
    add_mana(
        &mut runner,
        &[ManaType::Black, ManaType::Black, ManaType::Colorless],
    );
    runner.cast(murder).target_object(late).resolve();
    assert_eq!(runner.state().objects[&late].zone, Zone::Graveyard);
}

/// CR 608.2c + CR 614.1a: the same rider on the printed Anger of the Gods, Crush
/// the Weak and Yamabushi's Storm.
#[test]
fn die_exile_rider_exiles_for_each_printed_mass_damage_spell() {
    for spell in [anger(), crush_the_weak(), yamabushis_storm()] {
        let board = cast_mass_damage(&spell);
        assert_eq!(
            board.runner.state().objects[&board.lethal].zone,
            Zone::Exile,
            "{}: the lethally damaged creature is exiled",
            spell.name
        );
        let survivor = &board.runner.state().objects[&board.survivor];
        assert_eq!(survivor.zone, Zone::Battlefield, "{}", spell.name);
        assert_eq!(survivor.damage_marked, spell.damage, "{}", spell.name);
    }
}

/// CR 614.1a + CR 700.4: ruling for Anger of the Gods and Crush the Weak:
/// creatures need not be dealt lethal damage to be exiled; after being dealt
/// damage, if they would die for any reason that turn, they are exiled instead.
#[test]
fn a_damaged_survivor_that_dies_later_this_turn_is_exiled() {
    for spell in [anger(), crush_the_weak()] {
        let mut board = cast_mass_damage(&spell);
        add_mana(
            &mut board.runner,
            &[ManaType::Black, ManaType::Black, ManaType::Colorless],
        );
        board
            .runner
            .cast(board.murder)
            .target_object(board.survivor)
            .resolve();
        assert_eq!(
            board.runner.state().objects[&board.survivor].zone,
            Zone::Exile,
            "{}: a damaged survivor destroyed later this turn is exiled instead",
            spell.name
        );
        assert_eq!(
            board.runner.state().objects[&board.lethal].zone,
            Zone::Exile
        );
    }
}

/// CR 615.6 + CR 702.16e: a creature that was NOT dealt damage (protection from
/// red prevents all of it) is not in the damaged set, so it dies normally later.
/// Reach-guard: the unprotected creature of the same cast is exiled, proving the
/// rider and the damaged-set publication both fired.
#[test]
fn a_creature_not_dealt_damage_is_not_exiled() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let protected = scenario
        .add_creature(P1, "Protected Creature", 1, 2)
        .with_keyword(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)))
        .id();
    let unprotected = scenario.add_creature(P1, "Unprotected Creature", 1, 2).id();
    let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    scenario.with_mana_pool(P0, pool(&anger().mana));
    let card = scenario
        .add_spell_to_hand(P0, "Anger of the Gods", false)
        .with_mana_cost(anger().mana_cost)
        .from_oracle_text(ANGER)
        .id();
    let mut runner = scenario.build();

    runner.cast(card).resolve();
    assert_eq!(
        runner.state().objects[&unprotected].zone,
        Zone::Exile,
        "reach-guard: the unprotected creature was dealt damage and exiled"
    );
    assert_eq!(runner.state().objects[&protected].damage_marked, 0);
    assert_eq!(runner.state().objects[&protected].zone, Zone::Battlefield);

    add_mana(
        &mut runner,
        &[ManaType::Black, ManaType::Black, ManaType::Colorless],
    );
    runner.cast(murder).target_object(protected).resolve();
    assert_eq!(
        runner.state().objects[&protected].zone,
        Zone::Graveyard,
        "a creature never dealt damage is not named by 'dealt damage this way'"
    );
}

/// CR 400.7: a creature that enters after the spell resolved is a new object the
/// anaphor never named. Reach-guard: the creature damaged by the same Anger is
/// exiled.
#[test]
fn a_creature_entering_after_the_spell_is_not_exiled_by_it() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let damaged = scenario.add_creature(P1, "Damaged Creature", 1, 3).id();
    let late = scenario
        .add_creature_to_hand(P1, "Late Creature", 2, 2)
        .id();
    let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    scenario.with_mana_pool(P0, pool(&anger().mana));
    let card = scenario
        .add_spell_to_hand(P0, "Anger of the Gods", false)
        .with_mana_cost(anger().mana_cost)
        .from_oracle_text(ANGER)
        .id();
    let mut runner = scenario.build();

    runner.cast(card).resolve();
    assert_eq!(
        runner.state().objects[&damaged].zone,
        Zone::Exile,
        "reach-guard: the damaged creature was exiled"
    );
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), late, Zone::Battlefield, &mut events);
    add_mana(
        &mut runner,
        &[ManaType::Black, ManaType::Black, ManaType::Colorless],
    );
    runner.cast(murder).target_object(late).resolve();
    assert_eq!(runner.state().objects[&late].zone, Zone::Graveyard);
}

/// CR 120.3c + CR 704.5i + CR 614.1a: Underworld Fires' set covers a creature
/// AND a planeswalker; a loyalty-1 planeswalker loses its last counter and is
/// exiled instead of going to the graveyard (ruling: "Creatures and
/// planeswalkers don't necessarily have to be dealt lethal damage ... to be
/// exiled").
#[test]
fn underworld_fires_exiles_creature_and_planeswalker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "One Toughness", 1, 1).id();
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Liliana of the Veil", "Liliana", 1, LILIANA_OF_THE_VEIL)
        .id();
    let artifact = scenario
        .add_creature(P1, "Undamaged Artifact", 0, 0)
        .as_artifact()
        .id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Red, ManaType::Colorless]));
    let card = scenario
        .add_spell_to_hand(P0, "Underworld Fires", false)
        .with_mana_cost(cost(1, &[ManaCostShard::Red]))
        .from_oracle_text(UNDERWORLD_FIRES)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(card).resolve();

    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                target: TargetRef::Object(target),
                amount: 1,
                ..
            } if *target == walker
        )),
        "reach-guard: the planeswalker was dealt 1 damage"
    );
    outcome.assert_zone(&[creature, walker], Zone::Exile);
    assert_eq!(outcome.state().objects[&artifact].zone, Zone::Battlefield);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 514.2: the replacement ends at cleanup. A survivor destroyed the same turn
/// is exiled; a survivor of the same cast destroyed on the NEXT turn goes to the
/// graveyard.
#[test]
fn the_die_exile_replacement_expires_at_cleanup() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 cleanup fixture"; 12]);
    scenario.with_library_top(P1, &["P1 cleanup fixture"; 12]);
    let same_turn = scenario.add_creature(P1, "Survivor Same Turn", 1, 4).id();
    let next_turn = scenario.add_creature(P1, "Survivor Next Turn", 1, 4).id();
    let murder_same_turn = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    let murder_next_turn = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    scenario.with_mana_pool(P0, pool(&anger().mana));
    let card = scenario
        .add_spell_to_hand(P0, "Anger of the Gods", false)
        .with_mana_cost(anger().mana_cost)
        .from_oracle_text(ANGER)
        .id();
    let mut runner = scenario.build();

    runner.cast(card).resolve();
    for survivor in [same_turn, next_turn] {
        assert_eq!(
            runner.state().objects[&survivor].damage_marked,
            3,
            "reach-guard: Anger damaged {survivor:?} without killing it"
        );
        assert_eq!(runner.state().objects[&survivor].zone, Zone::Battlefield);
    }

    add_mana(
        &mut runner,
        &[ManaType::Black, ManaType::Black, ManaType::Colorless],
    );
    runner
        .cast(murder_same_turn)
        .target_object(same_turn)
        .resolve();
    assert_eq!(runner.state().objects[&same_turn].zone, Zone::Exile);

    let ending_turn = runner.state().turn_number;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[])
        .expect("P0 declares no attackers before cleanup");
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    runner
        .act(GameAction::PassPriority)
        .expect("the active player passes the End step priority window");
    runner
        .act(GameAction::PassPriority)
        .expect("both players pass and the real cleanup step executes");
    assert!(
        runner.state().turn_number > ending_turn,
        "the End-step passes must cross the real cleanup boundary"
    );
    assert_eq!(runner.state().objects[&next_turn].damage_marked, 0);

    runner.advance_to_phase(Phase::PreCombatMain);
    if !matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ) {
        runner
            .act(GameAction::PassPriority)
            .expect("the active player passes so P0 receives priority");
    }
    // CR 106.4: mana pools empty at the end of every step and phase.
    add_mana(
        &mut runner,
        &[ManaType::Black, ManaType::Black, ManaType::Colorless],
    );
    runner
        .cast(murder_next_turn)
        .target_object(next_turn)
        .resolve();
    assert_eq!(
        runner.state().objects[&next_turn].zone,
        Zone::Graveyard,
        "the replacement lapsed at cleanup (CR 514.2)"
    );
}

/// CR 118.8 + CR 202.3 + CR 601.2h + CR 608.2c + CR 614.1a: Draconic Intervention
/// exiles an instant or sorcery card from the caster's graveyard as an additional
/// cost, deals X (that card's mana value) damage to each non-Dragon creature, and
/// exiles the creatures that die. The spell then exiles itself.
#[test]
fn draconic_intervention_exiles_lethal_non_dragons_and_itself() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let draconic = scenario.add_real_card(P0, "Draconic Intervention", Zone::Hand, db);
    // Murder is an instant with mana value 3 ({1}{B}{B}); its text is not used.
    let murder = scenario.add_real_card(P0, "Murder", Zone::Graveyard, db);
    let lethal = scenario.add_creature(P1, "Three Toughness", 1, 3).id();
    let sturdy = scenario.add_creature(P0, "Five Toughness", 1, 5).id();
    let dragon = scenario.add_real_card(P1, "Shivan Dragon", Zone::Battlefield, db);
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Red,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(draconic).pay_cost_with(&[murder]).resolve();

    // Reach-guard for X (independent of the die-exile rider): the toughness-5
    // creature was dealt exactly the exiled card's mana value.
    assert_eq!(
        outcome.state().objects[&sturdy].damage_marked,
        3,
        "X must equal the exiled card's mana value (3)"
    );
    assert_eq!(outcome.state().objects[&sturdy].zone, Zone::Battlefield);
    // The Dragon is not a legal recipient of "each non-Dragon creature".
    assert_eq!(outcome.state().objects[&dragon].damage_marked, 0);
    assert_eq!(outcome.state().objects[&dragon].zone, Zone::Battlefield);
    outcome.assert_zone(&[lethal], Zone::Exile);
    outcome.assert_zone(&[murder], Zone::Exile);
    outcome.assert_zone(&[draconic], Zone::Exile);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
}

/// Real Gallifrey Falls // No More with a P1 creature of toughness 2, a P0
/// creature of toughness 2, a P0 creature of toughness 2 that No More targets
/// when fused, and a P1 creature of toughness 5.
struct GallifreyBoard {
    runner: GameRunner,
    card: ObjectId,
    lethal_opposing: ObjectId,
    lethal_own: ObjectId,
    sturdy: ObjectId,
    targeted: ObjectId,
}

fn gallifrey_board(mana: &[ManaType]) -> GallifreyBoard {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario.add_real_card(P0, "Gallifrey Falls", Zone::Hand, db);
    let lethal_opposing = scenario.add_creature(P1, "Opposing Two", 1, 2).id();
    let lethal_own = scenario.add_creature(P0, "Own Two", 1, 2).id();
    let targeted = scenario.add_creature(P0, "Phase Out Two", 1, 2).id();
    let sturdy = scenario.add_creature(P1, "Sturdy Five", 1, 5).id();
    scenario.with_mana_pool(P0, pool(mana));
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_eq!(runner.state().objects[&card].name, "Gallifrey Falls");
    assert_eq!(
        runner.state().objects[&card]
            .back_face
            .as_ref()
            .map(|face| face.name.as_str()),
        Some("No More"),
        "the real database fixture must retain No More as the split half"
    );
    GallifreyBoard {
        runner,
        card,
        lethal_opposing,
        lethal_own,
        sturdy,
        targeted,
    }
}

/// CR 709.3a + CR 709.3b + CR 614.1a: cast as its LEFT half only, Gallifrey Falls
/// deals 4 damage to each creature and exiles the ones that die. Only the left
/// half's {4}{R}{R} is paid and no creature phases out (ruling: on the stack an
/// unfused split spell has only that half's characteristics).
#[test]
fn gallifrey_falls_left_half_exiles_lethal_creatures() {
    let mut board = gallifrey_board(&[
        ManaType::Red,
        ManaType::Red,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
    ]);

    let commit = board
        .runner
        .cast(board.card)
        .casting_variant_face(CastingVariant::Normal, CastingVariantFace::Left)
        .commit();
    let StackEntryKind::Spell {
        casting_variant,
        actual_mana_spent,
        ..
    } = &commit
        .state()
        .stack
        .last()
        .expect("the left half is on the stack")
        .kind
    else {
        panic!("expected the split card to be a spell stack entry");
    };
    assert_eq!(*casting_variant, CastingVariant::Normal);
    assert_eq!(
        *actual_mana_spent, 6,
        "only the left half's {{4}}{{R}}{{R}} is paid"
    );
    let outcome = commit.resolve();

    assert_eq!(
        outcome.state().objects[&board.sturdy].damage_marked,
        4,
        "reach-guard: the left half dealt 4 damage to the toughness-5 creature"
    );
    assert_eq!(
        outcome.state().objects[&board.sturdy].zone,
        Zone::Battlefield
    );
    outcome.assert_zone(
        &[board.lethal_opposing, board.lethal_own, board.targeted],
        Zone::Exile,
    );
    assert!(!outcome.state().objects[&board.sturdy].is_phased_out());
    assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 702.102a + CR 702.102c + CR 702.102d + CR 709.4d + CR 702.26b + CR 704.3 + CR 608.2c: cast FUSED, the left half
/// resolves first: every creature is dealt 4 damage and the rider installs; then
/// No More phases out the targeted creature, which is neither destroyed nor
/// exiled ("the targeted creatures will be dealt 4 damage first and then be
/// phased out, even if they had less than 4 toughness"). Untargeted lethal
/// creatures are exiled.
#[test]
fn fused_gallifrey_falls_deals_damage_then_phases_out_targeted_creature() {
    let mut board = gallifrey_board(&[
        ManaType::Red,
        ManaType::Red,
        ManaType::White,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
    ]);

    let commit = board
        .runner
        .cast(board.card)
        .casting_variant(CastingVariant::Fuse)
        .target_object(board.targeted)
        .commit();
    let selected = commit
        .selected_casting_variant()
        .expect("fuse is selected through CastingVariantChoice");
    assert_eq!(selected.variant, CastingVariant::Fuse);
    assert_eq!(
        selected.mana_cost.mana_value(),
        9,
        "CR 702.102c: {{4}}{{R}}{{R}} + {{2}}{{W}}"
    );
    let outcome = commit.resolve();

    let targeted = &outcome.state().objects[&board.targeted];
    assert_eq!(targeted.zone, Zone::Battlefield);
    assert!(targeted.is_phased_out(), "No More phased it out");
    assert_eq!(
        targeted.damage_marked, 4,
        "dealt 4 damage first, even with toughness 2"
    );
    // The untargeted creatures of toughness 2 are exiled by the rider that the
    // left half's chain installed, although No More's targets sit in the same
    // merged chain.
    outcome.assert_zone(&[board.lethal_opposing, board.lethal_own], Zone::Exile);
    assert_eq!(outcome.state().objects[&board.sturdy].damage_marked, 4);
    assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
}

/// P1's half of the redirected-damage rows: real Gideon's Sacrifice in hand with
/// its {W}, and the permanents it shields.
struct RedirectBoard {
    runner: GameRunner,
    anger: ObjectId,
    sacrifice: ObjectId,
}

/// Cast Anger of the Gods as P0 and answer it as P1 with the real Gideon's
/// Sacrifice choosing `chosen`, through the production cast pipeline, then let the
/// whole stack resolve (the review's sequence for phase-rs/phase#11021).
fn anger_answered_by_gideons_sacrifice(board: &mut RedirectBoard, chosen: ObjectId) -> CastOutcome {
    let mut commit = board.runner.cast(board.anger).commit();
    commit
        .act(GameAction::PassPriority)
        .expect("P0 passes priority with Anger on the stack so P1 may respond");
    assert!(
        matches!(commit.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 must hold priority to respond: {:?}",
        commit.state().waiting_for
    );
    let outcome = commit.cast(board.sacrifice).target_object(chosen).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the whole stack must finish resolving: {:?}",
        outcome.final_waiting_for()
    );
    assert!(outcome.state().stack.is_empty());
    outcome
}

/// F1 (phase-rs/phase#11021 review). CR 109.2 + CR 614.1 + CR 704.5i + CR 120.3c:
/// P0 casts Anger of the Gods and P1 responds with Gideon's Sacrifice choosing
/// Liliana of the Veil. The redirect makes Liliana a recipient of the damage, so
/// she is in the "dealt damage this way" set, but she was never a creature: at 0
/// loyalty she is put into the graveyard by the state-based action and the
/// "a creature ... would die" rider does not exile her. A creature of the same
/// cast that the shield does not cover is exiled (the rider is live).
#[test]
fn anger_of_the_gods_redirected_damage_does_not_exile_a_planeswalker() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P1 controls the shielded permanents: Bears (covered by the redirect) and the
    // planeswalker the redirect lands on.
    let shielded_bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let liliana = scenario.add_real_card(P1, "Liliana of the Veil", Zone::Battlefield, db);
    let sacrifice = scenario.add_real_card(P1, "Gideon's Sacrifice", Zone::Hand, db);
    // P0 controls a creature the shield does not cover.
    let exposed_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.with_mana_pool(P0, pool(&anger().mana));
    scenario.with_mana_pool(P1, pool(&[ManaType::White]));
    let anger_card = scenario
        .add_spell_to_hand(P0, "Anger of the Gods", false)
        .with_mana_cost(anger().mana_cost)
        .from_oracle_text(ANGER)
        .id();
    let mut board = RedirectBoard {
        runner: scenario.build(),
        anger: anger_card,
        sacrifice,
    };
    // Reach-guard for the fixture: the real planeswalker starts at 3 loyalty (counters
    // and field), so Anger's redirected 3 damage is exactly lethal to her (CR 120.3c).
    let starting = &board.runner.state().objects[&liliana];
    assert_eq!(starting.loyalty, Some(3));
    assert_eq!(starting.counters.get(&CounterType::Loyalty), Some(&3));

    let outcome = anger_answered_by_gideons_sacrifice(&mut board, liliana);
    let dealt_to = |target: ObjectId| -> Vec<u32> {
        outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamageDealt {
                    source_id,
                    target: TargetRef::Object(object),
                    amount,
                    ..
                } if *source_id == anger_card && *object == target => Some(*amount),
                _ => None,
            })
            .collect()
    };

    // Reach-guards, asserted before the discriminating outcome:
    // (a) the redirect fired and Liliana was dealt Anger's damage;
    assert_eq!(
        dealt_to(liliana),
        vec![3],
        "the redirect must deal Anger's 3 damage to Liliana"
    );
    // (b) the shield covered the Bears it protects: nothing dealt, nothing marked;
    assert!(dealt_to(shielded_bears).is_empty());
    let shielded = &outcome.state().objects[&shielded_bears];
    assert_eq!(shielded.zone, Zone::Battlefield);
    assert_eq!(shielded.damage_marked, 0);
    // (c) the rider is live: the creature the shield does not cover was dealt 3 and
    // exiled, so Liliana's destination below is the creature gate, not a missing
    // rider;
    assert_eq!(dealt_to(exposed_bears), vec![3]);
    assert_eq!(outcome.state().objects[&exposed_bears].zone, Zone::Exile);

    // CR 704.5i + CR 109.2: a planeswalker with 0 loyalty goes to the graveyard; the
    // rider names creatures only.
    assert_eq!(
        outcome.state().objects[&liliana].zone,
        Zone::Graveyard,
        "Liliana was never a creature, so the creature-only rider must not exile her"
    );
}

/// F1 sibling, the creature half of the type gate. Rulings for Annihilating Fire
/// and Pillar of Flame: a creature dealt damage "even if it wasn't the target
/// (because the damage was redirected somehow)" is exiled. CR 700.4 + CR 614.1a:
/// P1 shields two Grizzly Bears with Gideon's Sacrifice choosing Bears B; B takes
/// both 3-damage events (its own and the redirected one) and dies, and is exiled;
/// Bears A, whose damage was redirected away, is untouched. The creature conjunct
/// must not be inverted or swapped for another card type.
#[test]
fn anger_of_the_gods_exiles_a_creature_that_received_redirected_damage() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let redirected_away = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let recipient = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let sacrifice = scenario.add_real_card(P1, "Gideon's Sacrifice", Zone::Hand, db);
    let exposed_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.with_mana_pool(P0, pool(&anger().mana));
    scenario.with_mana_pool(P1, pool(&[ManaType::White]));
    let anger_card = scenario
        .add_spell_to_hand(P0, "Anger of the Gods", false)
        .with_mana_cost(anger().mana_cost)
        .from_oracle_text(ANGER)
        .id();
    let mut board = RedirectBoard {
        runner: scenario.build(),
        anger: anger_card,
        sacrifice,
    };

    let outcome = anger_answered_by_gideons_sacrifice(&mut board, recipient);

    // Reach-guards: the redirect fired (the recipient was dealt both events, the
    // shielded Bears none) and the rider is live (the exposed Bears is exiled).
    let dealt_to = |target: ObjectId| -> Vec<u32> {
        outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamageDealt {
                    source_id,
                    target: TargetRef::Object(object),
                    amount,
                    ..
                } if *source_id == anger_card && *object == target => Some(*amount),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        dealt_to(recipient),
        vec![3, 3],
        "the chosen Bears is dealt its own damage and the redirected damage"
    );
    assert!(dealt_to(redirected_away).is_empty());
    let away = &outcome.state().objects[&redirected_away];
    assert_eq!(away.zone, Zone::Battlefield);
    assert_eq!(away.damage_marked, 0);
    assert_eq!(outcome.state().objects[&exposed_bears].zone, Zone::Exile);

    assert_eq!(
        outcome.state().objects[&recipient].zone,
        Zone::Exile,
        "a creature that took redirected damage is still 'dealt damage this way'"
    );
}

/// F1 sibling on the single-target route (the eight "any target" faces share the
/// detector). CR 109.2 + CR 704.5i + CR 120.3c: Touch of the Void dealing 3 damage
/// to Liliana of the Veil (loyalty 3) leaves her in the graveyard, since she is not
/// "a creature"; a second Touch of the Void on a Grizzly Bears exiles it, which is
/// the positive reach-guard that the rider is live on this route. Touch of the Void
/// is built from its verbatim Oracle text (as Anger is).
#[test]
fn touch_of_the_void_on_a_planeswalker_does_not_exile_it() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let liliana = scenario.add_real_card(P1, "Liliana of the Veil", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let touch_of_the_void = |scenario: &mut GameScenario| {
        scenario
            .add_spell_to_hand(P0, "Touch of the Void", false)
            .with_mana_cost(cost(2, &[ManaCostShard::Red]))
            .from_oracle_text_with_keywords(&["Devoid"], TOUCH_OF_THE_VOID)
            .id()
    };
    let at_planeswalker = touch_of_the_void(&mut scenario);
    let at_creature = touch_of_the_void(&mut scenario);
    let touch_mana = [ManaType::Red, ManaType::Colorless, ManaType::Colorless];
    scenario.with_mana_pool(P0, pool(&touch_mana));
    let mut runner = scenario.build();

    let outcome = runner
        .cast(at_planeswalker)
        .target_object(liliana)
        .resolve();
    // Reach-guard: Liliana was dealt exactly her loyalty.
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Object(target),
                amount: 3,
                ..
            } if *source_id == at_planeswalker && *target == liliana
        )),
        "Touch of the Void must deal 3 damage to Liliana"
    );
    assert_eq!(
        runner.state().objects[&liliana].zone,
        Zone::Graveyard,
        "Liliana was never a creature, so the creature-only rider must not exile her"
    );

    add_mana(&mut runner, &touch_mana);
    runner.cast(at_creature).target_object(bears).resolve();
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Exile,
        "reach-guard: the same rider exiles a creature it dealt lethal damage to"
    );
}

/// F1 timing (derived, not ruled): CR 614.1 + CR 613.1d + CR 700.4 + CR 109.2. A
/// replacement effect reads its noun when the event would happen, so a damaged
/// creature that is no longer a creature when it would die is not "a creature
/// ... would die". Anger of the Gods damages two Pillarfield Oxen (toughness 4,
/// both survive); Song of the Dryads makes one a Forest land, Suffering destroys
/// it and it goes to the graveyard, while Murder on the other (still a creature)
/// exiles it.
#[test]
fn anger_of_the_gods_rider_does_not_exile_a_damaged_creature_that_is_no_longer_a_creature() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forested = scenario.add_real_card(P1, "Pillarfield Ox", Zone::Battlefield, db);
    let control = scenario.add_real_card(P1, "Pillarfield Ox", Zone::Battlefield, db);
    let song = scenario.add_real_card(P0, "Song of the Dryads", Zone::Hand, db);
    let suffering = scenario.add_real_card(P0, "Suffering", Zone::Hand, db);
    let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    scenario.with_mana_pool(P0, pool(&anger().mana));
    let anger_card = scenario
        .add_spell_to_hand(P0, "Anger of the Gods", false)
        .with_mana_cost(anger().mana_cost)
        .from_oracle_text(ANGER)
        .id();
    let mut runner = scenario.build();

    runner.cast(anger_card).resolve();
    for ox in [forested, control] {
        let object = &runner.state().objects[&ox];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(
            object.damage_marked, 3,
            "reach-guard: Anger damaged {ox:?} without killing it"
        );
    }

    add_mana(
        &mut runner,
        &[ManaType::Green, ManaType::Colorless, ManaType::Colorless],
    );
    runner.cast(song).target_object(forested).resolve();
    {
        // Reach-guards, before the discriminating destroy: the Aura made it a
        // noncreature (layer-evaluated types), it is still damaged, and the rider
        // is installed on it.
        let object = &runner.state().objects[&forested];
        assert_eq!(object.zone, Zone::Battlefield);
        assert!(
            !object.card_types.core_types.contains(&CoreType::Creature),
            "Song of the Dryads must make the Ox a noncreature: {:?}",
            object.card_types
        );
        assert_eq!(object.damage_marked, 3);
        assert!(object
            .replacement_definitions
            .iter_all()
            .any(|def| def.event == ReplacementEvent::Moved));
    }

    add_mana(
        &mut runner,
        &[
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    runner
        .cast(suffering)
        .modal_back_face(false)
        .target_object(forested)
        .resolve();
    assert_eq!(
        runner.state().objects[&forested].zone,
        Zone::Graveyard,
        "an Ox that is no longer a creature is not 'a creature ... would die'"
    );

    add_mana(
        &mut runner,
        &[ManaType::Black, ManaType::Black, ManaType::Colorless],
    );
    runner.cast(murder).target_object(control).resolve();
    assert_eq!(
        runner.state().objects[&control].zone,
        Zone::Exile,
        "reach-guard: the rider is live and its creature conjunct passes for a creature"
    );
}
