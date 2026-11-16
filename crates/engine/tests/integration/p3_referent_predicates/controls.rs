//! Preservation controls: supported cards whose gate already lowers to `AbilityCondition::TargetMatchesFilter`
//! (past-tense LKI after a battlefield / stack exit, present-tense live reads). They pass at base and must stay
//! green through every stage. One runtime control per property x tense x exit zone; each cell has a positive and a
//! negative so a flipped gate is detected.

use super::fixture::*;
use super::oracle::*;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::zones::Zone;

fn scried(events: &[GameEvent]) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::Scry,
                ..
            }
        )
    })
}

/// P0 casts the real spell `name` at P1's filler permanent (built by `victim`), through the production cast path.
fn cast_at(
    name: &str,
    oracle: &str,
    printed_cost: &str,
    is_instant: bool,
    victim: impl Fn(&mut GameScenario) -> ObjectId,
) -> (GameRunner, ObjectId, Vec<GameEvent>) {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[printed_cost], 0));
    add_counter_holder(&mut sc);
    sc.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let v = victim(&mut sc);
    let spell = sc
        .add_spell_to_hand_from_oracle(P0, name, is_instant, oracle)
        .with_mana_cost(cost(printed_cost))
        .id();
    let mut r = sc.build();
    let out = r.cast(spell).target_object(v).resolve();
    let events = out.events().to_vec();
    (r, v, events)
}

fn creature(mvn: u32) -> impl Fn(&mut GameScenario) -> ObjectId {
    move |sc| {
        sc.add_creature(P1, "Filler Victim", 2, 2)
            .with_mana_cost(ManaCost::generic(mvn))
            .id()
    }
}

fn artifact(mvn: u32) -> impl Fn(&mut GameScenario) -> ObjectId {
    move |sc| {
        let mut b = sc.add_creature(P1, "Filler Artifact", 1, 1);
        b.with_mana_cost(ManaCost::generic(mvn));
        b.as_artifact();
        b.id()
    }
}

fn tokens_named(r: &GameRunner, name: &str) -> usize {
    r.state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.name == name)
        .count()
}

// ---- past tense, battlefield exit, mana value ----

#[test]
fn p3s2a_ctl_serum_snare_bounce_then_proliferate() {
    for (mvn, expect) in [(2, true), (5, false)] {
        let (r, v, _) = cast_at(
            "Serum Snare",
            SERUM_SNARE,
            SERUM_SNARE_COST,
            true,
            creature(mvn),
        );
        assert_eq!(
            zone(r.state(), v),
            Zone::Hand,
            "reach guard: returned to hand (MV {mvn})"
        );
        assert_eq!(
            is_proliferate(&r.state().waiting_for),
            expect,
            "MV {mvn}: proliferate iff <= 3"
        );
    }
}

#[test]
fn p3s2a_ctl_tainted_treats_food_iff_mv_4_or_less() {
    for (mvn, food) in [(2, 1), (6, 0)] {
        let (r, v, _) = cast_at(
            "Tainted Treats",
            TAINTED_TREATS,
            TAINTED_TREATS_COST,
            true,
            creature(mvn),
        );
        assert_eq!(
            zone(r.state(), v),
            Zone::Graveyard,
            "reach guard: destroyed (MV {mvn})"
        );
        assert_eq!(tokens_named(&r, "Food"), food, "MV {mvn}");
    }
}

#[test]
fn p3s2a_ctl_extinguish_the_light_gains_3_iff_mv_3_or_less() {
    for (mvn, gain) in [(2, 3), (5, 0)] {
        let (r, v, _) = cast_at(
            "Extinguish the Light",
            EXTINGUISH_THE_LIGHT,
            EXTINGUISH_THE_LIGHT_COST,
            true,
            creature(mvn),
        );
        assert_eq!(
            zone(r.state(), v),
            Zone::Graveyard,
            "reach guard: destroyed (MV {mvn})"
        );
        assert_eq!(life(r.state(), P0), 20 + gain, "MV {mvn}");
    }
}

#[test]
fn p3s2a_ctl_fading_hope_scries_iff_mv_3_or_less() {
    for (mvn, expect) in [(2, true), (5, false)] {
        let (r, v, ev) = cast_at(
            "Fading Hope",
            FADING_HOPE,
            FADING_HOPE_COST,
            true,
            creature(mvn),
        );
        assert_eq!(
            zone(r.state(), v),
            Zone::Hand,
            "reach guard: returned (MV {mvn})"
        );
        assert_eq!(scried(&ev), expect, "MV {mvn}");
    }
}

#[test]
fn p3s2a_ctl_perilous_voyage_scries_iff_mv_2_or_less() {
    for (mvn, expect) in [(2, true), (4, false)] {
        let (r, v, ev) = cast_at(
            "Perilous Voyage",
            PERILOUS_VOYAGE,
            PERILOUS_VOYAGE_COST,
            true,
            creature(mvn),
        );
        assert_eq!(
            zone(r.state(), v),
            Zone::Hand,
            "reach guard: returned (MV {mvn})"
        );
        assert_eq!(scried(&ev), expect, "MV {mvn}");
    }
}

#[test]
fn p3s2a_ctl_seedship_impact_lander_iff_mv_2_or_less() {
    for (mvn, lander) in [(1, 1), (4, 0)] {
        let (r, v, _) = cast_at(
            "Seedship Impact",
            SEEDSHIP_IMPACT,
            SEEDSHIP_IMPACT_COST,
            true,
            artifact(mvn),
        );
        assert_eq!(
            zone(r.state(), v),
            Zone::Graveyard,
            "reach guard: destroyed (MV {mvn})"
        );
        assert_eq!(tokens_named(&r, "Lander"), lander, "MV {mvn}");
    }
}

#[test]
fn p3s2a_ctl_raze_to_the_ground_draws_iff_mv_1_or_less() {
    for (mvn, drawn) in [(1, 1), (3, 0)] {
        let (r, v, _) = cast_at(
            "Raze to the Ground",
            RAZE_TO_THE_GROUND,
            RAZE_TO_THE_GROUND_COST,
            false,
            artifact(mvn),
        );
        assert_eq!(
            zone(r.state(), v),
            Zone::Graveyard,
            "reach guard: destroyed (MV {mvn})"
        );
        assert_eq!(r.state().players[0].library.len(), 3 - drawn, "MV {mvn}");
    }
}

// ---- past tense, battlefield exit, status / supertype ----

#[test]
fn p3s2a_ctl_brackish_blunder_map_iff_it_was_tapped() {
    for (tapped, maps) in [(true, 1), (false, 0)] {
        let mut sc = scenario();
        sc.with_mana_pool(P0, mana_for(&[BRACKISH_BLUNDER_COST], 0));
        let v = { sc.add_creature(P1, "Filler Victim", 2, 2).id() };
        let spell = sc
            .add_spell_to_hand_from_oracle(P0, "Brackish Blunder", true, BRACKISH_BLUNDER)
            .with_mana_cost(cost(BRACKISH_BLUNDER_COST))
            .id();
        let mut r = sc.build();
        r.state_mut().objects.get_mut(&v).expect("victim").tapped = tapped;
        assert_eq!(
            r.state().objects[&v].tapped,
            tapped,
            "fixture: tapped state"
        );
        r.cast(spell).target_object(v).resolve();
        assert_eq!(
            zone(r.state(), v),
            Zone::Hand,
            "reach guard: returned (tapped {tapped})"
        );
        assert_eq!(tokens_named(&r, "Map"), maps, "tapped {tapped}");
    }
}

fn nonbasic_land(basic: bool) -> impl Fn(&mut GameScenario) -> ObjectId {
    // A basic Forest, or a textless land with no supertypes (nonbasic).
    move |sc| {
        if basic {
            sc.add_basic_land(P1, ManaColor::Green)
        } else {
            sc.add_land_from_oracle(P1, "Filler Land", "").id()
        }
    }
}

#[test]
fn p3s2a_ctl_molten_rain_two_damage_iff_land_was_nonbasic() {
    for (basic, dmg) in [(false, 2), (true, 0)] {
        let (r, v, _) = cast_at(
            "Molten Rain",
            MOLTEN_RAIN,
            MOLTEN_RAIN_COST,
            false,
            nonbasic_land(basic),
        );
        assert_eq!(
            zone(r.state(), v),
            Zone::Graveyard,
            "reach guard: destroyed (basic {basic})"
        );
        assert_eq!(life(r.state(), P1), 20 - dmg, "basic {basic}");
    }
}

#[test]
fn p3s2a_ctl_choking_sands_two_damage_iff_land_was_nonbasic() {
    for (basic, dmg) in [(false, 2), (true, 0)] {
        let (r, v, _) = cast_at(
            "Choking Sands",
            CHOKING_SANDS,
            CHOKING_SANDS_COST,
            false,
            nonbasic_land(basic),
        );
        assert_eq!(
            zone(r.state(), v),
            Zone::Graveyard,
            "reach guard: destroyed (basic {basic})"
        );
        assert_eq!(life(r.state(), P1), 20 - dmg, "basic {basic}");
    }
}

// ---- present tense, live read ----

#[test]
fn p3s2a_ctl_fatal_push_destroys_only_mv_2_or_less() {
    for (mvn, destroyed) in [(2, true), (3, false)] {
        let (r, v, _) = cast_at(
            "Fatal Push",
            FATAL_PUSH,
            FATAL_PUSH_COST,
            true,
            creature(mvn),
        );
        let z = zone(r.state(), v);
        assert_eq!(
            z == Zone::Graveyard,
            destroyed,
            "MV {mvn} (no revolt): destroyed iff <= 2, zone {z:?}"
        );
        assert!(
            z == Zone::Graveyard || z == Zone::Battlefield,
            "reach guard: the creature is accounted for"
        );
    }
}

#[test]
fn p3s2a_ctl_anoint_with_affliction_exiles_only_mv_3_or_less() {
    for (mvn, exiled) in [(3, true), (4, false)] {
        let (r, v, _) = cast_at(
            "Anoint with Affliction",
            ANOINT_WITH_AFFLICTION,
            ANOINT_WITH_AFFLICTION_COST,
            true,
            creature(mvn),
        );
        let z = zone(r.state(), v);
        assert_eq!(
            z == Zone::Exile,
            exiled,
            "MV {mvn}: exiled iff <= 3, zone {z:?}"
        );
    }
}

#[test]
fn p3s2a_ctl_burning_hands_six_damage_iff_target_is_green() {
    for (green, dmg) in [(true, 6u32), (false, 2u32)] {
        let (r, v, _) = cast_at(
            "Burning Hands",
            BURNING_HANDS,
            BURNING_HANDS_COST,
            true,
            move |sc| {
                let mut b = sc.add_creature(P1, "Filler Victim", 1, 12);
                b.with_color(vec![if green {
                    ManaColor::Green
                } else {
                    ManaColor::Blue
                }]);
                b.id()
            },
        );
        assert_eq!(r.state().objects[&v].damage_marked, dmg, "green {green}");
    }
}

// ---- past tense, stack exit (the countered spell) ----

fn counter_creature_spell(
    name: &str,
    oracle: &str,
    printed_cost: &str,
    legendary: bool,
) -> GameRunner {
    let mut sc = scenario();
    sc.with_mana_pool(P0, mana_for(&[], 1));
    sc.with_mana_pool(P1, mana_for(&[printed_cost], 0));
    sc.with_library_top(P1, &["Island", "Island", "Island"]);
    let target = {
        let mut b = sc.add_creature_to_hand(P0, "Filler Spell", 2, 2);
        b.with_mana_cost(ManaCost::generic(1));
        if legendary {
            b.as_legendary();
        }
        b.id()
    };
    let counter = sc
        .add_spell_to_hand_from_oracle(P1, name, true, oracle)
        .with_mana_cost(cost(printed_cost))
        .id();
    let mut r = sc.build();
    cast_spell(&mut r, target);
    r.act(GameAction::PassPriority).expect("P0 passes");
    cast_spell(&mut r, counter);
    let w = drain(&mut r);
    assert!(
        matches!(w, WaitingFor::Priority { .. }),
        "resolution completed, got {w:?}"
    );
    assert_eq!(
        zone(r.state(), target),
        Zone::Graveyard,
        "reach guard: countered"
    );
    r
}

#[test]
fn p3s2a_ctl_glorious_gale_ring_tempts_iff_the_countered_spell_was_legendary() {
    for (legendary, tempted) in [(true, true), (false, false)] {
        let r = counter_creature_spell(
            "Glorious Gale",
            GLORIOUS_GALE,
            GLORIOUS_GALE_COST,
            legendary,
        );
        let level = r.state().ring_level.get(&P1).copied().unwrap_or(0);
        assert_eq!(
            level > 0,
            tempted,
            "legendary {legendary}: ring level {level}"
        );
    }
}

/// Statute of Denial: a gate on a Draw head suppresses its Discard continuation (the shape Sound the Trumpets uses).
#[test]
fn p3s2a_ctl_statute_of_denial_gate_on_a_draw_head() {
    for (blue, drew) in [(true, true), (false, false)] {
        let mut sc = scenario();
        sc.with_mana_pool(P0, mana_for(&[], 1));
        sc.with_mana_pool(P1, mana_for(&[STATUTE_OF_DENIAL_COST], 0));
        sc.with_library_top(P1, &["Island", "Island", "Island"]);
        if blue {
            sc.add_creature(P1, "Filler Blue Creature", 1, 1)
                .with_color(vec![ManaColor::Blue]);
        }
        let target = sc
            .add_creature_to_hand(P0, "Filler Spell", 2, 2)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let counter = sc
            .add_spell_to_hand_from_oracle(P1, "Statute of Denial", true, STATUTE_OF_DENIAL)
            .with_mana_cost(cost(STATUTE_OF_DENIAL_COST))
            .id();
        let mut r = sc.build();
        cast_spell(&mut r, target);
        r.act(GameAction::PassPriority).expect("pass");
        cast_spell(&mut r, counter);
        let w = drain(&mut r);
        assert_eq!(
            zone(r.state(), target),
            Zone::Graveyard,
            "reach guard: countered"
        );
        assert_eq!(
            r.state().players[1].library.len() == 2,
            drew,
            "blue {blue}: library {}",
            r.state().players[1].library.len()
        );
        assert_eq!(r.state().players[1].graveyard.len(), if drew { 2 } else { 1 }, "blue {blue}: the drawn Island is discarded iff drew (hand is that single card); waiting {w:?}");
    }
}
