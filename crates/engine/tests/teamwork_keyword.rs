//! Teamwork N — optional additional cast cost "tap any number of creatures you
//! control with total power N or more" (CR 601.2b/f; tap-any-number-total-power
//! mirrors Crew CR 702.122a / Saddle CR 702.171a). The spell's body references
//! whether the cost was paid via "if this spell was cast using teamwork", which
//! parses to the same `additional_cost_paid` gate as kicker/bargain.
//!
//! Two layers are verified:
//!   1. Parse coverage — each shipped MSH Teamwork card's full oracle text parses
//!      with zero `Effect::Unimplemented` (the keyword line + every body clause).
//!   2. Runtime discrimination — casting WITHOUT paying teamwork yields the base
//!      effect; casting WITH teamwork (tapping creatures with total power >= N)
//!      yields the upgraded/both effect. The divergence fails on revert.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AdditionalCost, Comparator, Effect, TapCreaturesAggregateStat,
    TapCreaturesRequirement,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

// ---------------------------------------------------------------------------
// Oracle text constants (verbatim from /tmp/msh-effort/cards.md)
// ---------------------------------------------------------------------------

const TEAM_TACTICS: &str = "Teamwork 1 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 1 or more.)\nTarget creature gains double strike until end of turn. If this spell was cast using teamwork, that creature also gains trample until end of turn.";

const WE_SAY_THEE_NAY: &str = "Teamwork 2 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 2 or more.)\nCounter target spell unless its controller pays {2}. Counter that spell unless its controller pays {4} instead if this spell was cast using teamwork.";

const CRUEL_ALLIANCE: &str = "Teamwork 2 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 2 or more.)\nExile target creature with mana value 3 or less. If this spell was cast using teamwork, instead exile target creature and you gain 3 life.";

const WIDOWS_BITE: &str = "Teamwork 3 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 3 or more.)\nChoose one. If this spell was cast using teamwork, choose both instead.\n• Target creature gains deathtouch until end of turn.\n• Target creature gets -2/-2 until end of turn.";

const GO_NUTS: &str = "Teamwork 3 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 3 or more.)\nChoose one. If this spell was cast using teamwork, choose both instead.\n• Put a +1/+1 counter on target creature.\n• Target creature you control fights target creature an opponent controls.";

const HULK_SMASH: &str = "Teamwork 4 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 4 or more.)\nChoose one. If this spell was cast using teamwork, choose both instead.\n• Destroy target noncreature artifact.\n• Target creature you control deals damage equal to its power to target creature an opponent controls.";

const MURDOCKS_CRUSADE: &str = "Teamwork 4 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 4 or more.)\nChoose one. If this spell was cast using teamwork, choose both instead.\n• Street Justice — Exile target creature with toughness 4 or greater.\n• Legal Justice — Exile target enchantment with mana value 4 or greater.";

const TOO_EVIL_TO_STAY_DEAD: &str = "Teamwork 4 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 4 or more.)\nChoose target creature card in your graveyard with mana value 4 or less. If this spell was cast using teamwork, instead choose target creature card in your graveyard. Return the chosen card to the battlefield.";

const ATLANTIS_ATTACKS: &str = "Teamwork 4 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 4 or more.)\nChoose one. If this spell was cast using teamwork, choose both instead.\n• Target player creates a 6/5 blue Leviathan creature token with hexproof.\n• Return one or two target nonland permanents to their owners' hands.";

// ---------------------------------------------------------------------------
// Parse helpers
// ---------------------------------------------------------------------------

fn parse_spell(
    name: &str,
    oracle: &str,
    types: &[&str],
) -> engine::parser::oracle::ParsedAbilities {
    let type_strings: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle, name, &[], &type_strings, &[])
}

/// Assert that no parsed ability/trigger carries an `Unimplemented` effect at
/// the top level or in a linked sub-ability chain (`Effect::Unimplemented` is
/// the authoritative "parser couldn't handle this" marker; effect chains are
/// modeled via `sub_ability` links, not a container variant).
fn assert_no_unimplemented(parsed: &engine::parser::oracle::ParsedAbilities, name: &str) {
    fn check_def(def: &engine::types::ability::AbilityDefinition, name: &str) {
        assert!(
            !matches!(*def.effect, Effect::Unimplemented { .. }),
            "{name}: Unimplemented effect: {:?}",
            def.effect
        );
        if let Some(sub) = def.sub_ability.as_deref() {
            check_def(sub, name);
        }
    }
    for def in &parsed.abilities {
        check_def(def, name);
    }
    for trig in &parsed.triggers {
        if let Some(def) = trig.execute.as_deref() {
            check_def(def, name);
        }
    }
    // Modal modes are flattened into `parsed.abilities` (one ability per bullet),
    // so the loop above already covers them.
}

/// Every shipped Teamwork card parses with zero Unimplemented effects. Reverting
/// the keyword/condition/synthesis wiring re-introduces Unimplemented on these.
#[test]
fn shipped_teamwork_cards_parse_without_unimplemented() {
    let cards: &[(&str, &str, &[&str])] = &[
        ("Team Tactics", TEAM_TACTICS, &["Instant"]),
        ("We Say Thee Nay!", WE_SAY_THEE_NAY, &["Instant"]),
        ("Cruel Alliance", CRUEL_ALLIANCE, &["Sorcery"]),
        ("Widow's Bite", WIDOWS_BITE, &["Instant"]),
        ("Go Nuts!", GO_NUTS, &["Sorcery"]),
        ("HULK SMASH!", HULK_SMASH, &["Sorcery"]),
        ("Murdock's Crusade", MURDOCKS_CRUSADE, &["Sorcery"]),
        ("Too Evil to Stay Dead", TOO_EVIL_TO_STAY_DEAD, &["Sorcery"]),
        ("Atlantis Attacks", ATLANTIS_ATTACKS, &["Sorcery"]),
    ];
    for (name, oracle, types) in cards {
        let parsed = parse_spell(name, oracle, types);
        assert_no_unimplemented(&parsed, name);
    }
}

/// The Teamwork keyword line parses to `Keyword::Teamwork(N)` and synthesis (run
/// inside `build_face_from_oracle`/`synthesize_all`) turns it into the optional
/// aggregate-power tap cost. Here we verify the keyword extraction + that the
/// synthesized additional cost is the aggregate form (not a fixed count).
#[test]
fn teamwork_keyword_extracts_and_synthesizes_aggregate_tap_cost() {
    use engine::database::synthesis::synthesize_teamwork;
    use engine::types::card::CardFace;

    let mut face = CardFace {
        keywords: vec![Keyword::Teamwork(3)],
        ..CardFace::default()
    };
    synthesize_teamwork(&mut face);
    match face.additional_cost.as_ref().expect("additional_cost set") {
        AdditionalCost::Optional {
            cost: AbilityCost::TapCreatures { requirement, .. },
            ..
        } => match requirement {
            TapCreaturesRequirement::Aggregate {
                stat: TapCreaturesAggregateStat::TotalPower,
                value,
                ..
            } => assert_eq!(*value, 3, "Teamwork 3 requires total power 3"),
            other => panic!("expected aggregate total-power tap requirement, got {other:?}"),
        },
        other => panic!("expected optional TapCreatures additional cost, got {other:?}"),
    }
}

// DEFERRED: Earth's Mightiest Heroes (Teamwork 5). The body "reveal top eight,
// you may put A creature card onto the battlefield; if cast using teamwork, put
// ANY NUMBER of creature cards instead; rest to graveyard" parses to a single
// `Effect::Dig { keep_count: u32::MAX, up_to: true, .. }` — it collapses the
// base (one) and upgraded (any number) keep counts into "any number"
// unconditionally, with no `AdditionalCostPaid`-gated count switch. Shipping it
// would silently let the no-teamwork case put any number of creatures, which is
// wrong. Deferred until the reveal-N effect supports a teamwork-conditional
// keep_count; the Teamwork keyword itself is shipped and the other nine cards
// reuse existing effects cleanly.

// ---------------------------------------------------------------------------
// Runtime discrimination — Team Tactics (Teamwork 1)
//
// Body: "Target creature gains double strike until end of turn. If this spell
// was cast using teamwork, that creature also gains trample until end of turn."
//
// The base ability grants DoubleStrike unconditionally; the linked sub-ability
// grants Trample gated on `AbilityCondition::AdditionalCostPaid`. The two casts
// below diverge ONLY in whether the optional Teamwork cost is paid:
//   - declined  -> DoubleStrike, NOT Trample
//   - paid      -> DoubleStrike AND Trample
// Reverting any of {keyword parse, synthesis, "cast using teamwork" condition,
// additional_cost_paid wiring} collapses this divergence and fails these.
// ---------------------------------------------------------------------------

fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

/// Build a scenario where P0 has Team Tactics (cost {0}) in hand and one 3/3
/// creature as both the spell target and an eligible teamwork tapper (power 3
/// >= Teamwork 1). Returns (runner, spell_id, target_id).
fn setup_team_tactics() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Target creature; also the eligible teamwork tap creature (power 3 >= 1).
    let target = scenario.add_creature(P0, "Bear", 3, 3).id();
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Team Tactics", true, TEAM_TACTICS);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();
    let runner = scenario.build();
    (runner, spell, target)
}

#[test]
fn team_tactics_without_teamwork_grants_double_strike_only() {
    let (mut runner, spell, target) = setup_team_tactics();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Team Tactics must be accepted");

    // The optional Teamwork cost is offered; decline it.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "Teamwork must surface an optional additional cost, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("declining teamwork must be accepted");

    // Choose the spell target.
    drive_single_target(&mut runner, target);
    resolve_stack(&mut runner);

    assert!(
        has_kw(&mut runner, target, &Keyword::DoubleStrike),
        "target must gain double strike"
    );
    assert!(
        !has_kw(&mut runner, target, &Keyword::Trample),
        "WITHOUT teamwork, target must NOT gain trample"
    );
}

#[test]
fn team_tactics_with_teamwork_grants_double_strike_and_trample() {
    let (mut runner, spell, target) = setup_team_tactics();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Team Tactics must be accepted");

    // Pay the optional Teamwork cost.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "Teamwork must surface an optional additional cost, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("paying teamwork must be accepted");

    // Tap the 3/3 (total power 3 >= Teamwork 1) to pay the aggregate cost.
    match runner.state().waiting_for.clone() {
        WaitingFor::PayCost {
            kind: PayCostKind::TapCreatures { aggregate },
            choices,
            ..
        } => {
            let aggregate = aggregate.expect("Teamwork surfaces an aggregate constraint");
            assert_eq!(
                aggregate.value, 1,
                "Teamwork 1 must surface an aggregate power threshold of 1"
            );
            assert_eq!(
                aggregate.comparator,
                Comparator::GE,
                "Teamwork's aggregate constraint is total power >= N"
            );
            assert!(
                choices.contains(&target),
                "the 3/3 must be an eligible teamwork tap creature"
            );
        }
        other => panic!("expected PayCost TapCreatures after paying teamwork, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![target],
        })
        .expect("tapping the 3/3 (total power 3 >= 1) must pay teamwork");
    assert!(
        runner.state().objects[&target].tapped,
        "the teamwork tap creature must be tapped"
    );

    drive_single_target(&mut runner, target);
    resolve_stack(&mut runner);

    assert!(
        has_kw(&mut runner, target, &Keyword::DoubleStrike),
        "target must gain double strike"
    );
    assert!(
        has_kw(&mut runner, target, &Keyword::Trample),
        "WITH teamwork, target must ALSO gain trample"
    );
}

// ---------------------------------------------------------------------------
// Runtime discrimination — Cruel Alliance (Teamwork 2)
//
// Body: "Exile target creature with mana value 3 or less. If this spell was
// cast using teamwork, instead exile target creature and you gain 3 life."
//
// The teamwork-paid path uses the "instead" upgrade. The observable divergence
// is the +3 life gain that only occurs on the teamwork path.
// ---------------------------------------------------------------------------

#[test]
fn cruel_alliance_with_teamwork_gains_three_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0's tap creature (power 2 >= Teamwork 2).
    let tapper = scenario.add_creature(P0, "Tapper", 2, 2).id();
    // An opponent creature to exile (any MV — the teamwork "instead" form has no
    // MV restriction).
    let victim = scenario.add_creature(PlayerId(1), "Big Threat", 6, 6).id();
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Cruel Alliance", false, CRUEL_ALLIANCE);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let life_before = runner.state().players[0].life;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Cruel Alliance must be accepted");

    // Drive the cast: the engine may surface target selection, the optional
    // teamwork cost, and the tap-creatures payment in any order. Pay teamwork
    // (tapping the 2/2, total power 2 >= 2) and target the victim.
    drive_cast_paying_teamwork(&mut runner, &[tapper], victim);
    resolve_stack(&mut runner);

    assert!(
        runner.state().objects[&tapper].tapped,
        "the teamwork tap creature must be tapped"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before + 3,
        "the teamwork 'instead' path must gain 3 life"
    );
    assert!(
        !runner.state().battlefield.contains(&victim),
        "the teamwork 'instead' path exiles the targeted creature"
    );
}

/// Drive a cast that PAYS the optional teamwork cost: at each window, accept the
/// optional cost, tap `tappers` for the aggregate power cost, and choose
/// `target` at any target-selection window. Order-agnostic so it tolerates the
/// engine surfacing target-before-cost or cost-before-target.
fn drive_cast_paying_teamwork(runner: &mut GameRunner, tappers: &[ObjectId], target: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("paying teamwork must be accepted");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: tappers.to_vec(),
                    })
                    .expect("tapping creatures for teamwork must be accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(target)),
                    })
                    .expect("choosing the target must be accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing {0} cost must be accepted");
            }
            _ => return,
        }
    }
}

/// Drive a single `TargetSelection` window choosing `target`.
fn drive_single_target(runner: &mut GameRunner, target: ObjectId) {
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(target)),
                    })
                    .expect("choosing the target must be accepted");
                return;
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing {0} cost must be accepted");
            }
            _ => return,
        }
    }
}

/// Resolve the stack to empty by passing priority.
fn resolve_stack(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().stack.is_empty()
            && !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}
