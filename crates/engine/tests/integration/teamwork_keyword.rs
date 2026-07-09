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
    AbilityCondition, AbilityCost, AbilityDefinition, AdditionalCost, AdditionalCostOrigin,
    Comparator, Effect, SpellCastingOptionKind, TapCreaturesAggregateStat, TapCreaturesRequirement,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
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

// Earth's Mightiest Heroes (Teamwork 5) — NOW FIXED. The teamwork-gated "put any
// number of creature cards from among them onto the battlefield instead" lowers
// to a `SpecialClause::DigInsteadAlt`: the top-level `Effect::Dig` carries
// `keep_count: u32::MAX, up_to: true` gated on `AdditionalCostPaid{Teamwork}`,
// with the base "put a creature card" (`keep_count: 1`) Dig stashed in
// `else_ability`. WITHOUT teamwork the else branch runs (put exactly one); WITH
// teamwork the main branch runs (put any number). The AST shape and the runtime
// keep-count divergence are pinned by `emh_dig_any_number_gated_on_teamwork`,
// `emh_with_teamwork_keeps_any_number`, and `emh_without_teamwork_keeps_one`
// below.

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

// ===========================================================================
// Trailing-rider + Dig-instead teamwork conditional fix (Beast Mode, We Say
// Thee Nay!, Earth's Mightiest Heroes). All three dropped their "if cast using
// teamwork" gate before this fix; the assertions below revert-fail if the
// shared teamwork recognizer is removed from `strip_leading_sequence_connector`
// / `strip_suffix_conditional` / `try_parse_dig_instead_alternative`.
// ===========================================================================

const BEAST_MODE: &str = "Teamwork 1 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 1 or more.)\nTarget creature gets +2/+2 and gains trample until end of turn. Also put a +1/+1 counter on that creature if this spell was cast using teamwork.";

const EARTHS_MIGHTIEST_HEROES: &str = "Teamwork 5 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 5 or more.)\nReveal the top eight cards of your library. You may put a creature card from among them onto the battlefield. If this spell was cast using teamwork, put any number of creature cards from among them onto the battlefield instead. Put the rest into your graveyard.";

const QUANTUM_REDUCTION: &str = "Teamwork 2 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 2 or more.)\nYou may cast this spell as though it had flash if it's cast using teamwork.\nEnchant creature\nEnchanted creature gets -5/-0 and loses all abilities.";

const HELICARRIER_STRIKE: &str = "Teamwork 2 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 2 or more.)\nHelicarrier Strike deals 2 damage to target attacking or blocking creature. If this spell was cast using teamwork, it deals 4 damage to that creature instead.";

const REPULSOR_BLAST: &str = "Teamwork 2 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 2 or more.)\nRepulsor Blast deals 5 damage to target creature. If this spell was cast using teamwork, it also deals 2 damage to that creature's controller.";

const FOLLOW_THE_LUMARETS: &str = "Infusion — Look at the top four cards of your library. You may reveal a creature or land card from among them and put it into your hand. If you gained life this turn, you may instead reveal two creature and/or land cards from among them and put them into your hand. Put the rest on the bottom of your library in a random order.";

/// Flatten a definition forest (`sub_ability` + `else_ability` chains) so tests
/// can assert on any clause regardless of how it was linked.
fn collect_defs<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a AbilityDefinition>) {
    out.push(def);
    if let Some(sub) = def.sub_ability.as_deref() {
        collect_defs(sub, out);
    }
    if let Some(els) = def.else_ability.as_deref() {
        collect_defs(els, out);
    }
}

fn all_defs(parsed: &engine::parser::oracle::ParsedAbilities) -> Vec<&AbilityDefinition> {
    let mut out = Vec::new();
    for def in &parsed.abilities {
        collect_defs(def, &mut out);
    }
    out
}

/// `true` if the condition is the Teamwork additional-cost-paid gate.
fn is_teamwork_gate(cond: &AbilityCondition) -> bool {
    matches!(
        cond,
        AbilityCondition::AdditionalCostPaid {
            origin: Some(AdditionalCostOrigin::Teamwork),
            ..
        }
    )
}

/// Beast Mode's second clause ("Also put a +1/+1 counter on that creature if
/// this spell was cast using teamwork") must parse to a real `PutCounter`
/// effect gated on the Teamwork payment — NOT `Effect::Unimplemented{"also"}`
/// and NOT an always-on counter. Revert the "Also " connector strip → the clause
/// becomes `Unimplemented`; revert the trailing recognizer → `condition == None`.
#[test]
fn beast_mode_teamwork_counter_rider() {
    let parsed = parse_spell("Beast Mode", BEAST_MODE, &["Instant"]);
    assert_no_unimplemented(&parsed, "Beast Mode");

    let counter_def = all_defs(&parsed)
        .into_iter()
        .find(|d| {
            matches!(
                &*d.effect,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    ..
                }
            )
        })
        .expect("Beast Mode must carry a +1/+1 PutCounter clause");

    let cond = counter_def
        .condition
        .as_ref()
        .expect("the +1/+1 counter must be gated (condition != None)");
    assert!(
        is_teamwork_gate(cond),
        "the +1/+1 counter must be gated on AdditionalCostPaid{{Teamwork}}, got {cond:?}"
    );
}

// KNOWN GAP — We Say Thee Nay! ("Counter that spell unless its controller pays
// {4} instead if this spell was cast using teamwork") is NOT fixed by this change.
// Tracing proves its inverted "... instead if teamwork" clause is consumed by the
// instead-clause / counter-unless chain path (try_parse_generic_instead_clause →
// split_inverted_instead_clause → build_instead_def, conditions.rs:2416/2473)
// BEFORE it can reach `strip_suffix_conditional` — so the trailing-rider
// recognizer never sees it (verified: strip_suffix_conditional is never called
// with the teamwork text for this card). The {4} tax sub-ability keeps
// `condition: null` and fires on every cast. The correct fix is inverted-form
// specific (the shared build_instead_def chain also serves the FORWARD teamwork
// "instead" cards — Cruel Alliance/Too Evil — which rely on it returning None so
// the leading peel folds them to `AdditionalCostPaidInstead`; adding teamwork to
// that shared chain would regress them). This is outside the reviewed plan's
// stated root cause (the plan asserted strip_suffix_conditional would peel it) and
// is returned to the orchestrator as a stop-and-return item. `shipped_teamwork_
// cards_parse_without_unimplemented` still covers WSTN parsing cleanly.

/// EMH lowers to a teamwork-gated `DigInsteadAlt`: the top-level Dig keeps any
/// number (`keep_count == u32::MAX`, `up_to`), gated on Teamwork, with the base
/// "put one" Dig (`keep_count == 1`) in `else_ability`. Revert the new
/// `.or_else` arm → a single unconditional `Dig{keep_count:u32::MAX}` (the live
/// free-upgrade bug) with no condition and no else branch.
#[test]
fn emh_dig_any_number_gated_on_teamwork() {
    let parsed = parse_spell(
        "Earth's Mightiest Heroes",
        EARTHS_MIGHTIEST_HEROES,
        &["Sorcery"],
    );
    assert_no_unimplemented(&parsed, "Earth's Mightiest Heroes");

    let top = parsed
        .abilities
        .iter()
        .find(|d| matches!(&*d.effect, Effect::Dig { .. }))
        .expect("EMH must have a top-level Dig");

    match &*top.effect {
        Effect::Dig {
            keep_count, up_to, ..
        } => {
            assert_eq!(
                *keep_count,
                Some(u32::MAX),
                "teamwork branch must keep any number (u32::MAX sentinel)"
            );
            assert!(*up_to, "any-number keep is an 'up to' selection");
        }
        other => panic!("expected Dig, got {other:?}"),
    }

    let cond = top
        .condition
        .as_ref()
        .expect("top-level Dig must be gated on Teamwork");
    assert!(
        is_teamwork_gate(cond),
        "EMH top Dig must gate on AdditionalCostPaid{{Teamwork}}, got {cond:?}"
    );

    let els = top
        .else_ability
        .as_deref()
        .expect("EMH must stash the base 'put one' Dig in else_ability");
    match &*els.effect {
        Effect::Dig { keep_count, .. } => assert_eq!(
            *keep_count,
            Some(1),
            "the no-teamwork branch must put exactly one creature"
        ),
        other => panic!("expected base Dig in else_ability, got {other:?}"),
    }
}

/// Trailing teamwork riders parse for all three subject/tense variants of the
/// shared phrase combinator ("this spell was" / "it was" / "it's"). Revert the
/// shared combinator and any of these loses its gate.
#[test]
fn trailing_teamwork_rider_subject_tense_variants() {
    for text in [
        "Draw a card if this spell was cast using teamwork.",
        "Draw a card if it was cast using teamwork.",
        "Draw a card if it's cast using teamwork.",
    ] {
        let parsed = parse_spell("Probe", text, &["Instant"]);
        assert_no_unimplemented(&parsed, text);
        let gated = all_defs(&parsed)
            .into_iter()
            .any(|d| d.condition.as_ref().is_some_and(is_teamwork_gate));
        assert!(gated, "{text}: trailing rider must produce a Teamwork gate");
    }
}

/// Quantum Reduction's conditional flash permission ("as though it had flash if
/// it's cast using teamwork") is NOT representable yet (deferred honest). The
/// parser must NOT leak an unconditional flash grant: no `AsThoughHadFlash`
/// casting option with `condition: None`. Coverage stays RED for the flash
/// clause; the rest of the Aura body parses normally.
#[test]
fn quantum_reduction_flash_not_unconditional() {
    let parsed = parse_spell(
        "Quantum Reduction",
        QUANTUM_REDUCTION,
        &["Enchantment", "Aura"],
    );

    let unconditional_flash = parsed
        .casting_options
        .iter()
        .any(|opt| opt.kind == SpellCastingOptionKind::AsThoughHadFlash && opt.condition.is_none());
    assert!(
        !unconditional_flash,
        "Quantum Reduction must NOT emit an unconditional flash grant; got {:?}",
        parsed.casting_options
    );
}

// ---------------------------------------------------------------------------
// Blast-radius regression — cards that flow near the changed seams must parse
// IDENTICALLY. These revert-pass (they assert the absence of a new teamwork gate
// where one should not appear, and the presence of the pre-existing gate).
// ---------------------------------------------------------------------------

/// Cruel Alliance's leading "instead" form stays an `AdditionalCostPaidInstead`
/// sub-ability — the trailing/Dig fix must not re-route it.
#[test]
fn cruel_alliance_unchanged() {
    let parsed = parse_spell("Cruel Alliance", CRUEL_ALLIANCE, &["Sorcery"]);
    assert_no_unimplemented(&parsed, "Cruel Alliance");
    let has_instead = all_defs(&parsed).into_iter().any(|d| {
        matches!(
            d.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        )
    });
    assert!(
        has_instead,
        "Cruel Alliance must keep its AdditionalCostPaidInstead sub-ability"
    );
}

/// Helicarrier Strike's leading "If teamwork, ... instead" peels at the leading
/// site (which already recognized teamwork) — its `AdditionalCostPaid{Teamwork}`
/// sub-ability is intact and the trailing rider must not double-handle it.
#[test]
fn helicarrier_strike_unchanged() {
    let parsed = parse_spell("Helicarrier Strike", HELICARRIER_STRIKE, &["Instant"]);
    assert_no_unimplemented(&parsed, "Helicarrier Strike");
    let gated = all_defs(&parsed)
        .into_iter()
        .filter(|d| matches!(&*d.effect, Effect::DealDamage { .. }))
        .filter(|d| d.condition.as_ref().is_some_and(is_teamwork_gate))
        .count();
    assert_eq!(
        gated, 1,
        "Helicarrier keeps exactly one Teamwork-gated DealDamage sub-ability"
    );
}

/// Repulsor Blast's mid-sentence "it also deals" must NOT be eaten by the
/// position-0 "Also " connector strip; the card still parses cleanly with its
/// trailing teamwork gate on the bonus damage.
#[test]
fn repulsor_blast_mid_sentence_also_unaffected() {
    let parsed = parse_spell("Repulsor Blast", REPULSOR_BLAST, &["Sorcery"]);
    assert_no_unimplemented(&parsed, "Repulsor Blast");
    let gated = all_defs(&parsed)
        .into_iter()
        .any(|d| d.condition.as_ref().is_some_and(is_teamwork_gate));
    assert!(
        gated,
        "Repulsor Blast's bonus damage must remain gated on Teamwork"
    );
}

/// Follow the Lumarets' `DigInsteadAlt` ("if you gained life this turn") matches
/// an earlier condition arm — the appended teamwork arm must never fire for it.
#[test]
fn follow_the_lumarets_dig_instead_unchanged() {
    let parsed = parse_spell("Follow the Lumarets", FOLLOW_THE_LUMARETS, &["Sorcery"]);
    assert_no_unimplemented(&parsed, "Follow the Lumarets");
    let top = parsed
        .abilities
        .iter()
        .find(|d| matches!(&*d.effect, Effect::Dig { .. }))
        .expect("Follow the Lumarets has a Dig");
    let cond = top
        .condition
        .as_ref()
        .expect("Lumarets' alt Dig is conditional");
    assert!(
        !is_teamwork_gate(cond),
        "Lumarets must NOT acquire a Teamwork gate; got {cond:?}"
    );
}

// ---------------------------------------------------------------------------
// Runtime discrimination — Beast Mode (+1/+1 counter gated on teamwork).
// ---------------------------------------------------------------------------

/// Build P0 with Beast Mode (cost {0}) in hand + a 3/3 (power 3 >= Teamwork 1)
/// that is both the spell target and the teamwork tapper.
fn setup_beast_mode() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P0, "Bear", 3, 3).id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Beast Mode", true, BEAST_MODE);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();
    (scenario.build(), spell, target)
}

fn beast_mode_counters(runner: &GameRunner, target: ObjectId) -> u32 {
    runner.state().objects[&target]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn beast_mode_without_teamwork_no_counter() {
    let (mut runner, spell, target) = setup_beast_mode();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Beast Mode must be accepted");
    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("declining teamwork must be accepted");
    drive_single_target(&mut runner, target);
    resolve_stack(&mut runner);
    assert_eq!(
        beast_mode_counters(&runner, target),
        0,
        "WITHOUT teamwork, no +1/+1 counter is placed"
    );
}

#[test]
fn beast_mode_with_teamwork_adds_counter() {
    let (mut runner, spell, target) = setup_beast_mode();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Beast Mode must be accepted");
    drive_cast_paying_teamwork(&mut runner, &[target], target);
    resolve_stack(&mut runner);
    assert_eq!(
        beast_mode_counters(&runner, target),
        1,
        "WITH teamwork, exactly one +1/+1 counter is placed"
    );
}

// ---------------------------------------------------------------------------
// Runtime discrimination — Earth's Mightiest Heroes (keep-count cap gated on
// teamwork). With teamwork the DigChoice caps at the number of revealed creature
// cards (any number); without, it caps at one.
// ---------------------------------------------------------------------------

/// Build P0 with EMH (cost {0}) in hand, a power-5 tapper on the battlefield
/// (Teamwork 5), and eight creature cards on top of the library.
fn setup_emh() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tapper = scenario.add_creature(P0, "Hulk", 5, 5).id();
    let lib_ids: Vec<ObjectId> = (0..8)
        .map(|i| scenario.add_card_to_library_top(P0, &format!("Recruit {i}")))
        .collect();
    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Earth's Mightiest Heroes",
        false,
        EARTHS_MIGHTIEST_HEROES,
    );
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();
    let mut runner = scenario.build();
    // Make the eight library cards real creature cards so the Dig's Creature
    // filter selects them.
    for id in lib_ids {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
    }
    (runner, spell, tapper)
}

/// Drive EMH up to its `DigChoice` and return the surfaced keep-count cap.
fn drive_emh_keep_count(runner: &mut GameRunner, pay_teamwork: bool, tapper: ObjectId) -> usize {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: pay_teamwork })
                    .expect("deciding teamwork must be accepted");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![tapper],
                    })
                    .expect("tapping the power-5 creature must pay teamwork");
            }
            WaitingFor::DigChoice { keep_count, .. } => return keep_count,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected window before DigChoice: {other:?}"),
        }
    }
    panic!("EMH never surfaced a DigChoice");
}

#[test]
fn emh_with_teamwork_keeps_any_number() {
    let (mut runner, spell, tapper) = setup_emh();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting EMH must be accepted");
    let keep = drive_emh_keep_count(&mut runner, true, tapper);
    assert_eq!(
        keep, 8,
        "WITH teamwork, the player may put any number (all 8 revealed creatures)"
    );
}

#[test]
fn emh_without_teamwork_keeps_one() {
    let (mut runner, spell, tapper) = setup_emh();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting EMH must be accepted");
    let keep = drive_emh_keep_count(&mut runner, false, tapper);
    assert_eq!(
        keep, 1,
        "WITHOUT teamwork, the base branch puts exactly one creature"
    );
}
