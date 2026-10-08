//! Issue #5937 — Wilhelt, the Rotcleaver: the intervening-if "if it didn't
//! have decayed" was swallowed, so Wilhelt created a 2/2 decayed Zombie for
//! EVERY Zombie death — including decayed ones — producing the reported
//! infinite-ish token loop (each decayed token's death minted the next).
//!
//! Verbatim Oracle text (corpus card-data, MTGJSON):
//!   "Whenever another Zombie you control dies, if it didn't have decayed,
//!   create a 2/2 black Zombie creature token with decayed. (It can't block.
//!   When it attacks, sacrifice it at end of combat.)
//!   At the beginning of your end step, you may sacrifice a Zombie. If you do,
//!   draw a card."
//!
//! Discriminator: the fix attaches
//!   `Not(ZoneChangeObjectMatchesFilter { Battlefield → Graveyard,
//!    creature + HasKeywordKind{Decayed} })`
//! as the dies trigger's CR 603.4 intervening-if. Reverting the parser arm
//! returns `condition: None` (P1 flips), re-opens the Condition_If swallow
//! (P2 flips), and un-gates token creation on decayed deaths (R1/R3/R4b/R5
//! flip from "no token" to "token created").
//!
//! Verified CR list (all grep-verified against docs/MagicCompRules.txt):
//! - CR 603.4 — intervening-if, checked at fire AND again at resolution.
//! - CR 603.10a — dies triggers look back in time; the event object is read
//!   from the zone-change LKI snapshot.
//! - CR 603.6c — leaves-the-battlefield family (dies = battlefield →
//!   graveyard subset) is the look-back class this arm serves.
//! - CR 700.4 — "dies" means put into a graveyard from the battlefield.
//! - CR 400.7 — the object in the graveyard is a new object; the snapshot
//!   is the only rules-correct authority for "had".
//! - CR 613.1f — keywords are layer-6 computed; the snapshot captures the
//!   layer-computed set, so continuous grants count (R5).
//! - CR 702.147a — decayed: "can't block" + attack-sacrifice triggered ability.
//! - CR 608.2h — R7 note below (resolution re-check reads the snapshot).
//!
//! R7 note — resolution re-check divergence is UNREACHABLE for this condition
//! class: CR 603.4 re-checks the condition as the trigger resolves, and CR
//! 608.2h routes a departed subject to last known information — but both the
//! fire-time check and the resolution re-check evaluate the SAME immutable
//! `ZoneChangeRecord` snapshot (types/game_state.rs), so the two verdicts can
//! never disagree for a past-tense "had <keyword>" gate. No test can force a
//! fire-true/resolve-false split here, unlike the live Sharp-Eyed Rookie gate
//! in l02_bb4_intervening_if.rs.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::{
    Effect, FilterProp, PtValue, TargetFilter, TriggerCondition, TypedFilter,
};
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

// ---------------------------------------------------------------------------
// Verbatim Oracle text (corpus card-data.json, character-exact)
// ---------------------------------------------------------------------------

const WILHELT: &str = "Whenever another Zombie you control dies, if it didn't have decayed, create a 2/2 black Zombie creature token with decayed. (It can't block. When it attacks, sacrifice it at end of combat.)\nAt the beginning of your end step, you may sacrifice a Zombie. If you do, draw a card.";

const DESTROY: &str = "Destroy target creature.";
const PUT_MINUS_ONE: &str = "Put a -1/-1 counter on target creature.";
const GRANT_DECAYED: &str = "Target creature gains decayed until end of turn.";

// ---------------------------------------------------------------------------
// Parse helpers (mirrors l02_bb4_intervening_if.rs)
// ---------------------------------------------------------------------------

fn parse_wilhelt() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        WILHELT,
        "Wilhelt, the Rotcleaver",
        &[],
        &["Creature".to_string()],
        &[],
    )
}

/// True when the parse produced a `SwallowedClause` diagnostic with `detector`.
fn has_swallowed(detector: &str) -> bool {
    parse_wilhelt().parse_warnings.iter().any(|w| {
        matches!(
            w,
            OracleDiagnostic::SwallowedClause { detector: d, .. } if d == detector
        )
    })
}

/// Count 2/2 decayed "Zombie" tokens Wilhelt has minted onto the battlefield.
fn zombie_tokens_on_battlefield(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield && o.name == "Zombie")
        .map(|o| o.id)
        .collect()
}

/// Standard board: Wilhelt (P0, verbatim oracle, Zombie Warrior) plus a Zombie
/// victim for P0 (decayed when `decayed`), and a 0-cost destroy spell in hand.
fn wilhelt_board(decayed: bool) -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wilhelt = scenario
        .add_creature_from_oracle(P0, "Wilhelt, the Rotcleaver", 3, 3, WILHELT)
        .with_subtypes(vec!["Zombie", "Warrior"])
        .id();
    let mut victim_builder = scenario.add_creature(P0, "Shambler", 2, 2);
    victim_builder.with_subtypes(vec!["Zombie"]);
    if decayed {
        victim_builder.with_keyword(Keyword::Decayed);
    }
    let victim = victim_builder.id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    (scenario, wilhelt, victim, destroy)
}

// ---------------------------------------------------------------------------
// P (parse fidelity)
// ---------------------------------------------------------------------------

/// P1: the dies trigger carries the negated keyword-possession look-back gate.
/// Revert-failing assertion: with the parser arm reverted, no trigger carries a
/// condition (`expect` panics on `None`).
#[test]
fn wilhelt_condition_is_not_had_decayed() {
    let parsed = parse_wilhelt();
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.condition.is_some())
        .expect("Wilhelt's dies trigger must carry an intervening-if condition");
    let cond = trigger.condition.clone().unwrap();
    assert_eq!(
        cond,
        TriggerCondition::Not {
            condition: Box::new(TriggerCondition::ZoneChangeObjectMatchesFilter {
                // CR 700.4 + CR 603.10a: dies — battlefield → graveyard look-back.
                origin: Some(Zone::Battlefield),
                destination: Zone::Graveyard,
                filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                    FilterProp::HasKeywordKind {
                        value: KeywordKind::Decayed,
                    },
                ])),
            }),
        },
        "expected Not(had decayed) on the dying event object"
    );
}

/// P2: the Condition_If swallow is cleared (currently present pre-fix). P1 is
/// the positive reach-guard pairing this negative: the clause was consumed
/// into the condition, so the swallow's absence is non-vacuous.
#[test]
fn wilhelt_condition_if_swallow_cleared() {
    assert!(
        parse_wilhelt()
            .triggers
            .iter()
            .any(|t| t.condition.is_some()),
        "reach-guard: the condition attached (P1)"
    );
    assert!(
        !has_swallowed("Condition_If"),
        "Condition_If swallow must clear once the intervening-if attaches"
    );
}

/// P3: peeling the if-clause leaves the residual effect intact — a 2/2 black
/// Zombie creature token with decayed (CR 702.147a).
#[test]
fn wilhelt_residual_effect_is_decayed_zombie_token() {
    let parsed = parse_wilhelt();
    let execute = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("dies trigger has an execute ability");
    match &*execute.effect {
        Effect::Token {
            name,
            power,
            toughness,
            colors,
            keywords,
            ..
        } => {
            assert_eq!(name, "Zombie");
            assert_eq!(*power, PtValue::Fixed(2));
            assert_eq!(*toughness, PtValue::Fixed(2));
            assert_eq!(colors, &[ManaColor::Black]);
            assert!(
                keywords.contains(&Keyword::Decayed),
                "token must carry decayed, got {keywords:?}"
            );
        }
        other => panic!("expected Effect::Token, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// R (runtime, real trigger pipeline)
// ---------------------------------------------------------------------------

/// R1: a DECAYED Zombie's death does NOT mint a token — the reported bug.
/// Reach-guard: the victim died (graveyard), so the zero is the gate declining,
/// not a never-fired trigger. Sibling positive: R2. Revert-failing assertion:
/// token count becomes 1 when the condition is dropped.
#[test]
fn wilhelt_decayed_death_creates_no_token() {
    let (scenario, _wilhelt, victim, destroy) = wilhelt_board(true);
    let mut runner = scenario.build();

    let out = runner.cast(destroy).target_object(victim).resolve();
    assert_eq!(
        out.zone_of(victim),
        Zone::Graveyard,
        "victim died (reach-guard)"
    );
    assert_eq!(
        zombie_tokens_on_battlefield(&runner).len(),
        0,
        "decayed Zombie death must NOT create a token (CR 603.4 gate)"
    );
}

/// R2: a NON-decayed Zombie's death mints exactly one 2/2 decayed Zombie token
/// (the positive sibling / reach companion for R1).
#[test]
fn wilhelt_nondecayed_death_creates_one_token() {
    let (scenario, _wilhelt, victim, destroy) = wilhelt_board(false);
    let mut runner = scenario.build();

    let out = runner.cast(destroy).target_object(victim).resolve();
    assert_eq!(
        out.zone_of(victim),
        Zone::Graveyard,
        "victim died (reach-guard)"
    );
    let tokens = zombie_tokens_on_battlefield(&runner);
    assert_eq!(tokens.len(), 1, "exactly one Zombie token minted");
    let token = &runner.state().objects[&tokens[0]];
    assert_eq!((token.power, token.toughness), (Some(2), Some(2)));
    assert!(
        engine::game::keywords::has_keyword(token, &Keyword::Decayed),
        "minted token has decayed"
    );
}

/// R3: the reported loop — destroying the token R2 minted must NOT mint a
/// second token (the token itself has decayed). Reach-guard: the first death
/// DID mint a token, and the second destroy removed it from the battlefield.
#[test]
fn wilhelt_token_death_does_not_loop() {
    let (mut scenario, _wilhelt, victim, destroy) = wilhelt_board(false);
    let destroy2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder Two", true, DESTROY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(destroy).target_object(victim).resolve();
    let tokens = zombie_tokens_on_battlefield(&runner);
    assert_eq!(tokens.len(), 1, "reach-guard: first death minted the token");
    let token = tokens[0];

    runner.cast(destroy2).target_object(token).resolve();
    assert_ne!(
        runner.state().objects.get(&token).map(|o| o.zone),
        Some(Zone::Battlefield),
        "reach-guard: the token left the battlefield"
    );
    assert_eq!(
        zombie_tokens_on_battlefield(&runner).len(),
        0,
        "the decayed token's death must not mint a replacement (no loop)"
    );
}

/// R4a: SBA death (CR 704.5f — toughness 0 via a -1/-1 counter on a 1/1) of a non-decayed
/// Zombie mints a token — the gate composes with state-based deaths, not just
/// destroy effects.
#[test]
fn wilhelt_sba_death_nondecayed_creates_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Wilhelt, the Rotcleaver", 3, 3, WILHELT)
        .with_subtypes(vec!["Zombie", "Warrior"]);
    let victim = scenario
        .add_creature(P0, "Frail Shambler", 1, 1)
        .with_subtypes(vec!["Zombie"])
        .id();
    let shrink = scenario
        .add_spell_to_hand_from_oracle(P0, "Shrink", true, PUT_MINUS_ONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let out = runner.cast(shrink).target_object(victim).resolve();
    assert_eq!(
        out.zone_of(victim),
        Zone::Graveyard,
        "victim died to the SBA (reach-guard)"
    );
    assert_eq!(
        zombie_tokens_on_battlefield(&runner).len(),
        1,
        "non-decayed SBA death mints the token"
    );
}

/// R4b: the mirrored decayed variant of R4a — an SBA death of a DECAYED Zombie
/// mints nothing. Reach-guard: the victim died; sibling positive: R4a.
#[test]
fn wilhelt_sba_death_decayed_creates_no_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Wilhelt, the Rotcleaver", 3, 3, WILHELT)
        .with_subtypes(vec!["Zombie", "Warrior"]);
    let victim = scenario
        .add_creature(P0, "Frail Rotter", 1, 1)
        .with_subtypes(vec!["Zombie"])
        .with_keyword(Keyword::Decayed)
        .id();
    let shrink = scenario
        .add_spell_to_hand_from_oracle(P0, "Shrink", true, PUT_MINUS_ONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let out = runner.cast(shrink).target_object(victim).resolve();
    assert_eq!(
        out.zone_of(victim),
        Zone::Graveyard,
        "victim died to the SBA (reach-guard)"
    );
    assert_eq!(
        zombie_tokens_on_battlefield(&runner).len(),
        0,
        "decayed SBA death must not mint a token"
    );
}

/// R5 (hostile LKI): a base NON-decayed Zombie granted decayed until end of
/// turn (CR 613.1f layer 6) and then destroyed mints NOTHING — the CR 603.10a
/// zone-change snapshot captures the LAYER-COMPUTED keyword set, so the
/// continuous grant counts as "had decayed". A base-keyword-only read would
/// wrongly mint a token here. Reach-guards: the grant is live on the victim
/// before the destroy resolves, and the victim died.
#[test]
fn wilhelt_granted_decayed_death_creates_no_token() {
    let (mut scenario, _wilhelt, victim, destroy) = wilhelt_board(false);
    let grant = scenario
        .add_spell_to_hand_from_oracle(P0, "Grant Rot", true, GRANT_DECAYED)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(grant).target_object(victim).resolve();
    assert!(
        engine::game::keywords::has_keyword(&runner.state().objects[&victim], &Keyword::Decayed),
        "reach-guard: the until-end-of-turn decayed grant is live on the victim"
    );

    let out = runner.cast(destroy).target_object(victim).resolve();
    assert_eq!(
        out.zone_of(victim),
        Zone::Graveyard,
        "victim died (reach-guard)"
    );
    assert_eq!(
        zombie_tokens_on_battlefield(&runner).len(),
        0,
        "layer-granted decayed must count in the death snapshot — no token"
    );
}

/// R6a (scope sibling): an OPPONENT's Zombie dying does not fire Wilhelt at all
/// ("another Zombie YOU control"). Reach-guard: the victim died.
#[test]
fn wilhelt_opponent_zombie_death_no_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Wilhelt, the Rotcleaver", 3, 3, WILHELT)
        .with_subtypes(vec!["Zombie", "Warrior"]);
    let victim = scenario
        .add_creature(P1, "Enemy Shambler", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let out = runner.cast(destroy).target_object(victim).resolve();
    assert_eq!(
        out.zone_of(victim),
        Zone::Graveyard,
        "victim died (reach-guard)"
    );
    assert_eq!(
        zombie_tokens_on_battlefield(&runner).len(),
        0,
        "opponent's Zombie death is out of scope — no trigger, no token"
    );
}

/// R6b (scope sibling): Wilhelt ITSELF dying mints nothing — "another Zombie"
/// excludes the source. Reach-guard: Wilhelt died.
#[test]
fn wilhelt_self_death_no_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wilhelt = scenario
        .add_creature_from_oracle(P0, "Wilhelt, the Rotcleaver", 3, 3, WILHELT)
        .with_subtypes(vec!["Zombie", "Warrior"])
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let out = runner.cast(destroy).target_object(wilhelt).resolve();
    assert_eq!(
        out.zone_of(wilhelt),
        Zone::Graveyard,
        "Wilhelt died (reach-guard)"
    );
    assert_eq!(
        zombie_tokens_on_battlefield(&runner).len(),
        0,
        "Wilhelt's own death is excluded by \"another\" — no token"
    );
}
