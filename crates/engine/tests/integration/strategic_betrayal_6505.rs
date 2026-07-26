//! Issue #6505: Strategic Betrayal ({1}{B} Sorcery) — verbatim Oracle text:
//!   "Target opponent exiles a creature they control and their graveyard."
//!
//! Rules-correct behavior (CR 115.1a + CR 115.10a/b + CR 702.21a + CR 109.4):
//! the spell targets ONLY the opponent (CR 115.1a). That opponent then CHOOSES
//! — does not TARGET — a creature THEY control to exile, and exiles THEIR
//! graveyard. Because the creature is chosen at resolution rather than targeted,
//! no `BecomesTarget` event fires, so Ward on the opponent's OWN creature never
//! triggers (CR 115.10a: being affected by a spell is not being targeted).
//!
//! The bug had two halves:
//!   (A) origin-inference leaked `Zone::Graveyard` onto the battlefield-creature
//!       leg (a trailing "and their graveyard" conjunct polluted the scan), and
//!   (B) the creature leg was emitted as a `Stack` target (Ward) chosen by the
//!       wrong actor (the caster instead of the target opponent).
//!
//! These tests are per-mechanism revert-sensitive; each notes which revert flips
//! it (see the file header in the PR for the RED→GREEN matrix).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, TargetChoiceTiming, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, WardCost};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Strategic Betrayal — verbatim Oracle text (used to build every test card).
const SB_ORACLE: &str = "Target opponent exiles a creature they control and their graveyard.";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Walk an ability chain (effect + sub_ability + else_ability) for any
/// `Effect::Unimplemented` — the coverage-honesty reach-guard shared by every
/// positive test so a vacuous pass (parse failed to a strict `Unimplemented`)
/// is impossible.
fn chain_has_unimplemented(def: &AbilityDefinition) -> bool {
    fn effect_unimplemented(effect: &Effect) -> bool {
        matches!(effect, Effect::Unimplemented { .. })
    }
    effect_unimplemented(&def.effect)
        || def
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
}

/// The single parsed spell ability for a verbatim sorcery body.
fn parse_sb() -> AbilityDefinition {
    let parsed = parse_oracle_text(SB_ORACLE, "Strategic Betrayal", &[], &[], &[]);
    assert_eq!(
        parsed.abilities.len(),
        1,
        "SB parses to exactly one spell ability; got {:#?}",
        parsed.abilities
    );
    parsed.abilities.into_iter().next().unwrap()
}

/// Find the first `ChangeZone` (single, the creature leg) in a chain.
fn find_change_zone(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
    let mut cursor = Some(def);
    while let Some(node) = cursor {
        if matches!(*node.effect, Effect::ChangeZone { .. }) {
            return Some(node);
        }
        cursor = node.sub_ability.as_deref();
    }
    None
}

/// Find the first `ChangeZoneAll` (mass, the graveyard leg) in a chain.
fn find_change_zone_all(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
    let mut cursor = Some(def);
    while let Some(node) = cursor {
        if matches!(*node.effect, Effect::ChangeZoneAll { .. }) {
            return Some(node);
        }
        cursor = node.sub_ability.as_deref();
    }
    None
}

fn ward2_creature(
    scenario: &mut GameScenario,
    controller: engine::types::player::PlayerId,
    p: i32,
    t: i32,
    name: &str,
) -> ObjectId {
    scenario
        .add_creature(controller, name, p, t)
        .with_keyword(Keyword::Ward(WardCost::Mana(ManaCost::generic(2))))
        .id()
}

fn graveyard_ids(runner: &GameRunner, player: engine::types::player::PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .graveyard
        .iter()
        .copied()
        .collect()
}

// ---------------------------------------------------------------------------
// 1. END-TO-END — the headline behavior.
// ---------------------------------------------------------------------------

/// Opponent controls TWO creatures (one warded) plus graveyard cards. Casting
/// SB on the opponent must: (a) raise NO Ward payment/choice; (b) prompt the
/// OPPONENT with an `EffectZoneChoice` offering its two battlefield creatures;
/// (c) exile the chosen creature; (d) exile the opponent's whole graveyard.
///
/// Revert sensitivity: reverting the timing stamp flips (a) to a Ward
/// `UnlessPayment`; reverting the `scoped_player` stamp flips (b)'s player to
/// the caster; reverting the origin slice flips (b)'s offered set to graveyard.
#[test]
fn end_to_end_opponent_chooses_creature_and_loses_graveyard_no_ward() {
    // Reach-guard: the spell parses with zero Unimplemented before we cast it.
    let sb = parse_sb();
    assert!(
        !chain_has_unimplemented(&sb),
        "SB must parse with no strict-failure Unimplemented; got {sb:#?}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let plain = scenario.add_creature(P1, "Opp Plain Beast", 2, 2).id();
    let warded = ward2_creature(&mut scenario, P1, 3, 3, "Opp Warded Beast");
    scenario.with_graveyard(P1, &["Grave Relic A", "Grave Relic B"]);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Strategic Betrayal", false, SB_ORACLE)
        .id();

    let mut runner = scenario.build();
    let gy = graveyard_ids(&runner, P1);
    assert_eq!(gy.len(), 2, "opponent graveyard seeded with two cards");

    let outcome = runner.cast(spell).target_player(P1).resolve();

    // (a) + (b): no Ward, opponent is the chooser, both battlefield creatures
    // offered — and NOT the graveyard cards.
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P1, "the OPPONENT chooses the exiled creature");
            assert!(
                cards.contains(&plain) && cards.contains(&warded),
                "both of the opponent's battlefield creatures must be offered; got {cards:?}"
            );
            for g in &gy {
                assert!(
                    !cards.contains(g),
                    "graveyard cards must not be offered to the creature leg; got {cards:?}"
                );
            }
        }
        WaitingFor::UnlessPayment { .. }
        | WaitingFor::WardSacrificeChoice { .. }
        | WaitingFor::WardDiscardChoice { .. } => {
            panic!(
                "Ward must NOT fire — the creature is chosen at resolution, not targeted; got {:?}",
                outcome.final_waiting_for()
            );
        }
        other => panic!("expected the opponent's creature EffectZoneChoice; got {other:?}"),
    }

    // (c): choose the plain creature.
    runner
        .act(GameAction::SelectCards { cards: vec![plain] })
        .expect("opponent selects a creature to exile");

    assert_eq!(
        runner.state().objects[&plain].zone,
        Zone::Exile,
        "the chosen creature is exiled"
    );
    assert_eq!(
        runner.state().objects[&warded].zone,
        Zone::Battlefield,
        "the unchosen creature stays on the battlefield"
    );
    // (d): the opponent's graveyard is exiled wholesale.
    for g in &gy {
        assert_eq!(
            runner.state().objects[g].zone,
            Zone::Exile,
            "every card in the opponent's graveyard is exiled"
        );
    }
    assert!(
        graveyard_ids(&runner, P1).is_empty(),
        "the opponent's graveyard is now empty"
    );
}

// ---------------------------------------------------------------------------
// 2. B(1)(c) ALONE — Ward isolation via the Resolution timing stamp.
// ---------------------------------------------------------------------------

/// The creature leg parses as a Resolution-timed, non-targeted pick (SHAPE), and
/// at runtime a lone warded creature raises NO Ward. Reverting ONLY the timing
/// stamp restores `TargetChoiceTiming::Stack`, which re-introduces the
/// `BecomesTarget` → Ward `UnlessPayment` path.
#[test]
fn creature_leg_is_resolution_chosen_so_ward_never_fires() {
    // SHAPE: creature leg carries Resolution timing (default is Stack).
    let sb = parse_sb();
    let creature = find_change_zone(&sb).expect("SB has a single-creature ChangeZone leg");
    assert_eq!(
        creature.target_choice_timing,
        TargetChoiceTiming::Resolution,
        "the creature leg must be resolution-chosen (non-targeted), not a Stack target"
    );

    // Runtime: a single warded creature (auto-selected once chosen at
    // resolution) must not produce a Ward payment window.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warded = ward2_creature(&mut scenario, P1, 3, 3, "Lone Warded Beast");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Strategic Betrayal", false, SB_ORACLE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();
    assert!(
        !matches!(
            outcome.final_waiting_for(),
            WaitingFor::UnlessPayment { .. }
                | WaitingFor::WardSacrificeChoice { .. }
                | WaitingFor::WardDiscardChoice { .. }
        ),
        "Ward must stay silent on a resolution-chosen creature; got {:?}",
        outcome.final_waiting_for()
    );
    // Reach-guard: the single warded creature was exiled (we reached resolution,
    // proving the no-Ward assertion is not vacuous).
    assert_eq!(
        runner.state().objects[&warded].zone,
        Zone::Exile,
        "the lone warded creature is exiled at resolution"
    );
}

// ---------------------------------------------------------------------------
// 3. B(2) ALONE — chooser isolation via the scoped_player stamp.
// ---------------------------------------------------------------------------

/// The OPPONENT (not the caster) is the chooser of the exiled creature.
/// Reverting ONLY the `scoped_player` stamp in `stack::resolve_top` flips the
/// `EffectZoneChoice.player` back to the caster.
#[test]
fn opponent_not_caster_is_the_chooser() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Two creatures so the choice is a real prompt (not an auto-pick).
    scenario.add_creature(P1, "Opp A", 2, 2);
    scenario.add_creature(P1, "Opp B", 2, 2);
    // The caster also controls creatures — if the chooser wrongly resolved to
    // the caster, its own creatures (not the opponent's) would be offered.
    let caster_creature = scenario.add_creature(P0, "Caster Own", 4, 4).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Strategic Betrayal", false, SB_ORACLE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(
                *player, P1,
                "the target OPPONENT is the chooser, not the caster"
            );
            assert!(
                !cards.contains(&caster_creature),
                "the caster's own creature must never be an eligible pick; got {cards:?}"
            );
        }
        other => panic!("expected the opponent's EffectZoneChoice; got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 4. PART A ALONE — the eligible set is the BATTLEFIELD, not the graveyard.
// ---------------------------------------------------------------------------

/// With graveyard cards present, the creature leg still offers BATTLEFIELD
/// creatures. Reverting the origin slice (letting the trailing "and their
/// graveyard" leak `Zone::Graveyard` onto the creature leg) would make the
/// eligible set the graveyard cards instead. Bites imperative.rs:8380.
#[test]
fn creature_leg_selects_from_battlefield_not_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bf_a = scenario.add_creature(P1, "BF Creature A", 2, 2).id();
    let bf_b = scenario.add_creature(P1, "BF Creature B", 2, 2).id();
    scenario.with_graveyard(P1, &["GY Card X", "GY Card Y"]);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Strategic Betrayal", false, SB_ORACLE)
        .id();
    let mut runner = scenario.build();
    let gy = graveyard_ids(&runner, P1);

    let outcome = runner.cast(spell).target_player(P1).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            zone,
            ..
        } => {
            assert_eq!(*player, P1);
            assert_eq!(
                *zone,
                Zone::Battlefield,
                "the creature leg must select from the battlefield"
            );
            assert!(
                cards.contains(&bf_a) && cards.contains(&bf_b),
                "battlefield creatures are the eligible set; got {cards:?}"
            );
            for g in &gy {
                assert!(
                    !cards.contains(g),
                    "graveyard cards must not be eligible for the creature leg; got {cards:?}"
                );
            }
        }
        other => panic!("expected a battlefield EffectZoneChoice; got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 5. CASTER-CHOOSER control (the BLOCKING-2 negative).
// ---------------------------------------------------------------------------

/// A caster-subject "you control" exile keeps the CASTER as chooser — the
/// scope-set and scoped_player stamp key on a single Player TARGET plus a
/// ScopedPlayer filter, neither of which a "you control" imperative produces.
#[test]
fn caster_subject_exile_keeps_caster_as_chooser() {
    const CASTER_EXILE: &str = "Exile a creature you control and their graveyard.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Two caster creatures so the caster's pick is a real prompt.
    let own_a = scenario.add_creature(P0, "Own A", 2, 2).id();
    let own_b = scenario.add_creature(P0, "Own B", 2, 2).id();
    // An opponent creature which must NEVER be offered to the caster.
    let opp = scenario.add_creature(P1, "Opp Creature", 5, 5).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Betrayal Control", false, CASTER_EXILE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the CASTER chooses their own creature");
            assert!(
                cards.contains(&own_a) && cards.contains(&own_b),
                "the caster's creatures are offered; got {cards:?}"
            );
            assert!(
                !cards.contains(&opp),
                "the opponent's creature must not be offered to a 'you control' exile; got {cards:?}"
            );
        }
        other => panic!("expected the caster's own EffectZoneChoice; got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 6. NEGATIVE — Ward is NOT over-suppressed.
// ---------------------------------------------------------------------------

/// A plain "Exile target creature." still targets the warded creature, so Ward
/// STILL fires (the timing fix is scoped to the resolution-picked SB shape, not
/// to all exiles). Reach-guard: declining the Ward payment counters the spell,
/// so the creature is NOT exiled.
#[test]
fn plain_targeted_exile_still_triggers_ward() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warded = ward2_creature(&mut scenario, P1, 3, 3, "Warded Target");

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder-Exile", true, "Exile target creature.")
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(warded).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::UnlessPayment { .. }
        ),
        "Ward must fire on a genuinely targeted exile; got {:?}",
        outcome.final_waiting_for()
    );

    // Reach-guard: decline the Ward cost — the exile spell is countered and the
    // creature stays on the battlefield.
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining Ward payment is legal");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&warded].zone,
        Zone::Battlefield,
        "unpaid Ward counters the exile; the creature survives"
    );
}

// ---------------------------------------------------------------------------
// 7. SIBLING regression — Diabolic Edict (sacrifice) is unchanged.
// ---------------------------------------------------------------------------

/// "Target player sacrifices a creature." routes through the sacrifice path
/// (untouched by this fix). The target player is still the chooser and no Ward
/// fires (sacrifice is not targeting the creature).
#[test]
fn target_player_sacrifice_still_binds_chooser_no_ward() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Sac A", 2, 2);
    let warded = ward2_creature(&mut scenario, P1, 3, 3, "Warded Sac Target");

    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Diabolic Edict",
            true,
            "Target player sacrifices a creature.",
        )
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { player, .. } => {
            assert_eq!(*player, P1, "the target player chooses the sacrifice");
        }
        WaitingFor::UnlessPayment { .. }
        | WaitingFor::WardSacrificeChoice { .. }
        | WaitingFor::WardDiscardChoice { .. } => panic!(
            "sacrifice must not target the warded creature; got {:?}",
            outcome.final_waiting_for()
        ),
        other => panic!("expected the target player's sacrifice choice; got {other:?}"),
    }
    // Reach-guard: the warded creature is still eligible to be sacrificed (the
    // sacrifice path is not target-gated by Ward).
    let WaitingFor::EffectZoneChoice { cards, .. } = outcome.final_waiting_for().clone() else {
        unreachable!("asserted above");
    };
    assert!(
        cards.contains(&warded),
        "the warded creature is a legal sacrifice; got {cards:?}"
    );
}

// ---------------------------------------------------------------------------
// 8. PARSER SHAPE (labeled SHAPE, semantic accessors).
// ---------------------------------------------------------------------------

/// The full parsed shape: creature leg is a battlefield-origin, ScopedPlayer-
/// scoped, Resolution-timed ChangeZone; graveyard leg is a ChangeZoneAll whose
/// filter carries `Owned{ScopedPlayer}`; zero Unimplemented anywhere.
#[test]
fn shape_creature_scopedplayer_resolution_graveyard_owned_scopedplayer() {
    let sb = parse_sb();
    assert!(
        !chain_has_unimplemented(&sb),
        "zero Unimplemented; got {sb:#?}"
    );

    // Root is the player TargetOnly wrapper.
    assert!(
        matches!(&*sb.effect, Effect::TargetOnly { target } if target_is_opponent_like(target)),
        "root effect targets ONLY the opponent; got {:?}",
        sb.effect
    );

    let creature = find_change_zone(&sb).expect("creature ChangeZone leg");
    // Battlefield origin (Part A): origin inference must NOT leak Graveyard.
    if let Effect::ChangeZone {
        origin,
        target,
        destination,
        ..
    } = &*creature.effect
    {
        assert!(
            origin.is_none() || *origin == Some(Zone::Battlefield),
            "creature leg origin is the battlefield (not Graveyard); got {origin:?}"
        );
        assert_eq!(*destination, Zone::Exile, "creature leg exiles");
        assert!(
            filter_controller_is_scoped(target),
            "creature leg controller is ScopedPlayer; got {target:?}"
        );
    } else {
        panic!(
            "expected ChangeZone creature leg; got {:?}",
            creature.effect
        );
    }
    assert_eq!(
        creature.target_choice_timing,
        TargetChoiceTiming::Resolution,
        "creature leg is resolution-chosen"
    );

    let graveyard = find_change_zone_all(&sb).expect("graveyard ChangeZoneAll leg");
    if let Effect::ChangeZoneAll {
        target,
        destination,
        ..
    } = &*graveyard.effect
    {
        assert_eq!(*destination, Zone::Exile, "graveyard leg exiles");
        assert!(
            filter_owned_scoped(target),
            "graveyard leg carries Owned{{ScopedPlayer}}; got {target:?}"
        );
    } else {
        panic!(
            "expected ChangeZoneAll graveyard leg; got {:?}",
            graveyard.effect
        );
    }
}

/// Hostile fixture: an explicit "target" on the creature keeps it a stack
/// target (no scope-set, no Resolution stamp) — and still parses cleanly.
#[test]
fn shape_explicit_target_creature_stays_stack_target() {
    const EXPLICIT: &str = "Exile target creature and their graveyard.";
    let parsed = parse_oracle_text(EXPLICIT, "Explicit Betrayal", &[], &[], &[]);
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    assert!(
        !chain_has_unimplemented(root),
        "explicit-target compound exile parses cleanly; got {root:#?}"
    );
    // REQUIRED (not `if let`): the creature leg MUST exist, or the assertion
    // below would pass vacuously when no ChangeZone is found.
    let creature = find_change_zone(root)
        .expect("explicit-target compound exile has a creature ChangeZone leg");
    assert_eq!(
        creature.target_choice_timing,
        TargetChoiceTiming::Stack,
        "an explicit 'target creature' stays a Stack target"
    );
}

/// Hostile fixture for Part A site 3 (mod.rs:15143): "Exile all creatures they
/// control and their graveyard." The mass-creature + whole-graveyard COMPOUND is
/// not modelled by the parser (a pre-existing gap unaffected by this fix — the
/// bare imperative never enters `lower_subject_predicate_ast`, and the site-3
/// origin slice only changes the inferred zone, never whether the arm parses).
/// Coverage honesty (per the plan's fail-closed rule): the WHOLE clause must
/// stay an explicit strict failure carrying the full text — NOT a silently
/// narrowed partial that dropped the "and their graveyard" leg (a swallow bug).
#[test]
fn shape_exile_all_creatures_and_graveyard_stays_fail_closed() {
    const MASS: &str = "Exile all creatures they control and their graveyard.";
    let parsed = parse_oracle_text(MASS, "Mass Betrayal", &[], &[], &[]);
    assert_eq!(parsed.abilities.len(), 1, "got {:#?}", parsed.abilities);
    let root = &parsed.abilities[0];
    // The unmodelled compound stays fail-closed: an Unimplemented carrying the
    // full sentence, not a partial that silently kept only one leg.
    assert!(
        matches!(&*root.effect, Effect::Unimplemented { description, .. }
            if description.as_deref().is_some_and(|d| d.contains("their graveyard"))),
        "the unmodelled mass-creature + graveyard compound must stay a whole-clause \
         strict failure (fail-closed, no silent orphan); got {root:#?}"
    );
}

// ---------------------------------------------------------------------------
// 11. SIBLING (review follow-up) — the narrowed rewrite generalizes to a bare
//     "target player exiles a creature they control" (no graveyard leg).
// ---------------------------------------------------------------------------

/// Doomfall / Debt to the Kami class: a bare "Target opponent exiles a creature
/// they control." (the SB creature leg WITHOUT the graveyard conjunct). The
/// narrowed post-lowering `You → ScopedPlayer` rewrite must still fire, so the
/// target OPPONENT (not the caster) chooses from THEIR OWN creatures at
/// resolution, no `BecomesTarget` fires, and Ward stays silent. Reverting the
/// narrowing (dropping the ChangeZone-leg rewrite) flips the chooser/eligible set
/// back to the caster.
#[test]
fn sibling_bare_target_player_exile_they_control_binds_chooser_no_ward() {
    const SIBLING: &str = "Target opponent exiles a creature they control.";

    // SHAPE: single ChangeZone leg is ScopedPlayer-scoped + Resolution-timed.
    let parsed = parse_oracle_text(SIBLING, "Doomfall Mode", &[], &[], &[]);
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    assert!(
        !chain_has_unimplemented(root),
        "the bare sibling parses cleanly; got {root:#?}"
    );
    let creature = find_change_zone(root).expect("sibling has a creature ChangeZone leg");
    assert!(
        filter_controller_is_scoped(match &*creature.effect {
            Effect::ChangeZone { target, .. } => target,
            other => panic!("expected ChangeZone; got {other:?}"),
        }),
        "the sibling creature leg is ScopedPlayer-scoped; got {:?}",
        creature.effect
    );
    assert_eq!(
        creature.target_choice_timing,
        TargetChoiceTiming::Resolution,
        "the sibling creature leg is resolution-chosen"
    );

    // Runtime: opponent controls a warded + a plain creature; the caster also
    // controls a creature that must never be offered.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let plain = scenario.add_creature(P1, "Opp Plain", 2, 2).id();
    let warded = ward2_creature(&mut scenario, P1, 3, 3, "Opp Warded");
    let caster_own = scenario.add_creature(P0, "Caster Own", 4, 4).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Doomfall Mode", false, SIBLING)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P1, "the target OPPONENT chooses, not the caster");
            assert!(
                cards.contains(&plain) && cards.contains(&warded),
                "both of the opponent's creatures are offered; got {cards:?}"
            );
            assert!(
                !cards.contains(&caster_own),
                "the caster's own creature must never be offered; got {cards:?}"
            );
        }
        WaitingFor::UnlessPayment { .. }
        | WaitingFor::WardSacrificeChoice { .. }
        | WaitingFor::WardDiscardChoice { .. } => panic!(
            "Ward must NOT fire — the creature is chosen at resolution, not targeted; got {:?}",
            outcome.final_waiting_for()
        ),
        other => panic!("expected the opponent's EffectZoneChoice; got {other:?}"),
    }

    // Reach-guard: the opponent exiles a chosen creature.
    runner
        .act(GameAction::SelectCards {
            cards: vec![warded],
        })
        .expect("opponent selects a creature to exile");
    assert_eq!(
        runner.state().objects[&warded].zone,
        Zone::Exile,
        "the opponent's chosen (warded) creature is exiled without paying Ward"
    );
}

// ---------------------------------------------------------------------------
// 12. LEADERSHIP VACUUM (review follow-up) — the mass ChangeZoneAll sibling still
//     binds ScopedPlayer under the narrowed rewrite (no whole-clause pin).
// ---------------------------------------------------------------------------

/// "Target player returns each commander they control from the battlefield to the
/// command zone." The mass `ChangeZoneAll` moved-object leg must carry a
/// `ScopedPlayer` controller (bound to the target player at resolution), proving
/// the narrowed rewrite covers the mass leg exactly as the whole-clause pin did.
#[test]
fn leadership_vacuum_mass_leg_is_scopedplayer() {
    const LV: &str =
        "Target player returns each commander they control from the battlefield to the command zone.";
    let parsed = parse_oracle_text(LV, "Leadership Vacuum", &[], &[], &[]);
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    assert!(
        !chain_has_unimplemented(root),
        "Leadership Vacuum's mass return parses cleanly; got {root:#?}"
    );
    // Root wraps the lone player target; the mass leg is the sub-ability.
    assert!(
        matches!(&*root.effect, Effect::TargetOnly { target } if target_is_opponent_like(target)
            || matches!(target, TargetFilter::Player)),
        "root targets only the player; got {:?}",
        root.effect
    );
    let mass = find_change_zone_all(root).expect("Leadership Vacuum has a mass ChangeZoneAll leg");
    let Effect::ChangeZoneAll { target, .. } = &*mass.effect else {
        unreachable!("find_change_zone_all guarantees the variant");
    };
    assert!(
        filter_controller_is_scoped(target),
        "the mass 'each commander they control' leg is ScopedPlayer-scoped; got {target:?}"
    );
    assert_eq!(
        mass.target_choice_timing,
        TargetChoiceTiming::Resolution,
        "the mass leg is resolution-bound (scoped_player stamp lands there)"
    );
}

// ---------------------------------------------------------------------------
// 13. ORIGIN-LEAK CLASS (review follow-up) — a compound "exile X and <graveyard
//     conjunct>" whose primary leg is a BATTLEFIELD/self object (Silent
//     Gravestone) must not inherit the trailing conjunct's Graveyard origin.
// ---------------------------------------------------------------------------

/// Silent Gravestone's activated body: "Exile this artifact and all cards from all
/// graveyards." The primary self/battlefield leg must NOT leak `Zone::Graveyard`
/// from the trailing "and all cards from all graveyards" conjunct — its origin
/// stays battlefield/default — while the trailing ChangeZoneAll leg keeps its own
/// `Zone::Graveyard` origin. Reverting `compound_exile_origin_scan` leaks Graveyard
/// onto the primary leg (the Part A defect), a different affected card than SB's
/// "and their graveyard".
#[test]
fn origin_leak_compound_exile_primary_leg_stays_battlefield() {
    const CLAUSE: &str = "Exile this artifact and all cards from all graveyards.";
    let parsed = parse_oracle_text(
        CLAUSE,
        "Silent Gravestone",
        &["Artifact".to_string()],
        &[],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    assert!(
        !chain_has_unimplemented(root),
        "the compound self-exile parses cleanly; got {root:#?}"
    );

    let primary = find_change_zone(root).expect("primary self/battlefield exile leg");
    let Effect::ChangeZone { origin, .. } = &*primary.effect else {
        unreachable!("find_change_zone guarantees the variant");
    };
    assert!(
        origin.is_none() || *origin == Some(Zone::Battlefield),
        "the primary leg's origin must NOT leak Graveyard from the trailing conjunct; got {origin:?}"
    );

    let graveyard =
        find_change_zone_all(root).expect("trailing 'all cards from all graveyards' leg");
    let Effect::ChangeZoneAll { origin, .. } = &*graveyard.effect else {
        unreachable!("find_change_zone_all guarantees the variant");
    };
    assert_eq!(
        *origin,
        Some(Zone::Graveyard),
        "the trailing conjunct keeps its own Graveyard origin; got {origin:?}"
    );
}

// ---------------------------------------------------------------------------
// SHAPE predicates
// ---------------------------------------------------------------------------

fn target_is_opponent_like(target: &TargetFilter) -> bool {
    matches!(target, TargetFilter::Opponent | TargetFilter::Player)
        || matches!(target, TargetFilter::Typed(tf) if tf.controller == Some(ControllerRef::Opponent))
}

fn filter_controller_is_scoped(target: &TargetFilter) -> bool {
    match target {
        TargetFilter::Typed(tf) => tf.controller == Some(ControllerRef::ScopedPlayer),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_controller_is_scoped)
        }
        TargetFilter::Not { filter } => filter_controller_is_scoped(filter),
        _ => false,
    }
}

fn filter_owned_scoped(target: &TargetFilter) -> bool {
    use engine::types::ability::FilterProp;
    match target {
        TargetFilter::Typed(tf) => tf.properties.iter().any(|p| {
            matches!(
                p,
                FilterProp::Owned {
                    controller: ControllerRef::ScopedPlayer
                }
            )
        }),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_owned_scoped)
        }
        TargetFilter::Not { filter } => filter_owned_scoped(filter),
        _ => false,
    }
}
