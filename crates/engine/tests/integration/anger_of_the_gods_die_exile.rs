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
//! The single-target sibling (Pillar of Flame, Demonfire, ...) is the separate
//! phase-rs/phase#10965 and is not covered here.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, CastingVariantFace, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

// Verbatim Oracle text (MTGJSON 5.3.0+20261116).
const ANGER: &str = "Anger of the Gods deals 3 damage to each creature. If a creature dealt damage this way would die this turn, exile it instead.";
const CRUSH_THE_WEAK: &str = "Crush the Weak deals 2 damage to each creature. If a creature dealt damage this way would die this turn, exile it instead.\nForetell {R} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";
const YAMABUSHIS_STORM: &str = "Yamabushi's Storm deals 1 damage to each creature. If a creature dealt damage this way would die this turn, exile it instead.";
const UNDERWORLD_FIRES: &str = "Underworld Fires deals 1 damage to each creature and each planeswalker. If a permanent dealt damage this way would die this turn, exile it instead.";
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
