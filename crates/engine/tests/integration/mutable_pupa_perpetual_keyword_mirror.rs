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
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
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
// data/card-data.json. The counter recipient, "any creature you control", now
// parses to a real `TargetFilter::Typed{Creature, controller: You}` (see the
// `parse_type_phrase_with_ctx` "any " quantifier fix) instead of falling back
// to the degenerate `TargetFilter::Any`.
//
// CR 608.2d: an untargeted "any creature you control" choice (no literal
// "target") is made independently at EACH instruction's own resolution, not
// once when the whole ability goes on the stack. `target_choice_timing_for_
// clause` (`oracle_effect/lower.rs`) marks every untargeted, non-context-ref
// `PutCounter` recipient `Resolution`-timed (widened from the narrower
// Equipped/Enchanted-only case, matching `MultiplyCounter`'s existing
// pattern); `resolve_chain_body` skips copying a parent's already-chosen
// target into a `Resolution`-timed sub; and each such instruction that
// reaches resolution with an empty, multi-candidate recipient opens an
// interactive `WaitingFor::ChooseFromZoneChoice` prompt (a single legal
// candidate auto-binds with no prompt; zero is a silent no-op — CR 608.2d:
// "The player can't choose an option that's illegal or impossible") —
// reusing the SAME parked-continuation machinery already proven
// for `PutCounter` (the Bolster keyword action). The two tests below cover
// both the single-creature case (no observable choice, matches the original
// #6321 regression exactly) and the multi-creature case (proves the choice
// is genuinely independent per instruction, not one shared pick).
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
    // Kathril costs {2}{W}{B}{G}; use exact colored and generic mana so the
    // cast reaches the ETB trigger under test.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(9_996), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(9_997), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_998), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(10_000), false, vec![]),
        ],
    );
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

// Discriminating case for CR 608.2d: the graveyard has BOTH flying and
// trample, so TWO independent PutCounter instructions fire (flying is node 0,
// the head; trample is node 9, reached only via the SAME per-item independent
// gate the test above proves). P0 controls TWO creatures — Kathril plus
// "Second Recipient", already on the battlefield — so each instruction's "any
// creature you control" choice has a genuine, non-trivial answer. Declaring
// [second_recipient, kathril] in that order pins the flying prompt (which
// fires first, since flying resolves before trample in Oracle-text order) to
// second_recipient and leaves kathril for the trample prompt. If the two
// instructions wrongly shared one upfront choice (the bug this test guards
// against), both counters would land on whichever object was bound first and
// second_recipient would have NEITHER counter.
#[test]
fn kathril_offers_each_matching_counter_its_own_independent_recipient() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A creature card with BOTH flying and trample in P0's graveyard, so both
    // the head (flying) and the trample sibling fire their own instruction.
    scenario
        .add_creature_to_graveyard(P0, "Skybound Charger", 3, 3)
        .flying()
        .trample();
    // Already on the battlefield when Kathril enters — the second legal
    // recipient for each "any creature you control" choice.
    let second_recipient = scenario.add_creature(P0, "Second Recipient", 1, 1).id();
    let kathril = scenario
        .add_creature_to_hand_from_oracle(P0, "Kathril, Aspect Warper", 3, 3, KATHRIL)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(9_996), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(9_997), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_998), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(10_000), false, vec![]),
        ],
    );
    let mut runner = scenario.build();

    // `.resolve()` stops at the first `WaitingFor::ChooseFromZoneChoice` and
    // hands control back here — the shared test driver deliberately does NOT
    // auto-drive this variant (it also carries the pre-existing CR 608.2d
    // tracked-set choice, e.g. Portent of Calamity's per-type exile picks,
    // whose own tests rely on manually driving one prompt at a time). Declare
    // the two independent recipients explicitly, in the order the two
    // instructions actually resolve: flying (the head) first, trample
    // (reached past the intervening false gates) second.
    let _outcome = runner.cast(kathril).resolve();
    let declared_recipients = [second_recipient, kathril];
    let mut next_recipient = declared_recipients.iter();
    while let WaitingFor::ChooseFromZoneChoice { .. } = &runner.state().waiting_for {
        let pick = *next_recipient
            .next()
            .expect("exactly two independent recipient prompts (flying, trample)");
        runner
            .act(GameAction::SelectCards { cards: vec![pick] })
            .expect("per-instruction recipient choice");
    }

    assert_eq!(
        runner.state().objects[&kathril].zone,
        Zone::Battlefield,
        "Kathril must resolve onto the battlefield"
    );

    let state = runner.state();
    let count_on =
        |id: ObjectId, ct: CounterType| state.objects[&id].counters.get(&ct).copied().unwrap_or(0);
    let flying_ct = CounterType::Keyword(Keyword::Flying.kind());
    let trample_ct = CounterType::Keyword(Keyword::Trample.kind());

    // The flying instruction (resolved first) independently chose
    // second_recipient — the first declared object still legal at that point.
    assert_eq!(
        count_on(second_recipient, flying_ct.clone()),
        1,
        "second_recipient receives the flying counter (first instruction's own choice)",
    );
    assert_eq!(
        count_on(second_recipient, trample_ct.clone()),
        0,
        "second_recipient must not also receive the trample counter",
    );
    // The trample instruction (resolved later, past flying/first_strike/…'s
    // now-satisfied-then-irrelevant gates — trample's OWN gate is what matters
    // here) independently chose kathril — the only object left declared.
    assert_eq!(
        count_on(kathril, trample_ct),
        1,
        "kathril receives the trample counter (second instruction's OWN, independent choice)",
    );
    assert_eq!(
        count_on(kathril, flying_ct),
        0,
        "kathril must not also receive the flying counter — the two choices are independent, \
         not one shared pick forced onto a single recipient",
    );
}
