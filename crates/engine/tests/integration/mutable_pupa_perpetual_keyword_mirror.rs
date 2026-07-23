//! Issue #6321 — Mutable Pupa's perpetual keyword-mirror ETB trigger.
//!
//! "Whenever another creature you control enters, this creature perpetually
//! gains flying if that creature has flying. The same is true for first strike,
//! double strike, deathtouch, haste, hexproof, indestructible, lifelink, menace,
//! reach, trample, and vigilance."
//!
//! Digital-only Alchemy (no CR entry for "perpetually"); CR 702.1c + CR 608.2c govern the
//! per-branch resolution order that the `SiblingCondition::ReplicatedOrBranch`
//! marker restores. Each of the 12 keyword nodes is an INDEPENDENT OR-branch
//! gated on the entering object having THAT keyword — so the grant list must not
//! collapse to keyword[0]'s gate, and a keyword deep in the list (vigilance,
//! node 11) must still resolve after the earlier gates (flying, etc.) are false.
//!
//! These drive the REAL cast pipeline (`GameRunner::cast(..).resolve()`); the
//! Mutable Pupa trigger grants to its SOURCE (no target selection), so the
//! entering creature is cast and the trigger auto-resolves. Oracle text is
//! verbatim from data/card-data.json.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MUTABLE_PUPA: &str = "Whenever another creature you control enters, this creature perpetually gains flying if that creature has flying. The same is true for first strike, double strike, deathtouch, haste, hexproof, indestructible, lifelink, menace, reach, trample, and vigilance.";

/// Every keyword the mirror can grant EXCEPT the ones a given entering creature
/// carries — used for the "no collapsed/leaked grant" negative sweep.
const ALL_MIRROR_KEYWORDS: &[Keyword] = &[
    Keyword::Flying,
    Keyword::FirstStrike,
    Keyword::DoubleStrike,
    Keyword::Deathtouch,
    Keyword::Haste,
    Keyword::Hexproof,
    Keyword::Indestructible,
    Keyword::Lifelink,
    Keyword::Menace,
    Keyword::Reach,
    Keyword::Trample,
    Keyword::Vigilance,
];

/// Fund a pool with `n` white mana (white pays generic too), so a small creature
/// cast auto-pays without surfacing a mana window. The exact cost is not the
/// subject under test.
fn white_pool(n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::White, ObjectId(9_999), false, vec![]); n]
}

// Affa Protector ({2}{W}, Human Soldier Ally, 1/4) has exactly one of the listed
// keywords — Vigilance. It enters under Mutable Pupa's controller: the mirror
// must grant Vigilance (node 11, reached ONLY because the resolve_chain_body
// ReplicatedOrBranch disjunct carries the chain past the false flying/first
// strike/... gates) and grant NOTHING else.
#[test]
fn mutable_pupa_gains_only_the_entering_creatures_vigilance() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pupa = scenario
        .add_creature_from_oracle(P0, "Mutable Pupa", 1, 1, MUTABLE_PUPA)
        .id();
    // Affa Protector — keyword built via the builder (not inline reminder text),
    // per the card-test foot-gun on inline keyword lines.
    let affa = scenario
        .add_creature_to_hand(P0, "Affa Protector", 1, 4)
        .vigilance()
        .id();
    scenario.with_mana_pool(P0, white_pool(3));
    let mut runner = scenario.build();

    // Baseline reach-guard (positive, not vacuous): Mutable Pupa starts with
    // neither Vigilance nor Flying.
    assert!(
        !runner.state().objects[&pupa].has_keyword(&Keyword::Vigilance),
        "baseline: Mutable Pupa has no vigilance",
    );
    assert!(
        !runner.state().objects[&pupa].has_keyword(&Keyword::Flying),
        "baseline: Mutable Pupa has no flying",
    );

    let outcome = runner.cast(affa).resolve();
    // Reach-guard: Affa Protector actually entered (so the trigger really fired).
    outcome.assert_zone(&[affa], Zone::Battlefield);

    // Re-run layers so the perpetual base_keywords grant is reflected live.
    let mut state = outcome.state().clone();
    state.layers_dirty.mark_full();
    evaluate_layers(&mut state);
    let pupa_obj = &state.objects[&pupa];

    // Vigilance IS granted — FALSE without the resolve_chain_body fix (the chain
    // would collapse at flying's false gate and never reach node 11).
    assert!(
        pupa_obj.has_keyword(&Keyword::Vigilance),
        "Affa Protector has vigilance ⇒ Mutable Pupa perpetually gains vigilance",
    );
    // Nothing else the entering creature lacks is granted (no collapse/leak).
    for kw in ALL_MIRROR_KEYWORDS {
        if *kw == Keyword::Vigilance {
            continue;
        }
        assert!(
            !pupa_obj.has_keyword(kw),
            "Mutable Pupa must not gain {kw:?} (Affa Protector lacks it)",
        );
    }
}

// Accumulation: an entering creature carrying TWO listed keywords (vigilance AND
// trample) makes the mirror grant BOTH — independent `ApplyPerpetual`
// resolutions accumulate (GrantKeywords pushes to base_keywords, never
// overwrites), and neither is at keyword[0]'s position.
#[test]
fn mutable_pupa_accumulates_every_matching_keyword() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pupa = scenario
        .add_creature_from_oracle(P0, "Mutable Pupa", 1, 1, MUTABLE_PUPA)
        .id();
    let twin = scenario
        .add_creature_to_hand(P0, "Twin Keyworder", 3, 3)
        .vigilance()
        .trample()
        .id();
    scenario.with_mana_pool(P0, white_pool(4));
    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&pupa].has_keyword(&Keyword::Vigilance)
            && !runner.state().objects[&pupa].has_keyword(&Keyword::Trample),
        "baseline: Mutable Pupa has neither vigilance nor trample",
    );

    let outcome = runner.cast(twin).resolve();
    outcome.assert_zone(&[twin], Zone::Battlefield);

    let mut state = outcome.state().clone();
    state.layers_dirty.mark_full();
    evaluate_layers(&mut state);
    let pupa_obj = &state.objects[&pupa];

    // BOTH matching keywords accumulate (trample is node 10, vigilance node 11 —
    // both past the earlier false gates, and both independently granted).
    assert!(
        pupa_obj.has_keyword(&Keyword::Vigilance),
        "Mutable Pupa gains vigilance",
    );
    assert!(
        pupa_obj.has_keyword(&Keyword::Trample),
        "Mutable Pupa gains trample (accumulates alongside vigilance, not overwritten)",
    );
    for kw in ALL_MIRROR_KEYWORDS {
        if matches!(kw, Keyword::Vigilance | Keyword::Trample) {
            continue;
        }
        assert!(
            !pupa_obj.has_keyword(kw),
            "Mutable Pupa must not gain {kw:?} (Twin Keyworder lacks it)",
        );
    }
}

// -----------------------------------------------------------------------
// Kathril, Aspect Warper — the SAME list-collapse bug in the counters class
// (`ReplicateKind::CounterPlacement` via `attach_repeat_process_keywords`),
// fixed by the same `SiblingCondition::ReplicatedOrBranch` marker + the shared
// `resolve_chain_body` disjunct. CR 608.2c. Oracle text verbatim from
// data/card-data.json. Kathril's counter recipient parses to `TargetFilter::Any`
// (a known TargetFallback), so the ETB opens a trigger target-selection prompt;
// the cast driver carries the declared Kathril choice forward to that prompt.
// -----------------------------------------------------------------------

const KATHRIL: &str = "When Kathril enters, put a flying counter on any creature you control if a creature card in your graveyard has flying. Repeat this process for first strike, double strike, deathtouch, hexproof, indestructible, lifelink, menace, reach, trample, and vigilance. Then put a +1/+1 counter on Kathril for each counter put on a creature this way.";

// Only trample is in the graveyard — flying (K0) and every gate before trample
// are FALSE. The trample counter must still be placed (the chain reaches node 9
// past the false earlier gates), the flying counter must NOT, and the
// unconditional +1/+1 tail must land (the chain reaches the tail past the last
// false gate). Reverting the `attach_repeat_process_keywords` marker OR the
// `resolve_chain_body` disjunct collapses the chain at flying's false gate, and
// both the trample and the +1/+1 assertions flip.
#[test]
fn kathril_reaches_matching_counter_and_tail_past_false_earlier_gates() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A creature card with ONLY trample (explicitly NOT flying) in P0's graveyard.
    scenario
        .add_creature_to_graveyard(P0, "Trampling Remains", 2, 2)
        .trample();
    let kathril = scenario
        .add_creature_to_hand_from_oracle(P0, "Kathril, Aspect Warper", 3, 3, KATHRIL)
        .id();
    scenario.with_mana_pool(P0, white_pool(6));
    let mut runner = scenario.build();

    let outcome = runner.cast(kathril).target_object(kathril).resolve();
    outcome.assert_zone(&[kathril], Zone::Battlefield);

    let kathril_obj = &outcome.state().objects[&kathril];
    let count = |ct: CounterType| kathril_obj.counters.get(&ct).copied().unwrap_or(0);

    // The trample gate is true → a trample counter is placed (chain reached it).
    assert_eq!(
        count(CounterType::Keyword(Keyword::Trample.kind())),
        1,
        "trample is in the graveyard ⇒ exactly one trample counter is placed",
    );
    // The flying gate is false → NO flying counter (per-item independence, not a
    // shared/collapsed gate).
    assert_eq!(
        count(CounterType::Keyword(Keyword::Flying.kind())),
        0,
        "no flying card in the graveyard ⇒ no flying counter",
    );
    // The unconditional tail fires: a +1/+1 counter on Kathril (proves the chain
    // reached the end past vigilance's false gate).
    assert_eq!(
        count(CounterType::Plus1Plus1),
        1,
        "exactly one +1/+1 counter is placed for the one counter put on a creature this way",
    );
}
