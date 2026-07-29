//! Unit tests for `policies::removal_lethality` — CR 704.5f-h removal-target
//! lethality. No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! The pure `outcome_is_lethal` arithmetic is checked directly; the composed
//! `pending_damage_to_object` / `lethality_bonus` pair runs against a real
//! `PolicyContext` built over a pending damage cast in `TargetSelection`,
//! mirroring the engine's own
//! `effects_returns_pending_cast_during_target_selection` fixture. The spell
//! object carries real keywords, so the source-dependent damage results
//! (CR 120.3d wither/infect counters, CR 702.2b deathtouch) are exercised
//! through the same `object_has_effective_keyword_kind` authority the engine's
//! `DamageContext::from_source` reads.

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::game_object::GameObject;
use engine::game::zones::create_object;
use engine::types::ability::{
    DamageContextSnapshot, DamageSource, EachDamageRecipient, Effect, EffectKind, QuantityExpr,
    ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    GameState, PendingCast, TargetEffectDetail, TargetSelectionSlot, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::removal_lethality::*;

const AI: PlayerId = PlayerId(0);
const OPP: PlayerId = PlayerId(1);

/// The body a removal spell is pointed at.
#[derive(Clone, Copy)]
struct Body {
    toughness: i32,
    damage_marked: u32,
    indestructible: bool,
}

impl Body {
    const fn new(toughness: i32) -> Self {
        Self {
            toughness,
            damage_marked: 0,
            indestructible: false,
        }
    }

    const fn marked(mut self, damage_marked: u32) -> Self {
        self.damage_marked = damage_marked;
        self
    }

    const fn indestructible(mut self) -> Self {
        self.indestructible = true;
        self
    }
}

/// Shape `object_id` into `body` in place so the pure-arithmetic helper and the
/// composed fixture build the identical creature.
fn shape_body(state: &mut GameState, object_id: ObjectId, body: Body) {
    let obj = state.objects.get_mut(&object_id).unwrap();
    obj.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![CoreType::Creature],
        subtypes: Vec::new(),
    };
    obj.power = Some(1);
    obj.toughness = Some(body.toughness);
    obj.damage_marked = body.damage_marked;
    if body.indestructible {
        obj.keywords.push(Keyword::Indestructible);
    }
}

// ─── outcome_is_lethal (pure CR 704.5f-h arithmetic) ────────────────────────

/// A detached stand-in creature carrying only the fields lethality reads.
fn creature(body: Body) -> GameObject {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let id = create_object(&mut state, CardId(9), OPP, "Body".into(), Zone::Battlefield);
    shape_body(&mut state, id, body);
    state.objects.remove(&id).unwrap()
}

/// Ordinary marked damage from a source with no relevant keywords (CR 120.3e).
fn marked(amount: u32) -> DamageOutcome {
    DamageOutcome {
        marked: amount,
        minus_counters: 0,
        deathtouch: false,
    }
}

#[test]
fn exact_toughness_is_lethal() {
    // CR 704.5g: 3 damage on an undamaged 3-toughness body destroys it.
    assert!(outcome_is_lethal(&creature(Body::new(3)), &marked(3)));
}

#[test]
fn short_of_toughness_is_not_lethal() {
    // The #6582 misplay: 3 damage on a 7-toughness body.
    assert!(!outcome_is_lethal(&creature(Body::new(7)), &marked(3)));
}

#[test]
fn prior_marked_damage_lowers_the_bar() {
    // CR 704.5g: marked damage accumulates — 1 already + 3 new ≥ 4 toughness.
    assert!(outcome_is_lethal(
        &creature(Body::new(4).marked(1)),
        &marked(3)
    ));
}

#[test]
fn indestructible_survives_lethal_marked_damage() {
    // CR 702.12b: indestructible ignores the lethal-damage state-based action.
    assert!(!outcome_is_lethal(
        &creature(Body::new(1).indestructible()),
        &marked(99)
    ));
}

#[test]
fn zero_toughness_is_not_killed_by_the_spell() {
    // Already dying to its own 0-toughness SBA (CR 704.5f), not to this damage.
    assert!(!outcome_is_lethal(&creature(Body::new(0)), &marked(5)));
}

#[test]
fn deathtouch_marked_damage_kills_any_size_body() {
    // CR 704.5h + CR 702.2b: 1 deathtouch damage destroys a 7/7 that the same
    // amount of ordinary marked damage leaves untouched.
    let outcome = DamageOutcome {
        marked: 1,
        minus_counters: 0,
        deathtouch: true,
    };
    assert!(outcome_is_lethal(&creature(Body::new(7)), &outcome));
    assert!(!outcome_is_lethal(&creature(Body::new(7)), &marked(1)));
}

#[test]
fn deathtouch_without_marked_damage_is_not_lethal() {
    // CR 704.5h keys on damage having been MARKED; a wither/infect deathtouch
    // source marks none (CR 120.3d), so only the counters can kill.
    let outcome = DamageOutcome {
        marked: 0,
        minus_counters: 1,
        deathtouch: true,
    };
    assert!(!outcome_is_lethal(&creature(Body::new(7)), &outcome));
}

#[test]
fn deathtouch_does_not_beat_indestructible() {
    // CR 702.12b: indestructible ignores CR 704.5h as well as CR 704.5g.
    let outcome = DamageOutcome {
        marked: 3,
        minus_counters: 0,
        deathtouch: true,
    };
    assert!(!outcome_is_lethal(
        &creature(Body::new(7).indestructible()),
        &outcome
    ));
}

#[test]
fn minus_counters_kill_through_indestructible() {
    // CR 120.3d + CR 122.1a + CR 704.5f: -1/-1 counters drive a 3/3 to 0
    // toughness, and CR 704.5f is not a destruction, so CR 702.12b does not
    // save it — the case a marked-damage-only model wrongly calls a waste.
    let outcome = DamageOutcome {
        marked: 0,
        minus_counters: 3,
        deathtouch: false,
    };
    assert!(outcome_is_lethal(
        &creature(Body::new(3).indestructible()),
        &outcome
    ));
}

#[test]
fn minus_counters_lower_the_marked_damage_bar() {
    // CR 122.1a + CR 704.5g: counters reduce toughness, so the marked-damage
    // threshold drops with it — 2 counters + 2 marked kills a 4/4, not a 5/5.
    let outcome = DamageOutcome {
        marked: 2,
        minus_counters: 2,
        deathtouch: false,
    };
    assert!(outcome_is_lethal(&creature(Body::new(4)), &outcome));
    assert!(!outcome_is_lethal(&creature(Body::new(5)), &outcome));
}

// ─── pending_damage_to_object / lethality_bonus (composed) ──────────────────

/// Build a pending cast of `effect` in `TargetSelection`, aimed at one opponent
/// creature shaped by `body`, and run `probe` against the resulting context.
/// `spell_keywords` land on the spell object, which is the default damage source
/// (CR 120.3) — on `base_keywords` too, since off-battlefield keyword reads go
/// through `off_zone_characteristics` rather than `keywords`.
fn with_pending<R>(
    spell_keywords: &[Keyword],
    body: Body,
    effect: Effect,
    probe: impl FnOnce(&PolicyContext<'_>, ObjectId, &GameObject) -> R,
) -> R {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let spell = create_object(&mut state, CardId(1), AI, "Removal".into(), Zone::Stack);
    let target = create_object(&mut state, CardId(2), OPP, "Body".into(), Zone::Battlefield);
    shape_body(&mut state, target, body);
    {
        let obj = state.objects.get_mut(&spell).unwrap();
        obj.keywords.extend(spell_keywords.iter().cloned());
        obj.base_keywords.extend(spell_keywords.iter().cloned());
    }

    let ability = ResolvedAbility::new(effect, Vec::new(), spell, AI);
    let pending = PendingCast::new(spell, CardId(1), ability, ManaCost::zero());
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::TargetSelection {
            player: AI,
            pending_cast: Box::new(pending),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: Vec::new(),
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            }],
            mode_labels: Vec::new(),
            selection: Default::default(),
        },
        candidates: Vec::new(),
    };
    let candidate = CandidateAction {
        action: GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Target),
    };
    let config = AiConfig::default();
    let aicontext = AiContext::empty(&config.weights);
    let ctx = PolicyContext {
        state: &state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &aicontext,
        cast_facts: None,
        search_depth: SearchDepth::Root,
    };
    let target_obj = state.objects.get(&target).unwrap();
    probe(&ctx, target, target_obj)
}

fn bonus_for(spell_keywords: &[Keyword], body: Body, effect: Effect) -> f64 {
    with_pending(spell_keywords, body, effect, |ctx, id, target| {
        lethality_bonus(ctx, id, target)
    })
}

fn pending_for(spell_keywords: &[Keyword], body: Body, effect: Effect) -> PendingDamage {
    with_pending(spell_keywords, body, effect, |ctx, id, target| {
        pending_damage_to_object(ctx, id, target)
    })
}

fn burn(damage: i32) -> Effect {
    burn_from(damage, None)
}

fn burn_from(damage: i32, damage_source: Option<DamageSource>) -> Effect {
    Effect::DealDamage {
        amount: QuantityExpr::Fixed { value: damage },
        target: TargetFilter::Any,
        damage_source,
        excess: None,
    }
}

#[test]
fn lethal_target_is_rewarded() {
    // 3 damage kills a 3/3 → the clean-kill bonus.
    let b = bonus_for(&[], Body::new(3), burn(3));
    assert!(
        (b - LETHAL_BONUS).abs() < 1e-9,
        "expected +{LETHAL_BONUS} for a clean kill, got {b}"
    );
}

#[test]
fn nonlethal_big_target_is_penalized() {
    // The #6582 misplay: 3 damage on a 7/7. Must score net-negative so a
    // killable smaller target outranks it.
    let b = bonus_for(&[], Body::new(7), burn(3));
    assert!(
        b < 0.0,
        "expected a waste penalty for a survivable target, got {b}"
    );
    // And the penalty must exceed the +2.0 target-quality lure it counteracts.
    assert!(
        b <= -2.0,
        "penalty must overcome the threat-value bonus, got {b}"
    );
}

#[test]
fn indestructible_target_is_penalized_even_when_damage_exceeds_toughness() {
    // CR 702.12b: 5 damage on an indestructible 1/1 still whiffs.
    let b = bonus_for(&[], Body::new(1).indestructible(), burn(5));
    assert!(
        b < 0.0,
        "indestructible target must read as wasted, got {b}"
    );
}

#[test]
fn non_damage_removal_is_inert() {
    // A Destroy spell carries no damage effect → the term must not perturb its
    // targeting at all.
    let destroy = Effect::Destroy {
        target: TargetFilter::Any,
        cant_regenerate: false,
    };
    assert_eq!(
        pending_for(&[], Body::new(7), destroy.clone()),
        PendingDamage::None
    );
    assert_eq!(bonus_for(&[], Body::new(7), destroy), 0.0);
}

// ─── source-dependent damage results ────────────────────────────────────────

#[test]
fn default_source_deathtouch_reads_as_a_clean_kill_on_an_oversized_body() {
    // CR 120.3 + CR 702.2b + CR 704.5h: a 1-damage deathtouch source (a spell
    // granted deathtouch) kills a 7/7 that ordinary burn only tickles.
    assert_eq!(
        pending_for(&[Keyword::Deathtouch], Body::new(7), burn(1)),
        PendingDamage::Dealt(DamageOutcome {
            marked: 1,
            minus_counters: 0,
            deathtouch: true,
        }),
        "deathtouch must be read from the resolved damage source"
    );
    let b = bonus_for(&[Keyword::Deathtouch], Body::new(7), burn(1));
    assert!(
        (b - LETHAL_BONUS).abs() < 1e-9,
        "deathtouch removal on a 7/7 is a clean kill, got {b}"
    );
    // Discriminating control: the same spell without deathtouch is the whiff.
    assert!(bonus_for(&[], Body::new(7), burn(1)) < 0.0);
}

#[test]
fn wither_kills_an_indestructible_creature_it_drives_to_zero_toughness() {
    // CR 120.3d + CR 702.80a + CR 704.5f: 3 wither damage puts three -1/-1
    // counters on an indestructible 3/3, and 0 toughness is not a destruction.
    assert_eq!(
        pending_for(&[Keyword::Wither], Body::new(3).indestructible(), burn(3)),
        PendingDamage::Dealt(DamageOutcome {
            marked: 0,
            minus_counters: 3,
            deathtouch: false,
        }),
        "wither damage must become -1/-1 counters, not marked damage"
    );
    let b = bonus_for(&[Keyword::Wither], Body::new(3).indestructible(), burn(3));
    assert!(
        (b - LETHAL_BONUS).abs() < 1e-9,
        "wither to 0 toughness kills through indestructible, got {b}"
    );
    // Discriminating control: without wither the same damage is a pure waste.
    assert!(bonus_for(&[], Body::new(3).indestructible(), burn(3)) < 0.0);
}

#[test]
fn infect_kills_an_indestructible_creature_it_drives_to_zero_toughness() {
    // CR 702.90c: infect routes damage to -1/-1 counters exactly as wither does.
    assert_eq!(
        pending_for(&[Keyword::Infect], Body::new(2).indestructible(), burn(2)),
        PendingDamage::Dealt(DamageOutcome {
            marked: 0,
            minus_counters: 2,
            deathtouch: false,
        })
    );
    let b = bonus_for(&[Keyword::Infect], Body::new(2).indestructible(), burn(2));
    assert!(
        (b - LETHAL_BONUS).abs() < 1e-9,
        "infect to 0 toughness kills through indestructible, got {b}"
    );
}

#[test]
fn wither_short_of_toughness_scales_the_waste_by_the_surviving_body() {
    // CR 122.1a: two counters on a 7/7 leave a 7/5 alive, so the waste penalty
    // must be measured against the toughness it kept, not the printed one.
    let b = bonus_for(&[Keyword::Wither], Body::new(7), burn(2));
    let expected = -(5.0_f64 * WASTE_PENALTY_MULT).min(WASTE_PENALTY_MAX);
    assert!(
        (b - expected).abs() < 1e-9,
        "expected {expected} for a 7/7 reduced to 7/5, got {b}"
    );
}

#[test]
fn target_sourced_damage_stays_neutral() {
    // CR 120.3: with `DamageSource::Target` the first object target IS the
    // damage source and is excluded from the recipients, so this object may not
    // be dealt damage at all. Its deathtouch/wither are likewise unknown while
    // targets are still being chosen → stay out of the ranking entirely.
    assert_eq!(
        pending_for(&[], Body::new(3), burn_from(3, Some(DamageSource::Target))),
        PendingDamage::Unresolved,
        "a target-sourced damage effect must not be modelled as a recipient hit"
    );
    assert_eq!(
        bonus_for(&[], Body::new(3), burn_from(3, Some(DamageSource::Target))),
        0.0
    );
    // Discriminating control: the identical amount with the default source IS
    // scored, so the neutrality above comes from the source, not the filter.
    assert!(bonus_for(&[], Body::new(3), burn(3)) > 0.0);
}

#[test]
fn each_target_sourced_damage_stays_neutral() {
    // CR 120.1: every leading target is an independent source with its own
    // keywords and its own re-resolved amount; only `targets.last()` is the
    // recipient.
    assert_eq!(
        pending_for(
            &[],
            Body::new(3),
            burn_from(3, Some(DamageSource::EachTarget))
        ),
        PendingDamage::Unresolved
    );
    assert_eq!(
        bonus_for(
            &[],
            Body::new(3),
            burn_from(3, Some(DamageSource::EachTarget))
        ),
        0.0
    );
}

#[test]
fn triggering_source_damage_stays_neutral() {
    // CR 120.3: the source is the triggering event's object, whose keywords this
    // layer cannot resolve — stay neutral rather than assume a vanilla source.
    assert_eq!(
        pending_for(
            &[],
            Body::new(3),
            burn_from(3, Some(DamageSource::TriggeringSource))
        ),
        PendingDamage::Unresolved
    );
}

/// A snapshot standing in for an already-replaced damage event (CR 120.3), with
/// every source characteristic off — so if `ApplyPostReplacementDamage` were
/// ever modelled instead of bailing, it would look like plain marked damage and
/// the `Unresolved` assertion below would fail loudly.
fn vanilla_damage_snapshot() -> DamageContextSnapshot {
    DamageContextSnapshot {
        source_id: ObjectId(1),
        controller: AI,
        source_is_creature: false,
        has_deathtouch: false,
        has_lifelink: false,
        has_wither: false,
        has_infect: false,
        combat_damage_poison: 0,
        excess_recipient: None,
        lifelink_bonus: 0,
    }
}

#[test]
fn batch_damage_effects_stay_neutral() {
    // CR 120.1: every member of the batch-damage group puts damage on this object
    // from sources this layer does not model per-source. Each must report
    // `Unresolved` — a variant that silently under-counted to zero would look
    // like "no damage reaches this target" and leave the ranking unguarded.
    // Covered as a table so the assertion set can never drift behind the match
    // arm it guards in `pending_damage_to_object`.
    let batch = [
        (
            "EachSourceDealsDamage",
            Effect::EachSourceDealsDamage {
                sources: TargetFilter::Any,
                amount: QuantityExpr::Fixed { value: 3 },
                recipient: EachDamageRecipient::EachController,
            },
        ),
        (
            "EachDealsDamageEqualToPower",
            Effect::EachDealsDamageEqualToPower {
                sources: TargetFilter::Any,
                recipient: TargetFilter::Any,
                extra_source: None,
            },
        ),
        (
            "DamageAll",
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                player_filter: None,
                damage_source: None,
            },
        ),
        (
            "ApplyPostReplacementDamage",
            Effect::ApplyPostReplacementDamage {
                context: vanilla_damage_snapshot(),
                target: TargetRef::Object(ObjectId(1)),
                amount: 3,
                is_combat: false,
            },
        ),
    ];

    for (name, effect) in batch {
        assert_eq!(
            pending_for(&[], Body::new(3), effect.clone()),
            PendingDamage::Unresolved,
            "{name} must report Unresolved rather than under-counting to zero"
        );
        assert_eq!(
            bonus_for(&[], Body::new(3), effect),
            0.0,
            "{name} must leave the target ranking untouched"
        );
    }
}
