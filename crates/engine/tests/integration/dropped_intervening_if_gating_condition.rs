//! "Dropped intervening-if / gating condition (condition: null)" — build-for-the-class.
//!
//! Each card below previously emitted its condition-bearing trigger / replacement
//! / effect-clause with `condition: null` and a `SwallowedClause { Condition_If }`
//! warning. The fixes route every game-state gate through the single
//! `parse_inner_condition` authority (and its trigger/replacement/effect bridges),
//! or through a source/event-referential seam mirroring the existing
//! `parse_*_intervening_if` combinators.
//!
//! Oracle text is verbatim vs `data/card-data.json` / Scryfall. Each parse test
//! names the assertion that flips when the fix is reverted (the condition returns
//! to `None` AND the `Condition_If` swallow reappears). Runtime tests drive the
//! real cast pipeline for the two most novel runtime-touching changes: Heir's
//! `IfControlsMatching` presence gate and Nine-Lives' `CastFromZone { zone: None }`
//! (the `Option<Zone>` widening's `cast_from_zone.is_some()` arm).
//!
//! THREE cards are DELIBERATELY out of scope and pinned as honest RED
//! (swallowed) — `anchor_to_reality_*` and `heroic_return_*` at the bottom of
//! this file, and `hawkeye_*_is_honest_red` in the A-series. Their gates are not
//! resolvable intervening-if game-state conditions with the seams built today:
//! Anchor compares a just-searched card's mana value (no runtime object-scope
//! binding yet), Heroic Return uses a reflexive "enters this way" replacement
//! (CR 614.1c), and Hawkeye's "if Hawkeye dealt damage to it this turn" names the
//! DYING object (the trigger event's SOURCE, CR 400.7) for which no object-matching
//! `TargetFilter` recipient exists yet (`EventTarget` resolves only a `DamageDealt`
//! recipient, not a `ZoneChanged` source — CR 120.1). They are pinned so this
//! change cannot silently mis-count them as fixed, and so any future half-fix
//! trips a failing test.

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::counter::CounterType;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// ── Verbatim Oracle text ─────────────────────────────────────────────────────

const BURNING_EYE_ZUBERA: &str = "When this creature dies, if 4 or more damage was dealt to it this turn, this creature deals 3 damage to any target.";
const KAMI_OF_TRANSIENCE: &str = "Trample\nWhenever you cast an enchantment spell, put a +1/+1 counter on this creature.\nAt the beginning of each end step, if an enchantment was put into your graveyard from the battlefield this turn, you may return this card from your graveyard to your hand.";
const NINE_LIVES_FAMILIAR: &str = "This creature enters with eight revival counters on it if you cast it.\nWhen this creature dies, if it had a revival counter on it, return it to the battlefield with one fewer revival counter on it at the beginning of the next end step.";
const HAWKEYE: &str = "Reach\nWhenever a creature an opponent controls dies, if Hawkeye dealt damage to it this turn, draw a card.\n{T}: Hawkeye deals 1 damage to any target.";
const ALEX_WILDER: &str = "Whenever Alex Wilder or another creature you control enters, if you cast it from anywhere other than your hand, it gets +2/+0 and gains haste until end of turn.\nEscape—{2}{R}, Exile three other cards from your graveyard. (You may cast this card from your graveyard for its escape cost.)";
const DAWN_EVANGEL: &str = "Whenever a creature dies, if an Aura you controlled was attached to it, return target creature card with mana value 2 or less from your graveyard to your hand.";
const HEIR_OF_THE_ANCIENT_FANG: &str = "This creature enters with a +1/+1 counter on it if you control a modified creature. (Equipment, Auras you control, and counters are modifications.)";
const FEAST_OF_WORMS: &str = "Destroy target land. If that land was legendary, its controller sacrifices another land of their choice.";
const HISOKA: &str =
    "{2}{U}, Discard a card: Counter target spell if it has the same mana value as the discarded card.";
const VANILLA: &str = "";

// ── Helpers ──────────────────────────────────────────────────────────────────

fn has_condition_if_swallow(parsed: &ParsedAbilities) -> bool {
    parsed.parse_warnings.iter().any(|w| {
        format!("{w:?}").contains("SwallowedClause") && format!("{w:?}").contains("Condition_If")
    })
}

fn parse(name: &str, oracle: &str, types: &[&str], subtypes: &[&str]) -> ParsedAbilities {
    let ts: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let sts: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle, name, &[], &ts, &sts)
}

// ══ A1 — Burning-Eye Zubera: amount-first passive damage-to-self this-turn ═════

#[test]
fn burning_eye_zubera_threshold_damage_dies_gate() {
    let p = parse(
        "Burning-Eye Zubera",
        BURNING_EYE_ZUBERA,
        &["Creature"],
        &["Spirit"],
    );
    let cond = format!("{:?}", p.triggers[0].condition);
    // Revert-failing: dropping the amount-first arm returns condition -> None.
    assert!(
        cond.contains("DamageDealtThisTurn")
            && cond.contains("target: SelfRef")
            && cond.contains("comparator: GE")
            && cond.contains("Fixed { value: 4 }"),
        "expected DamageDealtThisTurn>=4 to SelfRef, got {cond}"
    );
    assert!(
        !has_condition_if_swallow(&p),
        "Condition_If swallow must clear"
    );
}

// ══ A2 — Kami of Transience: owner-scoped this-turn zone-transfer tally ════════

#[test]
fn kami_of_transience_owner_scoped_graveyard_tally() {
    let p = parse(
        "Kami of Transience",
        KAMI_OF_TRANSIENCE,
        &["Creature"],
        &["Spirit"],
    );
    // The end-step trigger is the second one; the cast-enchantment trigger has no gate.
    let cond = format!("{:?}", p.triggers[1].condition);
    // Revert-failing: the owner axis must be Owned{You} (CR 404.1), not a controller
    // filter — a control-changed enchantment you control but an opponent OWNS goes to
    // the opponent's graveyard and must NOT satisfy this gate.
    assert!(
        cond.contains("ZoneChangeCountThisTurn")
            && cond.contains("from: Some(Battlefield)")
            && cond.contains("to: Some(Graveyard)")
            && cond.contains("Enchantment")
            && cond.contains("Owned { controller: You }"),
        "expected owner-scoped Enchantment battlefield->graveyard tally, got {cond}"
    );
    assert!(
        !has_condition_if_swallow(&p),
        "Condition_If swallow must clear"
    );
}

// ══ A3 — Nine-Lives dies: HadCounters (LKI) regression guard, no code change ═══

#[test]
fn nine_lives_dies_trigger_uses_lki_had_counters() {
    let p = parse(
        "Nine-Lives Familiar",
        NINE_LIVES_FAMILIAR,
        &["Creature"],
        &["Cat"],
    );
    let cond = format!("{:?}", p.triggers[0].condition);
    // Guards the LKI seam: a future refactor swapping `try_extract_had_counter_condition`
    // for the live-read `try_extract_has_counter_condition` would flip HadCounters->HasCounters.
    assert!(
        cond.contains("HadCounters") && cond.contains("Generic(\"revival\")"),
        "dies trigger must read last-known revival counters (HadCounters), got {cond}"
    );
}

// ══ A4 — Hawkeye: dying-object damage look-back is honest RED (pinned) ═════════

/// Hawkeye, Avenging Archer — honest RED. "Whenever a creature an opponent
/// controls dies, if Hawkeye dealt damage to it this turn, draw a card": the "it"
/// names the creature that DIED, which is the trigger event's SOURCE object
/// (`extract_source_from_event(ZoneChanged)` — CR 400.7: it is a new object in
/// the graveyard, referenced by LKI), NOT its `EventTarget`
/// (`extract_target_object_from_event`, which yields `Some` only for a
/// `DamageDealt` event — CR 120.1).
///
/// An earlier fix bound "it" to `TargetFilter::EventTarget`, which cleared the
/// swallow but made the intervening-if silently ALWAYS-FALSE at runtime: a dies
/// trigger carries a `ZoneChanged` event with no EventTarget, so the
/// `DamageDealtThisTurn` look-back matched zero records and the gate could never
/// hold — Hawkeye would never draw even after dealing the lethal damage. No
/// object-matching `TargetFilter` resolves the trigger *source* object today
/// (`TriggeringSource` is inert in `matches_target_filter`), so the dying-object
/// recipient filter this gate needs does not exist yet.
///
/// Pin: this stays honest RED (swallowed `Condition_If`, no leaked always-false
/// gate) until a dying-object recipient filter is built. Flip the assertions when
/// that seam exists.
#[test]
fn hawkeye_dealt_damage_to_dying_object_is_honest_red() {
    let p = parse(
        "Hawkeye, Avenging Archer",
        HAWKEYE,
        &["Creature"],
        &["Human", "Archer"],
    );
    let cond = format!("{:?}", p.triggers[0].condition);
    // No broken always-false damage look-back leaked onto the dies trigger. The
    // dying object is unresolvable as an EventTarget here, so the gate must NOT be
    // wired to `DamageDealtThisTurn { target: EventTarget }` (which resolves to
    // nothing on a ZoneChanged event). The condition stays dropped (`None`).
    assert!(
        !cond.contains("EventTarget") && !cond.contains("DamageDealtThisTurn"),
        "Hawkeye's dies-trigger damage gate must NOT be wired to an unresolvable \
         EventTarget look-back; condition must stay dropped, got {cond}"
    );
    assert!(
        p.triggers[0].condition.is_none(),
        "the dropped intervening-if leaves condition None (Mandatory shape), got {cond}"
    );
    // Reach-guard: the dies trigger + draw-a-card effect actually parsed, so the
    // swallow below is a real dropped gate — not a card-wide Unimplemented
    // short-circuit that would silence the detector for unrelated reasons.
    let execute = format!("{:?}", p.triggers[0].execute);
    assert!(
        execute.contains("Draw"),
        "the dies trigger's draw-a-card effect must parse, got {execute}"
    );
    // Honest RED: the dropped "if Hawkeye dealt damage to it this turn" gate is
    // flagged as a swallowed condition, not silently ignored.
    assert!(
        has_condition_if_swallow(&p),
        "Hawkeye's dying-object damage look-back must remain a flagged (RED) swallow \
         until the dying-object recipient filter exists. Warnings: {:?}",
        p.parse_warnings
    );
}

// ══ B — Alex Wilder: cast from anywhere OTHER than a named zone ════════════════

#[test]
fn alex_wilder_cast_from_non_hand_zone() {
    let p = parse(
        "Alex Wilder, Runaway",
        ALEX_WILDER,
        &["Creature"],
        &["Human"],
    );
    let cond = format!("{:?}", p.triggers[0].condition);
    // Revert-failing: the And[WasCast, Not(WasCast{Hand})] carries both conjuncts.
    // The positive WasCast excludes a never-cast token; the Not(Hand) excludes hand-casts.
    assert!(
        cond.contains("And")
            && cond.contains("WasCast { zone: None")
            && cond.contains("Not")
            && cond.contains("zone: Some(Hand)"),
        "expected And[WasCast, Not(WasCast{{Hand}})], got {cond}"
    );
    assert!(
        !has_condition_if_swallow(&p),
        "Condition_If swallow must clear"
    );
}

// ══ C — Dawn Evangel: Aura you controlled attached to the dead object (LKI) ════

#[test]
fn dawn_evangel_your_aura_attached_lki() {
    let p = parse(
        "Dawn Evangel",
        DAWN_EVANGEL,
        &["Creature"],
        &["Human", "Cleric"],
    );
    let cond = format!("{:?}", p.triggers[0].condition);
    // Revert-failing: dropping the attachment-first seam returns condition -> None.
    // HasAttachment{Aura, Some(You)} over the dying creature's battlefield->graveyard
    // LKI is the same runtime shape the "if it was enchanted" look-back produces.
    assert!(
        cond.contains("ZoneChangeObjectMatchesFilter")
            && cond.contains("destination: Graveyard")
            && cond.contains("HasAttachment")
            && cond.contains("kind: Aura")
            && cond.contains("controller: Some(You)"),
        "expected ZoneChangeObjectMatchesFilter HasAttachment{{Aura,You}}, got {cond}"
    );
    assert!(
        !has_condition_if_swallow(&p),
        "Condition_If swallow must clear"
    );
}

// ══ D1 — Heir of the Ancient Fang: enters-with gated by control presence ══════

#[test]
fn heir_of_the_ancient_fang_controls_modified_gate() {
    let p = parse(
        "Heir of the Ancient Fang",
        HEIR_OF_THE_ANCIENT_FANG,
        &["Creature"],
        &["Snake"],
    );
    let cond = format!("{:?}", p.replacements[0].condition);
    // Revert-failing: dropping the IsPresent -> IfControlsMatching bridge arm returns
    // condition -> None (Mandatory) so Heir would ALWAYS enter with the counter.
    assert!(
        cond.contains("IfControlsMatching")
            && cond.contains("Modified")
            && cond.contains("controller: Some(You)"),
        "expected IfControlsMatching over a modified creature you control, got {cond}"
    );
    assert!(
        !has_condition_if_swallow(&p),
        "Condition_If swallow must clear"
    );
}

// ══ D2 — Nine-Lives enters: zoneless "if you cast it" -> CastFromZone{None} ════

#[test]
fn nine_lives_enters_gated_on_cast() {
    let p = parse(
        "Nine-Lives Familiar",
        NINE_LIVES_FAMILIAR,
        &["Creature"],
        &["Cat"],
    );
    let repl = p
        .replacements
        .iter()
        .find(|r| {
            r.execute.as_ref().is_some_and(|e| {
                matches!(
                    &*e.effect,
                    engine::types::ability::Effect::PutCounter { .. }
                )
            })
        })
        .expect("enters-with-counters replacement must exist");
    let cond = format!("{:?}", repl.condition);
    // Revert-failing: without the "you cast it" -> WasCast{None} -> CastFromZone{None}
    // wiring the condition returns to None (Mandatory), so a non-cast entry would
    // wrongly gain the 8 counters.
    assert!(
        cond.contains("CastFromZone { zone: None }"),
        "expected CastFromZone{{zone:None}} (cast from anywhere), got {cond}"
    );
    assert!(
        !has_condition_if_swallow(&p),
        "Condition_If swallow must clear"
    );
}

// ══ E1 — Feast of Worms: leading LKI supertype gate on the destroyed land ═════

#[test]
fn feast_of_worms_legendary_lki_gate() {
    let p = parse("Feast of Worms", FEAST_OF_WORMS, &["Sorcery"], &[]);
    let sub = p.abilities[0]
        .sub_ability
        .as_ref()
        .expect("second-clause sub-ability must exist");
    let cond = format!("{:?}", sub.condition);
    // Revert-failing: dropping the "if that land was <supertype>" arm returns the
    // sub-ability condition -> None. use_lki:true reads the land at last-known info.
    assert!(
        cond.contains("TargetMatchesFilter")
            && cond.contains("HasSupertype")
            && cond.contains("Legendary")
            && cond.contains("use_lki: true"),
        "expected LKI Legendary supertype gate on the sacrifice sub, got {cond}"
    );
    // The gated effect itself parses (controller sacrifices another land).
    assert!(
        format!("{:?}", sub.effect).contains("Sacrifice"),
        "the gated sub-effect must be a Sacrifice, got {:?}",
        sub.effect
    );
    assert!(
        !has_condition_if_swallow(&p),
        "Condition_If swallow must clear"
    );
}

// ══ E2 — Hisoka: target MV == cost-paid (discarded) object MV ═════════════════

#[test]
fn hisoka_target_mv_equals_discarded_mv() {
    let p = parse(
        "Hisoka, Minamo Sensei",
        HISOKA,
        &["Creature", "Legendary"],
        &["Human", "Wizard"],
    );
    let cond = format!("{:?}", p.abilities[0].condition);
    // Revert-failing: dropping the "it has the same mana value as the discarded card"
    // combinator returns condition -> None. Both operands are ObjectManaValue refs
    // (Target vs CostPaidObject), no fixed threshold.
    assert!(
        cond.contains("QuantityCheck")
            && cond.contains("ObjectManaValue { scope: Target }")
            && cond.contains("comparator: EQ")
            && cond.contains("ObjectManaValue { scope: CostPaidObject }"),
        "expected QuantityCheck ManaValue(Target) EQ ManaValue(CostPaidObject), got {cond}"
    );
    assert!(
        !has_condition_if_swallow(&p),
        "Condition_If swallow must clear"
    );
}

// ══ D1 runtime — presence gate flips the enters-with counter ══════════════════

/// Positive reach-guard: Heir cast while controlling a modified creature (a
/// creature bearing a +1/+1 counter, CR 700.9) enters WITH its +1/+1 counter.
#[test]
fn heir_runtime_modified_present_gains_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    // A modified creature you control: a vanilla bear carrying a +1/+1 counter.
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_counter(bear, CounterType::Plus1Plus1, 1);
    let heir = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Heir of the Ancient Fang",
            1,
            1,
            HEIR_OF_THE_ANCIENT_FANG,
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(heir).resolve();
    assert_eq!(
        outcome.zone_of(heir),
        Zone::Battlefield,
        "Heir must resolve to the battlefield"
    );
    assert_eq!(
        outcome.counters(heir, CounterType::Plus1Plus1),
        1,
        "with a modified creature present, Heir must enter with a +1/+1 counter"
    );
}

/// Revert-failing veto: Heir cast while controlling NO modified creature enters
/// with ZERO counters. Reverting the `IsPresent -> IfControlsMatching` bridge makes
/// the replacement Mandatory, so this cast would wrongly gain the counter. Paired
/// with the positive above (a real placement), this is not a vacuous negative.
#[test]
fn heir_runtime_no_modified_no_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    // An UNmodified creature you control (no counters, not equipped/enchanted).
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    let heir = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Heir of the Ancient Fang",
            1,
            1,
            HEIR_OF_THE_ANCIENT_FANG,
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(heir).resolve();
    assert_eq!(
        outcome.zone_of(heir),
        Zone::Battlefield,
        "Heir must resolve to the battlefield"
    );
    assert_eq!(
        outcome.counters(heir, CounterType::Plus1Plus1),
        0,
        "with no modified creature, Heir must enter with no +1/+1 counter"
    );
}

// ══ D2 runtime — cast entry satisfies CastFromZone{None} and places counters ══

/// Positive reach-guard exercising the `CastFromZone { zone: None }` runtime arm
/// (`cast_from_zone.is_some()`): a hard cast of Nine-Lives from hand sets
/// `cast_from_zone = Some(Hand)`, so the zoneless gate holds and the 8 revival
/// counters are placed. Proves the widened `Option<Zone>` evaluator's `None` arm
/// returns true for a real cast (a bug that returned false would place 0).
#[test]
fn nine_lives_runtime_cast_gains_revival_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let familiar = scenario
        .add_creature_to_hand_from_oracle(P0, "Nine-Lives Familiar", 0, 3, NINE_LIVES_FAMILIAR)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(familiar).resolve();
    assert_eq!(
        outcome.zone_of(familiar),
        Zone::Battlefield,
        "the cast Nine-Lives must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(familiar, CounterType::Generic("revival".to_string())),
        8,
        "a cast Nine-Lives (cast_from_zone = Some) must enter with eight revival counters"
    );
}

/// Control (NOT a gate discriminator): `add_creature_from_oracle` seeds a permanent
/// directly onto the battlefield, bypassing the ETB replacement pipeline, so it
/// carries no revival counters regardless of the gate. Paired with the cast
/// positive above, it proves the 8 counters in that test come from the cast
/// pipeline + replacement (not from seeding). The condition-gate discrimination for
/// D2 lives in the PARSE test `nine_lives_enters_gated_on_cast` (revert → the
/// condition returns to `None`/Mandatory); a true runtime negative would require
/// reanimating a fresh non-cast object through the pipeline, which the harness does
/// not offer cleanly.
#[test]
fn nine_lives_runtime_noncast_entry_has_no_revival_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let familiar = scenario
        .add_creature_from_oracle(P0, "Nine-Lives Familiar", 0, 3, NINE_LIVES_FAMILIAR)
        .id();
    let runner = scenario.build();
    assert_eq!(
        runner
            .state()
            .objects
            .get(&familiar)
            .unwrap()
            .counters
            .get(&CounterType::Generic("revival".to_string()))
            .copied()
            .unwrap_or(0),
        0,
        "a directly-seeded (non-cast) Nine-Lives must carry no revival counters"
    );
}

// ══ Vanilla sanity for the runtime harness (self-check) ═══════════════════════

#[test]
fn vanilla_control_creature_is_not_modified() {
    // Guards the D1 negative fixture: a bare creature must not be "modified", else
    // heir_runtime_no_modified_no_counter would be vacuous.
    let p = parse("Grizzly Bears", VANILLA, &["Creature"], &["Bear"]);
    assert!(p.replacements.is_empty() && p.triggers.is_empty());
}

// ══ Pinned KNOWN GAPS — honest-RED cards in this backlog category ═════════════
//
// Two cards from the 605-card "Dropped intervening-if / gating condition"
// backlog are DELIBERATELY left unsupported (swallowed / RED), because their
// gates are NOT intervening-if game-state conditions and belong to separate,
// unbuilt seams. Pinning them here keeps them from being silently mis-counted as
// "fixed" by this change and turns any accidental future half-fix into a failing
// tripwire (flip the assertion when the real seam is built).

const ANCHOR_TO_REALITY: &str = "As an additional cost to cast this spell, sacrifice an artifact or creature.\nSearch your library for an Equipment or Vehicle card, put that card onto the battlefield, then shuffle. If it has mana value less than the sacrificed permanent's mana value, scry 2.";
const HEROIC_RETURN: &str = "This spell costs {2} less to cast if a creature is attacking you.\nReturn target creature card from your graveyard to the battlefield. If a Hero enters this way, it enters with two additional +1/+1 counters on it.";

/// Anchor to Reality — honest RED. "If it has mana value less than the sacrificed
/// permanent's mana value, scry 2" compares the just-SEARCHED/placed card's mana
/// value (CR 608.2c anaphor, not a target) to the cost-paid permanent's. Anchor
/// declares no object target and the searched card has no runtime object-scope
/// binding (search / change-zone never populate `effect_context_object`), so the
/// shared `parse_it_mana_value_vs_cost_paid_object` combinator is intentionally
/// NOT wired for this leading surface (only the Hisoka target-anaphor suffix is).
///
/// Revert-failing: re-adding the leading-`if` arm would parse Anchor "clean" (no
/// swallow) with a `QuantityCheck` whose LHS is `ObjectManaValue { Target }` — a
/// scope that resolves to nothing here — flipping this pin. Honest RED (swallowed,
/// with no leaked mana-value gate) is correct until the searched-object binding
/// exists.
#[test]
fn anchor_to_reality_dynamic_mv_gate_is_honest_red() {
    let p = parse("Anchor to Reality", ANCHOR_TO_REALITY, &["Artifact"], &[]);
    let abilities = format!("{:?}", p.abilities);
    // No broken dynamic-MV gate leaked into the ability tree.
    assert!(
        !abilities.contains("ObjectManaValue") && !abilities.contains("QuantityCheck"),
        "Anchor's scry gate must NOT be wired to a target-scoped ObjectManaValue \
         (unresolvable here); got {abilities}"
    );
    // Reach-guard: the search + scry pipeline actually parsed (so the swallow below
    // is a real dropped gate, not a card-wide Unimplemented short-circuit).
    assert!(
        abilities.contains("SearchLibrary") && abilities.contains("Scry"),
        "the search->scry effect chain must parse, got {abilities}"
    );
    // Honest RED: the dropped mana-value gate is flagged, not silently ignored.
    assert!(
        has_condition_if_swallow(&p),
        "Anchor's dynamic-MV scry gate must remain a flagged (RED) swallow until the \
         searched-object runtime binding exists. Warnings: {:?}",
        p.parse_warnings
    );
}

/// Heroic Return — honest RED. "If a Hero enters this way, it enters with two
/// additional +1/+1 counters" is a REFLEXIVE replacement created during the
/// spell's resolution (CR 614.1c), gating a bonus on the entering RETURNED
/// creature being a Hero — not a game-state intervening-if on the spell. The
/// intervening-if seam this change targets does not model reflexive
/// enters-this-way replacements (the enters-with-counters parser mis-attaches an
/// ungated rider to the spell's own `SelfRef`, which never enters), so this card
/// is explicitly out of scope and left RED.
///
/// Pin: flip this when a real "if a <type> enters this way" reflexive-replacement
/// seam is built. Until then, asserting the swallow persists prevents mis-marking
/// Heroic Return as fixed by this change.
#[test]
fn heroic_return_enters_this_way_is_honest_red() {
    let p = parse("Heroic Return", HEROIC_RETURN, &["Sorcery"], &[]);
    // Honest RED: the "if a Hero enters this way" rider is flagged as a dropped
    // condition, not modeled as a working typed gate.
    assert!(
        has_condition_if_swallow(&p),
        "Heroic Return's 'if a Hero enters this way' reflexive rider must remain a \
         flagged (RED) swallow — it is out of scope for the intervening-if seam. \
         Warnings: {:?}",
        p.parse_warnings
    );
    // The reflexive gate was NOT lifted into a typed condition: every replacement
    // the enters-with-counters parser produced here is still ungated
    // (`condition: None`, Mandatory) — the "if a Hero enters this way" gate is
    // dropped, matching the swallow above. (If a future seam models it, this
    // `condition` becomes `Some(..)` and the assertion flips, per the pin.)
    assert!(
        p.replacements.iter().all(|r| r.condition.is_none()),
        "no typed enters-this-way gate should exist yet; every rider stays ungated, \
         got {:?}",
        p.replacements
    );
}
