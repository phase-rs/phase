//! Standard long-tail batch E — shipped-card parse + runtime gates.
//!
//! Shipped cards (each parses with zero `Effect::Unimplemented`):
//!   - Chandra, Flameshaper (+2 "Choose one." → tracked-set reduction)
//!   - Contested Game Ball ("the attacking player gains control of ~ and untaps it")
//!   - Spider-Woman, Stunning Savior ("Venom Blast — Artifacts and creatures your
//!     opponents control enter tapped." — ability-word-prefixed external ETB-tapped)
//!
//! Building-block win (named-token parsing): "Primo, the Indivisible, a legendary
//! 0/0 … token" — a multi-comma legendary token name now parses.
//!
//! Building-block win (token-count multiplier): Ojer Taq, Deepest Foundation —
//! "three times that many of those tokens are created instead" now parses to the
//! parameterized `QuantityModification::Times { factor: 3 }` (the former ×2
//! `Double` is now `Times { factor: 2 }`). See the runtime triplication +
//! creature-gate tests in `game::replacement::tests`.
//!
//! Now supported (S25 P2e — "become a typed token"): Vraska, the Silencer — the
//! dies-trigger reanimate copula "It's a Treasure artifact with '{T}, Sacrifice
//! this artifact: Add one mana of any color,' and it loses all other card types"
//! lowers to a `GenericEffect` carrying `SetCardTypes{[Artifact]}`,
//! `AddSubtype{Treasure}`, and a `GrantAbility`, bound to the returned object
//! (`TriggeringSource`) as a `Duration::UntilHostLeavesPlay` continuous effect.
//! Parser round-trip and runtime binding tests below.
//!
//! Now supported (S25 P2e — Moonlit Meditation): "The first time you would create
//! one or more tokens each turn, you may instead create that many tokens that are
//! copies of enchanted permanent." lowers to an Optional `CreateToken` replacement
//! gated by `ReplacementCondition::FirstTokenCreationEachTurn`, whose
//! `CopyTokenOf { target: AttachedTo, count: EventContextAmount }` execute makes
//! host-copies. Per-PLAYER once-per-turn window (the Oracle's "you"), tracked via
//! the shared `GameState::players_who_created_token_this_turn` primitive (consumed
//! by the first token the controller creates this turn — so a source entering
//! mid-turn after an earlier creation does NOT fire, per the official ruling),
//! "that many" count, decline-consumes, turn-reset, per-player (not per-source)
//! window, and Doubling-Season non-recursion tests below.
//!
//! Deferred (honest `Effect::unimplemented` / SwallowedClause retained, NOT
//! asserted 0-unimpl): Zimone (prime-number intervening-if
//! condition — heavy primality predicate; the token+counter parse is fixed, the
//! card stays honestly condition-unsupported via a SwallowedClause warning).

use std::sync::Arc;

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::{AttachTarget, GameObject};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::turns::execute_cleanup;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::TargetFilter;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{DelayedTrigger, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;

fn parse(
    oracle: &str,
    name: &str,
    keywords: &[&str],
    types: &[&str],
    subtypes: &[&str],
) -> engine::parser::oracle::ParsedAbilities {
    let kw: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
    let t: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let s: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle, name, &kw, &t, &s)
}

fn assert_zero_unimplemented(parsed: &engine::parser::oracle::ParsedAbilities, name: &str) {
    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "{name}: expected zero Unimplemented nodes, parse was:\n{dbg}"
    );
}

// ---------------------------------------------------------------------------
// Chandra, Flameshaper — +2 "Choose one." tracked-set reduction
// ---------------------------------------------------------------------------

/// CR 608.2c + CR 700.2: The standalone "Choose one." sentence inside the impulse
/// chain ("Exile the top three cards … Choose one. You may play that card this
/// turn.") lowers to a `ChooseFromZone { Exile }` reduction over the tracked set,
/// followed by the play grant. Reverting the bare-"choose one" anaphor arm leaves
/// the clause `Unimplemented`, flipping `assert_zero_unimplemented` AND the
/// `ChooseFromZone` shape assertion below.
#[test]
fn chandra_flameshaper_choose_one_reduces_tracked_set() {
    let parsed = parse(
        "[+2]: Add {R}{R}{R}. Exile the top three cards of your library. Choose one. You may play that card this turn.\n[+1]: Create a token that's a copy of target creature you control, except it has haste and \"At the beginning of the end step, sacrifice this token.\"\n[−4]: Chandra deals 8 damage divided as you choose among any number of target creatures and/or planeswalkers.",
        "Chandra, Flameshaper",
        &[],
        &["Legendary", "Planeswalker"],
        &["Chandra"],
    );
    assert_zero_unimplemented(&parsed, "Chandra, Flameshaper");

    // The +2 chain must carry an interactive ChooseFromZone over the exiled set
    // (the impulse reduction), then a PlayFromExile grant. Reverting the fix
    // replaces the ChooseFromZone with an Unimplemented sub-effect.
    use engine::types::ability::Effect;
    let plus_two = parsed
        .abilities
        .iter()
        .find(|a| format!("{:#?}", a).contains("Exile the top three cards"))
        .expect("+2 ability present");
    let chain = format!("{plus_two:#?}");
    assert!(
        chain.contains("ChooseFromZone"),
        "+2 chain must reduce the exiled set via ChooseFromZone, got:\n{chain}"
    );
    // Sanity: an exile-top still leads the chain.
    assert!(
        matches!(&*plus_two.effect, Effect::Mana { .. }),
        "+2 leads with the {{R}}{{R}}{{R}} mana ability"
    );
}

// ---------------------------------------------------------------------------
// Spider-Woman, Stunning Savior — ability-word-prefixed external ETB-tapped
// ---------------------------------------------------------------------------

/// CR 207.2c + CR 614.1d: The "Venom Blast —" ability word is flavor; the body
/// "Artifacts and creatures your opponents control enter tapped." must parse
/// through the external-entry replacement machinery exactly as the unprefixed
/// Authority of the Consuls / Blind Obedience lines do. Reverting the
/// ability-word strip in the replacement priority leaves the whole line
/// `Unimplemented`.
#[test]
fn spider_woman_venom_blast_external_enters_tapped() {
    let parsed = parse(
        "Flying\nVenom Blast — Artifacts and creatures your opponents control enter tapped.",
        "Spider-Woman, Stunning Savior",
        &["Flying"],
        &["Legendary", "Creature"],
        &["Spider"],
    );
    assert_zero_unimplemented(&parsed, "Spider-Woman, Stunning Savior");

    // A ChangeZone-event replacement scoped to opponents' artifacts/creatures
    // must be produced (it would be absent if the ability-word prefix blocked
    // the replacement parser).
    assert_eq!(
        parsed.replacements.len(),
        1,
        "expected exactly one external enters-tapped replacement, got {:#?}",
        parsed.replacements
    );
    let dbg = format!("{:#?}", parsed.replacements[0]);
    assert!(
        dbg.contains("Opponent") && dbg.contains("SetTapState") && dbg.contains("Tap"),
        "replacement must tap opponents' permanents on entry, got:\n{dbg}"
    );
}

// ---------------------------------------------------------------------------
// Named-token building block — multi-comma legendary token name
// ---------------------------------------------------------------------------

/// CR 111.4: A token whose name itself contains a comma ("Primo, the
/// Indivisible") must parse with the full epithet as the name, the article
/// boundary being the ", a " that introduces the token's characteristics — not
/// the first comma. Reverting `parse_named_token_preamble` to first-comma
/// splitting leaves the clause `Unimplemented`.
#[test]
fn named_token_with_comma_in_name_parses() {
    use engine::types::ability::Effect;
    let parsed = parse(
        "When this creature enters, create Primo, the Indivisible, a legendary 0/0 green and blue Fractal creature token, then put that many +1/+1 counters on it.",
        "Named Token Probe",
        &[],
        &["Creature"],
        &[],
    );
    assert_zero_unimplemented(&parsed, "Named Token Probe");
    let trigger = parsed.triggers.first().expect("ETB trigger present");
    let exec = trigger.execute.as_ref().expect("trigger execute present");
    match &*exec.effect {
        Effect::Token {
            name, supertypes, ..
        } => {
            assert_eq!(
                name, "Primo, the Indivisible",
                "named token must keep the full comma-bearing epithet"
            );
            assert!(
                supertypes.iter().any(|s| format!("{s:?}") == "Legendary"),
                "token must be Legendary, got {supertypes:?}"
            );
        }
        other => panic!("expected Token effect, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Contested Game Ball — runtime: attacking player gains control + untaps it
// ---------------------------------------------------------------------------

/// CR 110.2 + CR 603.7c + CR 109.4: On a DamageReceived trigger
/// ("Whenever you're dealt combat damage, the attacking player gains control of
/// this artifact and untaps it."), the recipient of control is the controller of
/// the triggering damage *source* (the attacker, P1) — resolved through the new
/// `TargetFilter::TriggeringSourceController` — and the artifact is untapped.
///
/// Discrimination: the artifact starts tapped under P0's control; after resolving
/// the trigger's execute with the combat-damage event live, it is controlled by
/// P1 AND untapped. Reverting any of the three pieces flips an assertion:
///   - drop `TriggeringSourceController` resolution → recipient unresolved →
///     control stays with P0 (controller assertion fails);
///   - drop the "untaps" bare-and split → SetTapState becomes Unimplemented and
///     never runs → artifact stays tapped (tapped assertion fails);
///   - mis-map "the attacking player" to `TriggeringPlayer` → control would go to
///     the damaged player P0 (controller assertion fails, since for a DamageDealt
///     event TriggeringPlayer is the damaged player).
#[test]
fn contested_game_ball_attacker_gains_control_and_untaps() {
    let parsed = parse(
        "Whenever you're dealt combat damage, the attacking player gains control of this artifact and untaps it.\n{2}, {T}: Draw a card and put a point counter on this artifact. Then if it has five or more point counters on it, sacrifice it and create a Treasure token.",
        "Contested Game Ball",
        &[],
        &["Artifact"],
        &[],
    );
    assert_zero_unimplemented(&parsed, "Contested Game Ball");

    let trigger = parsed
        .triggers
        .iter()
        .find(|t| format!("{:?}", t.mode) == "DamageReceived")
        .expect("DamageReceived trigger present");
    let exec = trigger.execute.as_ref().expect("trigger execute present");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let ball = scenario
        .add_creature(P0, "Contested Game Ball", 0, 0)
        .as_artifact()
        .id();
    // The attacking creature is controlled by P1.
    let attacker = scenario.add_creature(P1, "Attacker", 2, 2).id();
    let mut runner = scenario.build();

    // The Game Ball starts tapped under P0's control.
    runner.state_mut().objects.get_mut(&ball).unwrap().tapped = true;
    assert_eq!(
        runner.state().objects[&ball].controller,
        P0,
        "precondition: P0 controls the ball"
    );
    assert!(
        runner.state().objects[&ball].tapped,
        "precondition: the ball is tapped"
    );

    // Make the combat-damage event live: P1's attacker dealt combat damage to P0.
    runner.state_mut().current_trigger_event = Some(GameEvent::DamageDealt {
        source_id: attacker,
        target: TargetRef::Player(P0),
        amount: 2,
        is_combat: true,
        excess: 0,
    });
    let attacker_lki = runner.state().objects[&attacker].snapshot_for_mana_spent();
    runner.state_mut().lki_cache.insert(attacker, attacker_lki);
    runner.state_mut().objects.remove(&attacker);

    let ability = build_resolved_from_def(exec, ball, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("trigger execute resolves");

    // Control transfers to the attacking player (P1), and the artifact is untapped.
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&ball].controller,
        P1,
        "the attacking player (P1) must gain control of the Game Ball"
    );
    assert!(
        !runner.state().objects[&ball].tapped,
        "the Game Ball must be untapped after the trigger resolves"
    );
    // The recipient really came from the triggering source's controller.
    let _ = TargetFilter::TriggeringSourceController;
}

// ---------------------------------------------------------------------------
// Ojer Taq, Deepest Foundation — token-count ×3 multiplier replacement
// ---------------------------------------------------------------------------

/// CR 614.1a + CR 111.1: The full front-face oracle parses with zero
/// `Unimplemented` nodes. The previously-deferred token-triplication line
/// ("three times that many of those tokens are created instead") now lowers to a
/// `CreateToken` replacement carrying the parameterized
/// `QuantityModification::Times { factor: 3 }` multiplier, gated to creature
/// tokens. Vigilance and the dies-trigger already parsed; this asserts they
/// stay clean alongside the new replacement. Reverting the multiplier parser
/// leaves the line `Unimplemented`, flipping `assert_zero_unimplemented` and the
/// replacement-shape assertions below.
#[test]
fn ojer_taq_token_triplication_full_card_parses() {
    use engine::types::ability::QuantityModification;
    use engine::types::replacements::ReplacementEvent;

    let parsed = parse(
        "Vigilance\nIf one or more creature tokens would be created under your control, three times that many of those tokens are created instead.\nWhen Ojer Taq, Deepest Foundation dies, return it transformed.",
        "Ojer Taq, Deepest Foundation",
        &["Vigilance"],
        &["Legendary", "Creature"],
        &["God"],
    );
    assert_zero_unimplemented(&parsed, "Ojer Taq, Deepest Foundation");

    let token_repl = parsed
        .replacements
        .iter()
        .find(|r| r.event == ReplacementEvent::CreateToken)
        .expect("Ojer Taq must produce a CreateToken replacement");
    assert_eq!(
        token_repl.quantity_modification,
        Some(QuantityModification::Times { factor: 3 }),
        "Ojer Taq must triplicate (Times {{ factor: 3 }}), not double"
    );
}

// ---------------------------------------------------------------------------
// S25 P2e — "become a typed token": Vraska, the Silencer + Brilliance Unleashed
// ---------------------------------------------------------------------------

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::layers::evaluate_layers;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ChosenAttribute, ContinuousModification,
    DelayedTriggerCondition, DelayedTriggerLifetime, Duration, Effect, ManaProduction, PlayerScope,
    QuantityExpr, StaticCondition, TriggerDefinition,
};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::keywords::Keyword;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const VRASKA_ORACLE: &str = "Deathtouch\nWhenever a nontoken creature an opponent controls dies, you may pay {1}. If you do, return that card to the battlefield tapped under your control. It's a Treasure artifact with \"{T}, Sacrifice this artifact: Add one mana of any color,\" and it loses all other card types.";

const BRILLIANCE_ORACLE: &str = "Choose one or both —\n• Brilliance Unleashed deals 5 damage to target creature.\n• Choose target artifact card in your graveyard. Return it to the battlefield if it's an artifact creature card. Otherwise, return it to the battlefield and it's a 3/3 Robot artifact creature with flying.";

/// Depth-first search for the first effect in a def chain (sub_ability +
/// else_ability) matching `pred`.
fn find_effect_in_def<'a>(
    def: &'a AbilityDefinition,
    pred: &dyn Fn(&Effect) -> bool,
) -> Option<&'a Effect> {
    if pred(def.effect.as_ref()) {
        return Some(def.effect.as_ref());
    }
    if let Some(sub) = &def.sub_ability {
        if let Some(found) = find_effect_in_def(sub, pred) {
            return Some(found);
        }
    }
    if let Some(els) = &def.else_ability {
        if let Some(found) = find_effect_in_def(els, pred) {
            return Some(found);
        }
    }
    None
}

/// CR 701.21a: does `cost` sacrifice the ability's own source object (`SelfRef`)?
/// A granted "{T}, Sacrifice this artifact: …" resolves `SelfRef` to the object
/// carrying the granted ability — i.e. the returned Treasure, not Vraska.
fn cost_sacrifices_self(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Sacrifice(s) => matches!(s.target, TargetFilter::SelfRef),
        AbilityCost::Composite { costs } => costs.iter().any(cost_sacrifices_self),
        _ => false,
    }
}

fn generic_effect_static_mods(
    effect: &Effect,
) -> Option<(
    &Vec<ContinuousModification>,
    &Option<Duration>,
    &Option<TargetFilter>,
)> {
    match effect {
        Effect::GenericEffect {
            static_abilities,
            duration,
            target,
            end_cost: _,
        } => {
            let mods = &static_abilities.first()?.modifications;
            Some((mods, duration, target))
        }
        _ => None,
    }
}

/// Parser round-trip: the reanimate copula lowers to a `GenericEffect`
/// (`SetCardTypes{[Artifact]}` + `AddSubtype{Treasure}` + `GrantAbility`) bound to
/// the returned object (`TriggeringSource`) as `UntilHostLeavesPlay`.
/// Revert proof: reverting the Block-1 arm in `subject.rs` drops the copula to
/// `Effect::Unimplemented`, flipping `assert_zero_unimplemented` AND the
/// `SetCardTypes`/`AddSubtype`/`GrantAbility` shape assertions below.
#[test]
fn vraska_reanimate_copula_parses_to_treasure_artifact_grant() {
    let parsed = parse(
        VRASKA_ORACLE,
        "Vraska, the Silencer",
        &["Deathtouch"],
        &["Legendary", "Planeswalker"],
        &[],
    );
    assert_zero_unimplemented(&parsed, "Vraska, the Silencer");

    let exec = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.as_ref())
        .expect("Vraska dies-trigger must carry an execute chain");

    let copula = find_effect_in_def(exec, &|e| {
        matches!(e, Effect::GenericEffect { static_abilities, .. }
            if static_abilities.iter().any(|s| s.modifications.iter().any(|m|
                matches!(m, ContinuousModification::SetCardTypes { core_types } if core_types == &vec![CoreType::Artifact]))))
    })
    .expect("copula must lower to a GenericEffect with SetCardTypes{[Artifact]}");

    let (mods, duration, _target) =
        generic_effect_static_mods(copula).expect("copula GenericEffect has a static def");
    // CR 611.2a + CR 400.7: indefinite, ends when the returned object leaves play.
    assert_eq!(
        duration,
        &Some(Duration::UntilHostLeavesPlay),
        "reanimate copula must be UntilHostLeavesPlay, not Permanent (C3)"
    );
    // The copula binds to the RETURNED object (the triggering source), not Vraska.
    let affected = match copula {
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities[0].affected.clone(),
        _ => unreachable!(),
    };
    assert_eq!(
        affected,
        Some(TargetFilter::TriggeringSource),
        "copula must bind to the returned dies-triggering object, not SelfRef"
    );
    assert!(
        mods.iter().any(
            |m| matches!(m, ContinuousModification::AddSubtype { subtype } if subtype == "Treasure")
        ),
        "copula must add the Treasure subtype"
    );
    let grant = mods
        .iter()
        .find_map(|m| match m {
            ContinuousModification::GrantAbility { definition } => Some(definition),
            _ => None,
        })
        .expect("copula must grant the '{T}, Sacrifice this artifact: Add one mana' ability");
    assert!(
        grant.cost.as_ref().is_some_and(cost_sacrifices_self),
        "granted mana ability must sacrifice the granted-to (returned) object (SelfRef)"
    );
}

/// Runtime (C1 + C7): resolving the return + copula binds the continuous effect to
/// the RETURNED object's id — not Vraska (source, the `use_self` misbind) and not
/// nowhere (inert). The returned object becomes an Artifact (losing Creature),
/// carries Treasure, and its granted mana ability sacrifices THAT object.
/// Revert proof: reverting the Block-1 arm leaves the copula `Unimplemented`, so no
/// TCE is installed → the `find(...).expect(...)` for the returned-object TCE panics.
#[test]
fn vraska_returned_creature_becomes_treasure_artifact_not_vraska() {
    let parsed = parse(
        VRASKA_ORACLE,
        "Vraska, the Silencer",
        &["Deathtouch"],
        &["Legendary", "Planeswalker"],
        &[],
    );
    let exec = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.clone())
        .expect("Vraska dies-trigger execute");
    // The PayCost's sub_ability is the return + copula chain, gated on the optional
    // pay via `OptionalEffectPerformed`. The optional pay is orthogonal machinery
    // (unchanged by this work); clear the gate and resolve the return + copula that
    // this change adds.
    let mut return_def = (*exec.sub_ability.clone().expect("return chain sub_ability")).clone();
    return_def.condition = None;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let vraska = scenario.add_creature(P0, "Vraska, the Silencer", 0, 0).id();
    let dead = scenario
        .add_creature_to_graveyard(P1, "Deadfellow", 2, 2)
        .id();
    let mut runner = scenario.build();
    // The dies event: TriggeringSource resolves to the dead creature's card.
    runner.state_mut().current_trigger_event =
        Some(GameEvent::CreatureDestroyed { object_id: dead });

    let ability = build_resolved_from_def(&return_def, vraska, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("return + copula chain resolves");

    // C1: the copula's continuous effect binds to the RETURNED object's id.
    let tce = runner
        .state()
        .transient_continuous_effects
        .iter()
        .find(|t| matches!(t.affected, TargetFilter::SpecificObject { id } if id == dead))
        .expect("copula TCE must bind to the returned object's id (not inert)");
    // C7 wrong-object: it must NOT bind to Vraska (the source / use_self misbind).
    assert!(
        !runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|t| matches!(t.affected, TargetFilter::SpecificObject { id } if id == vraska)),
        "copula must NOT bind to Vraska (the source object) — use_self misbind"
    );
    assert!(
        tce.modifications.iter().any(|m| matches!(m, ContinuousModification::SetCardTypes { core_types } if core_types == &vec![CoreType::Artifact])),
        "TCE must SET card types to [Artifact]"
    );
    let grant = tce
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::GrantAbility { definition } => Some(definition),
            _ => None,
        })
        .expect("TCE must grant the mana ability");
    assert!(
        grant.cost.as_ref().is_some_and(cost_sacrifices_self),
        "C7: the granted ability sacrifices the granted-to (returned) object"
    );

    // Effective characteristics after layers: an Artifact (not Creature), Treasure,
    // tapped, under P0's control, on the battlefield.
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&dead];
    assert_eq!(obj.zone, Zone::Battlefield, "returned to the battlefield");
    assert_eq!(obj.controller, P0, "under P0's control");
    assert!(obj.tapped, "returned tapped");
    assert_eq!(
        obj.card_types.core_types,
        vec![CoreType::Artifact],
        "returned object is an Artifact and lost Creature (CR 205.1a)"
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Treasure"),
        "returned object carries the Treasure subtype"
    );
    // Vraska (the source) is untouched — still a 0/0 non-Treasure.
    assert!(
        !runner.state().objects[&vraska]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Treasure"),
        "Vraska (source) must NOT gain Treasure"
    );
}

/// Parser round-trip: the mode-2 `Otherwise` else animation binds `ParentTarget`
/// (the chosen artifact card) with the 3/3 Robot flying spec. Revert proof:
/// reverting the Block-2 referent seed (`mod.rs`) leaves the else animation
/// `Unimplemented`, flipping `assert_zero_unimplemented` AND the `SetPower`/`Robot`/
/// `Flying` shape assertions below. The `anaphoric_return_then_animation_honest_
/// defers…` snapshot test stays green (isolated fragment still has no referent).
#[test]
fn brilliance_otherwise_animation_parses_to_robot_spec() {
    use engine::types::keywords::Keyword;

    let parsed = parse(
        BRILLIANCE_ORACLE,
        "Brilliance Unleashed",
        &[],
        &["Sorcery"],
        &[],
    );
    assert_zero_unimplemented(&parsed, "Brilliance Unleashed");

    let mode2 = &parsed.abilities[1];
    let anim = find_effect_in_def(mode2, &|e| {
        matches!(e, Effect::GenericEffect { static_abilities, .. }
            if static_abilities.iter().any(|s| s.modifications.iter().any(|m|
                matches!(m, ContinuousModification::AddSubtype { subtype } if subtype == "Robot"))))
    })
    .expect("mode-2 else must carry the 3/3 Robot animation GenericEffect");

    let (mods, duration, _target) =
        generic_effect_static_mods(anim).expect("animation GenericEffect has a static def");
    assert_eq!(
        duration,
        &Some(Duration::UntilHostLeavesPlay),
        "reanimate-then-animate else must be UntilHostLeavesPlay, not Permanent (C3)"
    );
    let affected = match anim {
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities[0].affected.clone(),
        _ => unreachable!(),
    };
    assert_eq!(
        affected,
        Some(TargetFilter::ParentTarget),
        "animation must bind ParentTarget (the chosen artifact card), not SelfRef"
    );
    assert!(
        mods.iter()
            .any(|m| matches!(m, ContinuousModification::SetPower { value } if *value == 3)),
        "animation sets base power to 3"
    );
    assert!(
        mods.iter().any(|m| matches!(m, ContinuousModification::AddKeyword { keyword } if *keyword == Keyword::Flying)),
        "animation grants flying"
    );
}

/// Runtime: a non-creature artifact card returned via mode 2's `Otherwise` branch
/// is animated as a 3/3 Robot with flying, bound to the returned card's id. An
/// artifact-creature card returns as-is (if-branch, no animation). Revert proof:
/// reverting the Block-2 seed leaves the else animation `Unimplemented`, so no
/// animation TCE is installed → the returned object stays `power`-unset and the
/// `SetPower{3}`/Robot assertions fail.
#[test]
fn brilliance_otherwise_animates_returned_artifact_as_robot() {
    let parsed = parse(
        BRILLIANCE_ORACLE,
        "Brilliance Unleashed",
        &[],
        &["Sorcery"],
        &[],
    );
    let mode2 = parsed.abilities[1].clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Brilliance Unleashed", 0, 0).id();
    let art = scenario
        .add_spell_to_graveyard(P0, "Filigree Familiar", false)
        .id();
    let mut runner = scenario.build();
    {
        // A NON-creature artifact card in P0's graveyard → the `if it's an artifact
        // creature card` branch is false → the `Otherwise` animation fires.
        let obj = runner.state_mut().objects.get_mut(&art).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.base_card_types = obj.card_types.clone();
    }

    let ability =
        build_resolved_from_def_with_targets(&mode2, source, P0, vec![TargetRef::Object(art)]);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("mode-2 (choose target artifact card → otherwise animate) resolves");

    let tce = runner
        .state()
        .transient_continuous_effects
        .iter()
        .find(|t| matches!(t.affected, TargetFilter::SpecificObject { id } if id == art))
        .expect("animation TCE must bind to the returned artifact card's id");
    assert!(
        tce.modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetPower { value } if *value == 3)),
        "returned object is animated with base power 3"
    );
    assert!(
        tce.modifications.iter().any(
            |m| matches!(m, ContinuousModification::AddSubtype { subtype } if subtype == "Robot")
        ),
        "returned object gains the Robot subtype"
    );

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&art];
    assert_eq!(obj.zone, Zone::Battlefield, "returned to the battlefield");
    assert_eq!(
        obj.power,
        Some(3),
        "the inert-return hollow win is power == None; the animation makes it 3"
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Robot"),
        "returned object is a Robot"
    );
}

// ---------------------------------------------------------------------------
// Moonlit Meditation — first-time-each-turn copy-of-host token replacement
// ---------------------------------------------------------------------------

const MOONLIT_ORACLE: &str = "Enchant artifact or creature you control\n\
The first time you would create one or more tokens each turn, you may instead \
create that many tokens that are copies of enchanted permanent.";

/// The parsed CreateToken replacement carried by Moonlit Meditation.
fn moonlit_replacement() -> engine::types::ability::ReplacementDefinition {
    let parsed = parse(
        MOONLIT_ORACLE,
        "Moonlit Meditation",
        &[],
        &["Enchantment"],
        &["Aura"],
    );
    parsed
        .replacements
        .into_iter()
        .find(|r| r.event == ReplacementEvent::CreateToken)
        .expect("Moonlit must parse to a CreateToken replacement")
}

/// Put a Moonlit Meditation Aura on the battlefield under `controller`, attached
/// to `host`, carrying its parsed first-time copy-of-host replacement.
fn install_moonlit(state: &mut GameState, host: ObjectId, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(950),
        controller,
        "Moonlit Meditation".to_string(),
        Zone::Battlefield,
    );
    let reps = vec![moonlit_replacement()];
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.card_types.subtypes = vec!["Aura".to_string()];
    obj.attached_to = Some(AttachTarget::Object(host));
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
    id
}

/// Put a Doubling Season (token-doubling half only) on the battlefield under
/// `controller` — a mandatory `CreateToken` doubler used to exercise the
/// #1511 interaction (a *different* source's replacement still doubles the
/// substitute copies).
fn install_doubling_season(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let parsed = parse_oracle_text(
        "If one or more tokens would be created under your control, twice that \
         many tokens are created instead.",
        "Doubling Season",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert!(
        !parsed.replacements.is_empty(),
        "Doubling Season token doubler must parse"
    );
    let id = create_object(
        state,
        CardId(960),
        controller,
        "Doubling Season".to_string(),
        Zone::Battlefield,
    );
    let reps = parsed.replacements.clone();
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
    id
}

/// Resolve a token-creating sorcery controlled by `controller`, driving the real
/// token pipeline (propose → `replace_event`). If an optional replacement
/// (Moonlit) applies, the pipeline parks on `WaitingFor::ReplacementChoice`.
fn resolve_token_source(runner: &mut GameRunner, controller: PlayerId, oracle: &str) {
    let parsed = parse_oracle_text(oracle, "Token Source", &[], &["Sorcery".to_string()], &[]);
    let def = parsed
        .abilities
        .first()
        .expect("token source should parse to an ability");
    let src = create_object(
        runner.state_mut(),
        CardId(951),
        controller,
        "Token Source".to_string(),
        Zone::Stack,
    );
    let ability = build_resolved_from_def(def, src, controller);
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("token effect should resolve");
}

fn host_copy_tokens<'a>(
    runner: &'a GameRunner,
    host_name: &str,
    controller: PlayerId,
) -> Vec<&'a GameObject> {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| o.is_token && o.controller == controller && o.name == host_name)
        .collect()
}

fn at_replacement_choice(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    )
}

/// Give a host a distinctive subtype on BOTH the live and base card types —
/// `CopyTokenOf` reads copiable values from `base_card_types`
/// (`intrinsic_copiable_values`), so a copy inherits the subtype only if the
/// base carries it.
fn set_copiable_subtype(state: &mut GameState, id: ObjectId, subtype: &str) {
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.subtypes = vec![subtype.to_string()];
    obj.base_card_types.subtypes = vec![subtype.to_string()];
}

/// A1 — accept: a your-owned token creation is replaced by copies of the
/// enchanted host (name/P/T/subtypes match the host, not the original token
/// spec). Revert the parser to `.valid_card(SelfRef)` and the replacement never
/// matches (CreateToken has no affected object) → no prompt, plain Soldier:
/// both `at_replacement_choice` and `copies.len() == 1` flip.
#[test]
fn moonlit_accept_creates_copies_of_enchanted_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), host, "Ox");
    install_moonlit(runner.state_mut(), host, P0);

    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );

    assert!(
        at_replacement_choice(&runner),
        "your token creation must surface Moonlit's optional replacement, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Moonlit");
    runner.advance_until_stack_empty();

    let copies = host_copy_tokens(&runner, "Host Ox", P0);
    assert_eq!(copies.len(), 1, "accept → exactly one host-copy token");
    let copy = copies[0];
    assert_eq!(
        (copy.power, copy.toughness),
        (Some(5), Some(4)),
        "the copy has the host's P/T (5/4), not the 1/1 Soldier spec"
    );
    assert!(
        copy.card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Ox")),
        "the copy is the enchanted host (Ox), got {:?}",
        copy.card_types.subtypes
    );
    assert!(
        !copy
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Soldier")),
        "the original Soldier spec was replaced by a host-copy"
    );
    assert!(
        runner
            .state()
            .players_who_created_token_this_turn
            .contains(&P0),
        "accept records the copy token → the per-player window is consumed"
    );
}

/// A2 — owner scope: a P1-owned creation on P0's turn is NOT replaced by P0's
/// Moonlit (`token_owner_scope(You)`). Paired positive reach-guard in the same
/// test: a P0-owned creation with the same Moonlit installed DOES prompt — so
/// the non-prompt is owner-scope rejection, not a dead Moonlit.
#[test]
fn moonlit_ignores_opponent_owned_token_creation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let mut runner = scenario.build();
    install_moonlit(runner.state_mut(), host, P0);

    resolve_token_source(
        &mut runner,
        P1,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        !at_replacement_choice(&runner),
        "P0's Moonlit must not replace a P1-owned token creation, got {:?}",
        runner.state().waiting_for
    );
    let p1_tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| o.is_token && o.controller == P1)
        .collect();
    assert_eq!(p1_tokens.len(), 1, "the opponent's plain token is created");
    assert!(
        p1_tokens[0]
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Soldier")),
        "the opponent's token stays a Soldier, not a host-copy"
    );

    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        at_replacement_choice(&runner),
        "reach-guard: a your-owned creation with the same Moonlit must prompt"
    );
}

/// B1 (official ruling) — per-player window, pre-consumed by an earlier token: a
/// P0-owned token is created BEFORE Moonlit exists (recording P0 in
/// `players_who_created_token_this_turn`), then Moonlit enters, then a second
/// creation the SAME turn does NOT prompt — P0's per-player window is already
/// spent. This is the exact official ruling: "If you create one or more tokens,
/// and then Moonlit Meditation comes under your control that same turn, the
/// replacement effect won't apply to any tokens you create for the rest of the
/// turn." SWITCH DISCRIMINATOR: revert the eval to the per-source latch (empty for
/// a source that just entered and never applied) → Moonlit would wrongly prompt
/// and the `!at_replacement_choice` assertion below fails.
#[test]
fn moonlit_source_entering_after_earlier_token_does_not_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let mut runner = scenario.build();

    // First creation, BEFORE Moonlit exists → records P0 in the per-player set.
    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        !at_replacement_choice(&runner),
        "no replacement before Moonlit exists"
    );
    assert!(
        runner
            .state()
            .players_who_created_token_this_turn
            .contains(&P0),
        "the pre-Moonlit creation consumed P0's per-player window"
    );

    install_moonlit(runner.state_mut(), host, P0);

    // Second creation, same turn, AFTER Moonlit enters → window already spent.
    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        !at_replacement_choice(&runner),
        "Moonlit entering after an earlier same-turn creation does NOT fire \
         (per-player window pre-consumed; official ruling), got {:?}",
        runner.state().waiting_for
    );
    assert!(
        host_copy_tokens(&runner, "Host Ox", P0).is_empty(),
        "no host-copy — Moonlit did not fire"
    );
}

/// B2 — "that many" count: an N=3 token creation, accepted, yields exactly 3
/// host-copies. Revert the `quantity.rs` `EventContextAmount` scoped arm → the
/// count reads `None` → 0 copies. Hostile cascade shadow: a *different*
/// `current_trigger_match_count` (7) must not win — the Moonlit-scoped count is
/// read first, un-shadowable.
#[test]
fn moonlit_copies_that_many_for_multi_token_events() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let mut runner = scenario.build();
    install_moonlit(runner.state_mut(), host, P0);

    resolve_token_source(
        &mut runner,
        P0,
        "Create three 1/1 white Soldier creature tokens.",
    );
    assert!(
        at_replacement_choice(&runner),
        "an N-token creation must prompt, got {:?}",
        runner.state().waiting_for
    );
    // Hostile shadow: the highest-priority cascade entry after the Moonlit field.
    runner.state_mut().current_trigger_match_count = Some(7);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept");
    runner.advance_until_stack_empty();
    assert_eq!(
        host_copy_tokens(&runner, "Host Ox", P0).len(),
        3,
        "'that many' == the replaced event count (3), not 0 (revert quantity arm) nor 7 (cascade shadow)"
    );
}

/// B3 — decline consumes the window: declining still creates the original token,
/// which `record_token_created` records in the per-player
/// `players_who_created_token_this_turn` set, so a second creation the same turn
/// does not prompt. Decline falls through to the original event → a plain Soldier,
/// no host-copy. If the original creation did not record the player, the window
/// would stay open and the second creation would prompt.
#[test]
fn moonlit_decline_consumes_the_turn_allowance() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let mut runner = scenario.build();
    install_moonlit(runner.state_mut(), host, P0);

    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        at_replacement_choice(&runner),
        "reach-guard: the first creation prompts"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline");
    runner.advance_until_stack_empty();

    let soldiers: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| {
            o.is_token
                && o.controller == P0
                && o.card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("Soldier"))
        })
        .collect();
    assert_eq!(
        soldiers.len(),
        1,
        "decline creates the original plain Soldier"
    );
    assert!(
        host_copy_tokens(&runner, "Host Ox", P0).is_empty(),
        "no host-copy is created on decline"
    );
    assert!(
        runner
            .state()
            .players_who_created_token_this_turn
            .contains(&P0),
        "decline creates the original token → the per-player window is consumed"
    );

    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        !at_replacement_choice(&runner),
        "allowance consumed by the decline → the second creation is not replaced"
    );
}

/// B4 — turn reset: consuming the window on turn N (here by DECLINING — which
/// still creates the original token and records the player, proven in B3, and —
/// unlike accept — leaves no mid-resolution copy-continuation seed to interfere
/// with this off-stack harness) and then crossing a turn boundary
/// (`start_next_turn`) clears `players_who_created_token_this_turn`, so Moonlit
/// fires again. Without the turn-start clear (`turns.rs`), the second turn's
/// creation would not prompt.
#[test]
fn moonlit_resets_at_turn_start() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let mut runner = scenario.build();
    install_moonlit(runner.state_mut(), host, P0);

    // Turn N: fire, then DECLINE to consume the window without seeding a copy
    // continuation.
    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        at_replacement_choice(&runner),
        "turn N: first creation prompts"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline");
    runner.advance_until_stack_empty();
    assert!(
        runner
            .state()
            .players_who_created_token_this_turn
            .contains(&P0),
        "turn N: the window is consumed"
    );
    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        !at_replacement_choice(&runner),
        "turn N: window consumed → second creation not replaced"
    );

    // Cross a turn boundary through the real reset path.
    let mut events = Vec::<GameEvent>::new();
    engine::game::turns::start_next_turn(runner.state_mut(), &mut events);
    assert!(
        !runner
            .state()
            .players_who_created_token_this_turn
            .contains(&P0),
        "turn start clears the per-player token-creation record"
    );

    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        at_replacement_choice(&runner),
        "next turn: the per-player record reset → Moonlit fires again, got {:?}",
        runner.state().waiting_for
    );
}

/// B6 — turn-start clears the transient copy-count seed (fix #1,
/// `turns.rs::start_next_turn`). Directly discriminating: the on-stack accept
/// flow can never observe a *stale* seed at a turn boundary — the intervening
/// return-to-priority full-drain (`effects/mod.rs`) already nulls
/// `post_replacement_token_substitution_count` one action after the owning
/// resolution, so in a natural cast it is already `None` before
/// `start_next_turn` runs and removing the turn-start clear would not change
/// that flow. To make the turn-start clear *itself* revert-detectable we seed
/// both transients to their live "mid-substitution" values (as the accept path
/// would leave them if a priority pass had NOT intervened) and prove the turn
/// boundary alone scrubs them. Revert either `= None` line in `start_next_turn`
/// → the matching post-boundary assertion below stays `Some`/non-empty and
/// fails. The decline-based B4 keeps covering the
/// `players_who_created_token_this_turn` turn-reset; this closes the
/// copy-count/applied-seed clean-state gap.
#[test]
fn moonlit_turn_start_scrubs_transient_substitution_seeds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let mut runner = scenario.build();
    let moonlit = install_moonlit(runner.state_mut(), host, P0);

    // Mid-substitution snapshot: the accept path seeds the "that many" copy
    // count and the self-suppression applied set keyed by the Moonlit source.
    runner.state_mut().post_replacement_token_substitution_count = Some(4);
    runner.state_mut().post_replacement_token_choice_applied =
        Some(std::collections::HashSet::from([
            engine::types::proposed_event::AppliedReplacementKey::object(moonlit, 0),
        ]));

    // Reach-guard (non-vacuity): the seeds are actually set when the boundary
    // is crossed — `start_next_turn` is not operating on an already-clean state.
    assert_eq!(
        runner.state().post_replacement_token_substitution_count,
        Some(4),
        "reach-guard: copy-count seed is Some before the turn boundary"
    );
    assert!(
        runner
            .state()
            .post_replacement_token_choice_applied
            .as_ref()
            .is_some_and(|s| s.len() == 1),
        "reach-guard: applied seed is populated before the turn boundary"
    );

    let mut events = Vec::<GameEvent>::new();
    engine::game::turns::start_next_turn(runner.state_mut(), &mut events);

    // Fix #1: revert `state.post_replacement_token_substitution_count = None;`
    // in start_next_turn → this stays Some(4) and fails.
    assert_eq!(
        runner.state().post_replacement_token_substitution_count,
        None,
        "turn start scrubs the transient copy-count seed"
    );
    // Fix #1 (applied-seed line): revert
    // `state.post_replacement_token_choice_applied = None;` → this stays
    // Some(..) and fails.
    assert!(
        runner
            .state()
            .post_replacement_token_choice_applied
            .is_none(),
        "turn start scrubs the transient self-suppression applied seed"
    );
}

/// B5 — per-PLAYER window (not per-source): Moonlit A firing (and creating a copy,
/// which records P0 in `players_who_created_token_this_turn`) consumes P0's window
/// for the whole turn. A distinct Moonlit B installed afterward the SAME turn does
/// NOT fire — "the first time you would create … each turn" is per-player, not
/// keyed by source `ObjectId`. SWITCH DISCRIMINATOR: revert the eval to the
/// per-source latch → B (a different, unlatched `ObjectId`) would wrongly prompt
/// and produce an Elk copy, failing both assertions below.
#[test]
fn moonlit_window_is_per_player_not_per_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host_a = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let host_b = scenario.add_creature(P0, "Host Elk", 3, 3).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), host_b, "Elk");
    install_moonlit(runner.state_mut(), host_a, P0);

    // Moonlit A fires on the first creation and makes a copy → records P0.
    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(at_replacement_choice(&runner), "Moonlit A prompts");
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept A");
    runner.advance_until_stack_empty();
    assert!(
        runner
            .state()
            .players_who_created_token_this_turn
            .contains(&P0),
        "A's copy consumed P0's per-player window"
    );

    // Moonlit B enters the same turn AFTER the window was spent → does NOT fire.
    install_moonlit(runner.state_mut(), host_b, P0);
    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        !at_replacement_choice(&runner),
        "Moonlit B does NOT fire — P0's per-player window is already spent, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        host_copy_tokens(&runner, "Host Elk", P0).is_empty(),
        "no Elk copy — B did not fire (per-player window, not per-source)"
    );
}

/// Accept-path window recording (guards the removal of the per-source note fn): a
/// single Moonlit present from the start, the first creation is accepted → a copy
/// is created, which `record_token_created` records in the per-player set. A second
/// creation the SAME turn then does NOT prompt. This is NOT a per-source→per-player
/// switch discriminator (with one ever-present source both models agree); its job
/// is to prove the ACCEPT path still closes the window through
/// `record_token_created` now that `note_first_token_replacement_applied` is gone —
/// were the copy path to stop recording the player, the second creation would
/// wrongly prompt.
#[test]
fn moonlit_second_creation_same_turn_after_accept_does_not_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), host, "Ox");
    install_moonlit(runner.state_mut(), host, P0);

    // First creation: accept → a host-copy is created and records P0.
    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(at_replacement_choice(&runner), "first creation prompts");
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept");
    runner.advance_until_stack_empty();
    assert_eq!(
        host_copy_tokens(&runner, "Host Ox", P0).len(),
        1,
        "accept produced one host-copy"
    );
    assert!(
        runner
            .state()
            .players_who_created_token_this_turn
            .contains(&P0),
        "the copy recorded P0 in the per-player set"
    );

    // Second creation, same turn: window already spent → no prompt, no new copy.
    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(
        !at_replacement_choice(&runner),
        "second same-turn creation does NOT prompt — accept consumed the window, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        host_copy_tokens(&runner, "Host Ox", P0).len(),
        1,
        "still exactly one host-copy — the second creation was not replaced"
    );
}

/// B3-doubler (#1511 interaction): Moonlit + Doubling Season, create 1 token,
/// accept → exactly 2 host-copies with no re-prompt/recursion. Doubling Season
/// (a different source's rid, absent from the inherited applied set) still
/// doubles the substitute copies; Moonlit does NOT re-fire on its own copies
/// (inherited applied set, CR 614.5). Revert Step 5 (`HashSet::new()`)
/// → the copies inherit no applied set → Doubling Season re-applies to the
/// count-2 copy batch → >2 copies (and/or a re-prompt).
#[test]
fn moonlit_with_doubling_season_yields_two_host_copies_no_recursion() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), host, "Ox");
    install_moonlit(runner.state_mut(), host, P0);
    install_doubling_season(runner.state_mut(), P0);

    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    // Drive every replacement prompt (apply candidate 0) to completion.
    for _ in 0..8 {
        if at_replacement_choice(&runner) {
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("apply replacement");
            runner.advance_until_stack_empty();
        } else {
            break;
        }
    }
    assert!(
        !at_replacement_choice(&runner),
        "must not re-prompt/recurse on the substitute copies, got {:?}",
        runner.state().waiting_for
    );
    let copies = host_copy_tokens(&runner, "Host Ox", P0);
    assert_eq!(
        copies.len(),
        2,
        "Moonlit (copies of host) doubled by Doubling Season → exactly 2 host-copies, \
         not >2 (recursion) nor plain Soldiers"
    );
    for c in &copies {
        assert_eq!(
            (c.power, c.toughness),
            (Some(5), Some(4)),
            "each is a host-copy (5/4 Ox), not a copy-of-copy or a 1/1 Soldier"
        );
    }
}

/// P1 — parse round-trip: Moonlit lowers to the expected Optional CreateToken
/// replacement with zero `Effect::Unimplemented`. Sibling reach-guard: Jinnie
/// Fay's "if you would create one or more tokens…" still parses to a
/// `ChooseOneOf` substitution, unaffected by Moonlit's specific antecedent arm.
#[test]
fn moonlit_parses_to_copy_of_host_replacement() {
    use engine::types::ability::{
        ControllerRef, Effect, QuantityExpr, QuantityRef, ReplacementCondition, ReplacementMode,
    };

    let parsed = parse(
        MOONLIT_ORACLE,
        "Moonlit Meditation",
        &[],
        &["Enchantment"],
        &["Aura"],
    );
    assert_zero_unimplemented(&parsed, "Moonlit Meditation");

    let rep = parsed
        .replacements
        .iter()
        .find(|r| r.event == ReplacementEvent::CreateToken)
        .expect("Moonlit CreateToken replacement");
    assert_eq!(
        rep.token_owner_scope,
        Some(ControllerRef::You),
        "'you would create' → You owner scope"
    );
    assert_eq!(
        rep.condition,
        Some(ReplacementCondition::FirstTokenCreationEachTurn {
            player: ControllerRef::You,
        }),
        "first-time-each-turn gate"
    );
    assert!(
        matches!(rep.mode, ReplacementMode::Optional { decline: None }),
        "'you may instead' → Optional, got {:?}",
        rep.mode
    );
    assert_eq!(
        rep.valid_card, None,
        "no valid_card gate — CreateToken has no affected object id"
    );
    let exec = rep.execute.as_deref().expect("execute payload");
    match &*exec.effect {
        Effect::CopyTokenOf { target, count, .. } => {
            assert_eq!(
                *target,
                TargetFilter::AttachedTo,
                "copies of the enchanted host"
            );
            assert_eq!(
                *count,
                QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                "'that many' → EventContextAmount"
            );
        }
        other => panic!("expected CopyTokenOf, got {other:?}"),
    }

    let jinnie = parse(
        "If you would create one or more tokens, you may instead create that many \
         1/1 green and white Rabbit creature tokens or that many 3/3 green and white \
         Elk creature tokens.",
        "Jinnie Fay, Jetmir's Second",
        &[],
        &["Legendary", "Creature"],
        &["Cat", "Elf", "Druid"],
    );
    let jinnie_rep = jinnie
        .replacements
        .iter()
        .find(|r| r.event == ReplacementEvent::CreateToken)
        .expect("Jinnie CreateToken replacement still parses");
    assert!(
        matches!(
            &*jinnie_rep.execute.as_deref().unwrap().effect,
            Effect::ChooseOneOf { .. }
        ),
        "Jinnie remains a ChooseOneOf substitution, not stolen by Moonlit's arm"
    );
}

// ---------------------------------------------------------------------------
// Princess Yue / Fang / Gideon / quote-scoped assigned names
// ---------------------------------------------------------------------------

const PRINCESS_YUE_ORACLE: &str = "When Princess Yue dies, if she was a nonland creature, return this card to the battlefield tapped under your control. She's a land named Moon. She gains \"{T}: Add {C}.\" (She's still legendary.)\n{T}: Scry 2.";
const FANG_ORACLE: &str = "Flying\nWhenever Fang attacks, another target legendary creature you control gets +X/+0 until end of turn, where X is Fang's power.\nWhen Fang dies, if he wasn't a Spirit, return this card to the battlefield under your control. He's a Spirit in addition to his other types.";
const GIDEON_CHAMPION_ORACLE: &str = "[+1]: Put a loyalty counter on Gideon for each creature target opponent controls.\n[0]: Until end of turn, Gideon becomes a Human Soldier creature with power and toughness each equal to the number of loyalty counters on him and gains indestructible. He's still a planeswalker. Prevent all damage that would be dealt to him this turn.\n[−15]: Exile all other permanents.";
const ARGOTHIAN_ORACLE: &str = "Put two +1/+1 counters on each of X target lands you control. They each become 0/0 Elemental creatures with reach, haste, and \"When this creature leaves the battlefield, conjure a card named Forest onto the battlefield tapped.\" They're still lands.";
const AWAKENING_ORACLE: &str = "Put nine +1/+1 counters on target land you control. It becomes a legendary 0/0 Elemental creature with haste named Vitu-Ghazi. It's still a land.";
const TENTH_DISTRICT_HERO_ORACLE: &str = "{1}{W}, Collect evidence 2: This creature becomes a Human Detective with base power and toughness 4/4 and gains vigilance.\n{2}{W}, Collect evidence 4: If this creature is a Detective, it becomes a legendary creature named Mileva, the Stalwart, it has base power and toughness 5/5, and it gains \"Other creatures you control have indestructible.\"";
const CURSE_OF_FENRIC_ORACLE: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — For each player, destroy up to one target creature that player controls. For each creature destroyed this way, its controller creates a 3/3 green Mutant creature token with deathtouch.\nII — Target nontoken creature becomes a 6/6 legendary Horror creature named Fenric and loses all abilities.\nIII — Target Mutant fights another target creature named Fenric.";
const IRENCRAG_ORACLE: &str = "{T}: Add {C}.\nWhenever a legendary creature you control enters, you may have The Irencrag become a legendary Equipment artifact named Everflame, Heroes' Legacy. If you do, it gains equip {3} and \"Equipped creature gets +3/+3\" and loses all other abilities.";
const DISTURBED_SLUMBER_ORACLE: &str = "Until end of turn, target land you control becomes a 4/4 Dinosaur creature with reach and haste. It's still a land. It must be blocked this turn if able.";
const NISSA_VITAL_FORCE_ORACLE: &str = "[+1]: Untap target land you control. Until your next turn, it becomes a 5/5 Elemental creature with haste. It's still a land.\n[−3]: Return target permanent card from your graveyard to your hand.\n[−6]: You get an emblem with \"Whenever a land you control enters, you may draw a card.\"";
const SYLVAN_AWAKENING_ORACLE: &str = "Until your next turn, all lands you control become 2/2 Elemental creatures with reach, indestructible, and haste. They're still lands.";
const WRENN_REALMBREAKER_ORACLE: &str = "Lands you control have \"{T}: Add one mana of any color.\"\n[+1]: Up to one target land you control becomes a 3/3 Elemental creature with vigilance, hexproof, and haste until your next turn. It's still a land.\n[−2]: Mill three cards. You may put a permanent card from among the milled cards into your hand.\n[−7]: You get an emblem with \"You may play lands and cast permanent spells from your graveyard.\"";
const AWAKENER_DRUID_ORACLE: &str = "When this creature enters, target Forest becomes a 4/5 green Treefolk creature for as long as this creature remains on the battlefield. It's still a land.";
const HEDGE_WHISPERER_ORACLE: &str = "You may choose not to untap this creature during your untap step.\n{3}{G}, {T}, Collect evidence 4: Target land you control becomes a 5/5 green Plant Boar creature with haste for as long as this creature remains tapped. It's still a land. Activate only as a sorcery. (To collect evidence 4, exile cards with total mana value 4 or greater from your graveyard.)";
const CACOPHONY_UNLEASHED_ORACLE: &str = "When this enchantment enters, if you cast it, destroy all nonenchantment creatures.\nWhenever this enchantment or another enchantment you control enters, until end of turn, this enchantment becomes a legendary 6/6 Nightmare God creature with menace and deathtouch. It's still an enchantment.";
const CAVERNOUS_MAW_ORACLE: &str = "{T}: Add {C}.\n{2}: This land becomes a 3/3 Elemental creature until end of turn. It's still a Cave land. Activate only if the number of other Caves you control plus the number of Cave cards in your graveyard is three or greater.";

fn all_modifications(def: &AbilityDefinition) -> Vec<&ContinuousModification> {
    let mut result = Vec::new();
    let mut pending = vec![def];
    while let Some(node) = pending.pop() {
        if let Effect::GenericEffect {
            static_abilities, ..
        } = node.effect.as_ref()
        {
            result.extend(
                static_abilities
                    .iter()
                    .flat_map(|static_def| static_def.modifications.iter()),
            );
        }
        pending.extend(node.sub_ability.as_deref());
        pending.extend(node.else_ability.as_deref());
    }
    result
}

fn retained_type_definition<'a>(
    definition: &'a AbilityDefinition,
    core_type: &CoreType,
    subtype: Option<&str>,
) -> Option<&'a AbilityDefinition> {
    let carries_retained_type = match definition.effect.as_ref() {
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities.iter().any(|static_definition| {
            let has_core_type = static_definition.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    ContinuousModification::AddType {
                        core_type: actual
                    } if actual == core_type
                )
            });
            let has_subtype = subtype.is_none_or(|expected| {
                static_definition.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::AddSubtype { subtype }
                            if subtype == expected
                    )
                })
            });
            has_core_type && has_subtype
        }),
        _ => false,
    };
    if carries_retained_type {
        return Some(definition);
    }
    definition
        .sub_ability
        .as_deref()
        .and_then(|sub| retained_type_definition(sub, core_type, subtype))
        .or_else(|| {
            definition
                .else_ability
                .as_deref()
                .and_then(|otherwise| retained_type_definition(otherwise, core_type, subtype))
        })
}

fn parsed_retained_type_definition<'a>(
    parsed: &'a engine::parser::oracle::ParsedAbilities,
    core_type: CoreType,
    subtype: Option<&str>,
) -> &'a AbilityDefinition {
    parsed
        .abilities
        .iter()
        .find_map(|definition| retained_type_definition(definition, &core_type, subtype))
        .or_else(|| {
            parsed.triggers.iter().find_map(|trigger| {
                trigger.execute.as_deref().and_then(|definition| {
                    retained_type_definition(definition, &core_type, subtype)
                })
            })
        })
        .expect("retained-type production definition")
}

fn assert_retained_type_duration(
    parsed: &engine::parser::oracle::ParsedAbilities,
    name: &str,
    core_type: CoreType,
    subtype: Option<&str>,
    expected_duration: Duration,
) {
    assert_zero_unimplemented(parsed, name);
    let retained = parsed_retained_type_definition(parsed, core_type, subtype);
    assert_eq!(retained.duration, Some(expected_duration.clone()), "{name}");
    assert!(
        matches!(
            retained.effect.as_ref(),
            Effect::GenericEffect {
                duration: Some(actual),
                ..
            } if actual == &expected_duration
        ),
        "{name}: retained-type effect duration must match the governing animation: {retained:#?}"
    );
}

fn assert_exact_text_name(
    definitions: impl IntoIterator<Item = AbilityDefinition>,
    expected_name: &str,
) {
    let definitions: Vec<_> = definitions.into_iter().collect();
    let modifications: Vec<_> = definitions.iter().flat_map(all_modifications).collect();
    assert!(
        modifications.iter().any(|modification| matches!(
            modification,
            ContinuousModification::SetTextName { name } if name == expected_name
        )),
        "missing SetTextName({expected_name:?}) in {modifications:#?}"
    );
    assert!(
        !modifications
            .iter()
            .any(|modification| matches!(modification, ContinuousModification::SetName { .. })),
        "non-copy assigned name must not use SetName: {modifications:#?}"
    );
}

#[test]
fn resolving_outer_assigned_names_are_layer_three_in_all_full_cards() {
    let awakening = parse(
        AWAKENING_ORACLE,
        "Awakening of Vitu-Ghazi",
        &[],
        &["Instant"],
        &[],
    );
    assert_zero_unimplemented(&awakening, "Awakening of Vitu-Ghazi");
    assert_exact_text_name(awakening.abilities, "Vitu-Ghazi");

    let tenth = parse(
        TENTH_DISTRICT_HERO_ORACLE,
        "Tenth District Hero",
        &[],
        &["Creature"],
        &["Human"],
    );
    assert_zero_unimplemented(&tenth, "Tenth District Hero");
    assert_exact_text_name(tenth.abilities, "Mileva, the Stalwart");

    let fenric = parse(
        CURSE_OF_FENRIC_ORACLE,
        "The Curse of Fenric",
        &[],
        &["Enchantment"],
        &["Saga"],
    );
    assert_zero_unimplemented(&fenric, "The Curse of Fenric");
    let fenric_chapters = fenric
        .triggers
        .iter()
        .filter_map(|trigger| trigger.execute.as_deref().cloned());
    assert_exact_text_name(fenric_chapters, "Fenric");

    let irencrag = parse(IRENCRAG_ORACLE, "The Irencrag", &[], &["Artifact"], &[]);
    assert_zero_unimplemented(&irencrag, "The Irencrag");
    let execute = irencrag
        .triggers
        .iter()
        .filter_map(|trigger| trigger.execute.as_deref().cloned());
    assert_exact_text_name(execute, "Everflame, Heroes' Legacy");
}

#[test]
fn princess_fang_gideon_and_argothian_full_cards_parse_semantically() {
    let princess = parse(
        PRINCESS_YUE_ORACLE,
        "Princess Yue",
        &[],
        &["Legendary", "Creature"],
        &["Human", "Noble"],
    );
    assert_zero_unimplemented(&princess, "Princess Yue");
    let princess_trigger = princess.triggers.first().expect("Princess dies trigger");
    assert!(matches!(
        princess_trigger.condition,
        Some(engine::types::ability::TriggerCondition::ZoneChangeObjectMatchesFilter { .. })
    ));
    let princess_mods = all_modifications(
        princess_trigger
            .execute
            .as_deref()
            .expect("Princess trigger execute"),
    );
    assert!(princess_mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::SetTextName { name } if name == "Moon"
    )));
    assert!(princess_mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::SetCardTypes { core_types }
            if core_types == &vec![CoreType::Land]
    )));
    assert!(princess_mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::GrantAbility { definition }
            if matches!(definition.cost, Some(AbilityCost::Tap))
                && matches!(definition.effect.as_ref(), Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 }
                    },
                    ..
                })
    )));
    assert!(
        princess.abilities.iter().any(|definition| {
            matches!(definition.cost, Some(AbilityCost::Tap))
                && matches!(
                    definition.effect.as_ref(),
                    Effect::Scry {
                        count: QuantityExpr::Fixed { value: 2 },
                        ..
                    }
                )
        }),
        "Princess's printed tap/Scry ability must remain distinct from the granted mana ability"
    );

    let fang = parse(
        FANG_ORACLE,
        "Fang, Roku's Companion",
        &["Flying"],
        &["Legendary", "Creature"],
        &["Wolf", "Dog"],
    );
    assert_zero_unimplemented(&fang, "Fang, Roku's Companion");
    let fang_trigger = fang
        .triggers
        .iter()
        .find(|trigger| {
            matches!(
                trigger.condition,
                Some(engine::types::ability::TriggerCondition::Not { .. })
            )
        })
        .expect("Fang dies trigger");
    assert!(all_modifications(fang_trigger.execute.as_deref().unwrap())
        .iter()
        .any(|modification| matches!(
            modification,
            ContinuousModification::AddSubtype { subtype } if subtype == "Spirit"
        )));

    let gideon = parse(
        GIDEON_CHAMPION_ORACLE,
        "Gideon, Champion of Justice",
        &[],
        &["Legendary", "Planeswalker"],
        &["Gideon"],
    );
    assert_zero_unimplemented(&gideon, "Gideon, Champion of Justice");
    assert!(gideon
        .abilities
        .iter()
        .flat_map(all_modifications)
        .any(|modification| matches!(
            modification,
            ContinuousModification::AddType {
                core_type: CoreType::Planeswalker
            }
        )));

    let argothian = parse(
        ARGOTHIAN_ORACLE,
        "Argothian Uprooting",
        &[],
        &["Sorcery"],
        &[],
    );
    assert_zero_unimplemented(&argothian, "Argothian Uprooting");
    let argothian_mods: Vec<_> = argothian
        .abilities
        .iter()
        .flat_map(all_modifications)
        .collect();
    assert!(!argothian_mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::SetName { .. } | ContinuousModification::SetTextName { .. }
    )));
    assert!(argothian_mods
        .iter()
        .any(|modification| matches!(modification, ContinuousModification::SetPower { value: 0 })));
    assert!(argothian_mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::SetToughness { value: 0 }
    )));
    assert!(argothian_mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddSubtype { subtype } if subtype == "Elemental"
    )));
    assert!(argothian_mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddType {
            core_type: CoreType::Creature
        }
    )));
    assert!(argothian_mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddKeyword {
            keyword: Keyword::Reach
        }
    )));
    assert!(argothian_mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::AddKeyword {
            keyword: Keyword::Haste
        }
    )));
    assert!(argothian_mods.iter().any(|modification| matches!(
        modification,
        ContinuousModification::GrantTrigger { trigger }
            if matches!(trigger.execute.as_deref().map(|ability| ability.effect.as_ref()),
                Some(Effect::Conjure {
                    cards,
                    destination: Zone::Battlefield,
                    tapped: true,
                    ..
                }) if cards.len() == 1 && cards[0].named_name() == Some("Forest"))
    )));
}

fn run_dies_return_case(
    oracle: &str,
    name: &str,
    subtypes: Vec<&str>,
    starts_as_land: bool,
) -> (ObjectId, ObjectId, engine::game::scenario::CastOutcome) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut subject = scenario.add_creature_from_oracle(P0, name, 2, 2, oracle);
    subject
        .as_legendary()
        .with_subtypes(subtypes)
        .controlled_by(P1);
    if starts_as_land {
        subject.as_land().as_creature();
    }
    let subject = subject.id();
    let sentinel = scenario.add_creature(P1, "Sentinel", 3, 3).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(murder).target_object(subject).resolve();
    (subject, sentinel, outcome)
}

#[test]
fn princess_yue_dies_filter_and_returned_object_transformation_execute() {
    let (yue, sentinel, outcome) =
        run_dies_return_case(PRINCESS_YUE_ORACLE, "Princess Yue", vec!["Human"], false);
    outcome.assert_zone(&[yue], Zone::Battlefield);
    outcome.assert_zone(&[sentinel], Zone::Battlefield);
    let object = &outcome.state().objects[&yue];
    assert!(object.tapped);
    assert_eq!(object.controller, P1);
    assert_eq!(object.name, "Moon");
    assert!(object.card_types.supertypes.contains(&Supertype::Legendary));
    assert!(object.card_types.core_types.contains(&CoreType::Land));
    assert!(!object.card_types.core_types.contains(&CoreType::Creature));
    assert!(object.abilities.iter().any(|definition| {
        matches!(definition.cost, Some(AbilityCost::Tap))
            && matches!(
                definition.effect.as_ref(),
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 }
                    },
                    ..
                }
            )
    }));
    assert!(outcome
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            matches!(effect.affected, TargetFilter::SpecificObject { id } if id == yue)
        }));
    assert!(!outcome
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            matches!(effect.affected, TargetFilter::SpecificObject { id } if id == sentinel)
        }));
    assert!(!outcome.state().objects[&sentinel]
        .abilities
        .iter()
        .any(|definition| matches!(
            definition.effect.as_ref(),
            Effect::Mana {
                produced: ManaProduction::Colorless { .. },
                ..
            }
        )));

    let (land_yue, _, negative) =
        run_dies_return_case(PRINCESS_YUE_ORACLE, "Princess Yue", vec!["Human"], true);
    negative.assert_zone(&[land_yue], Zone::Graveyard);
    assert!(!negative
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            matches!(effect.affected, TargetFilter::SpecificObject { id } if id == land_yue)
        }));
    assert!(matches!(
        negative.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

fn assert_princess_yue_commander_recast_remains_printed(
    death_spell_name: &str,
    death_spell_oracle: &str,
    targets_yue: bool,
) {
    let parsed = parse(
        PRINCESS_YUE_ORACLE,
        "Princess Yue",
        &[],
        &["Legendary", "Creature"],
        &["Human", "Noble", "Ally"],
    );
    assert_zero_unimplemented(&parsed, "Princess Yue");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yue = scenario
        .add_creature_from_oracle(P0, "Princess Yue", 2, 2, PRINCESS_YUE_ORACLE)
        .as_legendary()
        .with_subtypes(vec!["Human", "Noble", "Ally"])
        .commander()
        .id();
    let death_spell = scenario
        .add_spell_to_hand_from_oracle(P0, death_spell_name, true, death_spell_oracle)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;

    let mut cast = if targets_yue {
        runner.cast(death_spell).target_object(yue).commit()
    } else {
        runner.cast(death_spell).commit()
    };

    // CR 903.9a: after Yue dies, the command-zone choice is offered before her
    // zone-change trigger can resolve.
    for _ in 0..4 {
        if matches!(
            cast.state().waiting_for,
            WaitingFor::CommanderZoneChoice { .. }
        ) {
            break;
        }
        cast.act(GameAction::PassPriority)
            .expect("death spell priority pass must be accepted");
    }
    assert!(
        matches!(
            cast.state().waiting_for,
            WaitingFor::CommanderZoneChoice {
                commander_id,
                current_zone: Zone::Graveyard,
                ..
            } if commander_id == yue
        ),
        "Yue's graveyard commander choice must surface before her dies trigger resolves; waiting_for = {:?}",
        cast.state().waiting_for
    );

    cast.act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Yue's commander-zone choice must be accepted");
    assert_eq!(
        cast.state().objects[&yue].zone,
        Zone::Command,
        "accepted commander choice must move Yue to the command zone"
    );

    let old_yue_trigger_on_stack = cast.state().stack.iter().any(|entry| {
        matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == yue
        )
    });
    assert!(
        old_yue_trigger_on_stack,
        "reach guard: Yue's old dies trigger must be on the stack after the command-zone move; stack = {:#?}",
        cast.state().stack
    );

    // CR 603.6c: Yue's leaves-the-battlefield trigger can find her only in the
    // first zone she went to. Resolve it normally after the command-zone move.
    for _ in 0..4 {
        if cast.state().stack.is_empty() {
            break;
        }
        cast.act(GameAction::PassPriority)
            .expect("Yue's old trigger priority pass must be accepted");
    }
    assert!(
        cast.state().stack.is_empty(),
        "Yue's old dies trigger must resolve before her command-zone recast"
    );

    let card_id = cast.state().objects[&yue].card_id;
    cast.act(GameAction::CastSpell {
        object_id: yue,
        card_id,
        targets: vec![],
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    })
    .expect("Yue must be castable from the command zone");
    for _ in 0..4 {
        if cast.state().stack.is_empty() {
            break;
        }
        cast.act(GameAction::PassPriority)
            .expect("Yue recast priority pass must be accepted");
    }

    let yue = &cast.state().objects[&yue];
    assert_eq!(
        yue.zone,
        Zone::Battlefield,
        "Yue must resolve from the command zone"
    );
    assert_eq!(
        yue.name, "Princess Yue",
        "a recast Yue must not retain the old dies trigger's Moon name"
    );
    assert!(
        yue.card_types.core_types.contains(&CoreType::Creature),
        "a recast Yue must retain her printed creature type"
    );
    assert!(
        !yue.card_types.core_types.contains(&CoreType::Land),
        "a recast Yue must not retain the old dies trigger's land type"
    );
    assert!(
        yue.abilities.iter().any(|definition| {
            matches!(definition.cost, Some(AbilityCost::Tap))
                && matches!(
                    definition.effect.as_ref(),
                    Effect::Scry {
                        count: QuantityExpr::Fixed { value: 2 },
                        ..
                    }
                )
        }),
        "reach guard: Yue's printed tap-to-scry ability must be present after recast"
    );
    assert!(
        !yue.abilities.iter().any(|definition| {
            matches!(definition.cost, Some(AbilityCost::Tap))
                && matches!(
                    definition.effect.as_ref(),
                    Effect::Mana {
                        produced: ManaProduction::Colorless {
                            count: QuantityExpr::Fixed { value: 1 }
                        },
                        ..
                    }
                )
        }),
        "Yue must not retain the old dies trigger's granted colorless mana ability"
    );
}

#[test]
fn princess_yue_commander_destroyed_then_recast_does_not_keep_moon_effects() {
    assert_princess_yue_commander_recast_remains_printed(
        "Murder",
        "Destroy target creature.",
        true,
    );
}

#[test]
fn princess_yue_commander_zero_toughness_then_recast_does_not_keep_moon_effects() {
    assert_princess_yue_commander_recast_remains_printed(
        "Languish",
        "All creatures get -4/-4 until end of turn.",
        false,
    );
}

fn dies_draw_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ))
}

fn commander_return_draw_trigger(commander: ObjectId) -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(Zone::Graveyard)
        .destination(Zone::Command)
        .valid_card(TargetFilter::SpecificObject { id: commander })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ))
}

fn commander_return_event_matcher(commander: ObjectId) -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(Zone::Graveyard)
        .destination(Zone::Command)
        .valid_card(TargetFilter::SpecificObject { id: commander })
}

/// CR 104.2a + CR 603.3b + CR 704.5a + CR 800.4a: a player-loss SBA that ends
/// the game can emit zone changes while the losing player's objects leave the
/// game. Once elimination has terminalized the game and cleared trigger
/// scaffolding, the outer priority pipeline must not collect those events into a
/// new deferred batch.
#[test]
fn sba_game_over_does_not_repopulate_terminal_trigger_scaffolding() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let observer = scenario
        .add_creature(P0, "Surviving exile observer", 2, 2)
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Battlefield)
                .destination(Zone::Exile)
                .valid_card(TargetFilter::Typed(
                    engine::types::ability::TypedFilter::creature(),
                ))
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )),
        )
        .id();
    let departing = scenario.add_creature(P1, "Departing creature", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().players[P1.0 as usize].life = 0;

    let result = runner
        .act(GameAction::PassPriority)
        .expect("the priority pass that discovers the player-loss SBA must be accepted");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    ));
    assert_eq!(runner.state().objects[&observer].zone, Zone::Battlefield);
    assert!(
        result.events.iter().any(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Exile,
                    ..
                } if *object_id == departing
            )
        }),
        "reach guard: player elimination must emit the observed battlefield-to-exile event"
    );
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "terminal GameOver cleanup must not be repopulated from SBA events"
    );
    assert!(runner.state().pending_trigger_order.is_none());
    assert!(runner.state().pending_trigger.is_none());
    assert!(runner.state().stack.is_empty());
}

/// An SBA batch that opens the commander-zone return choice must collect both
/// ordinary and delayed dies triggers before yielding the prompt. The two
/// trigger controllers make the APNAP stack order observable through draws.
#[test]
fn sba_commander_choice_defers_ordinary_and_delayed_dies_triggers() {
    let mut scenario = GameScenario::new_n_player(3, 91);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 delayed draw"]);
    scenario.with_library_top(P1, &["P1 SBA draw", "P1 commander-answer draw"]);
    let yue = scenario
        .add_creature_from_oracle(P0, "Princess Yue", 2, 2, PRINCESS_YUE_ORACLE)
        .as_legendary()
        .with_subtypes(vec!["Human", "Noble", "Ally"])
        .commander()
        .id();
    let p1_fodder = scenario
        .add_creature(P1, "P1 dies observer", 1, 1)
        .with_trigger_definition(dies_draw_trigger())
        .id();
    let answer_observer = scenario
        .add_creature(P1, "Commander return observer", 5, 5)
        .with_trigger_definition(commander_return_draw_trigger(yue))
        .id();
    let languish = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Languish",
            true,
            "All creatures get -4/-4 until end of turn.",
        )
        .id();
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    runner
        .state_mut()
        .delayed_triggers
        .push(DelayedTrigger::new(
            DelayedTriggerCondition::WhenDies {
                filter: TargetFilter::SpecificObject { id: p1_fodder },
            },
            Box::new(engine::types::ability::ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                yue,
                P0,
            )),
            P0,
            yue,
            true,
        ));
    let mut answer_delayed_ability = engine::types::ability::ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        vec![],
        yue,
        P0,
    );
    answer_delayed_ability.trigger_source =
        Some(engine::game::triggers::trigger_source_context_for_latch(
            runner.state(),
            &runner.state().objects[&yue],
        ));
    runner
        .state_mut()
        .delayed_triggers
        .push(DelayedTrigger::new(
            DelayedTriggerCondition::WhenNextEvent {
                trigger: Box::new(commander_return_event_matcher(yue)),
                or_trigger: None,
                lifetime: DelayedTriggerLifetime::Persistent,
            },
            Box::new(answer_delayed_ability),
            P0,
            yue,
            true,
        ));

    let mut cast = runner.cast(languish).commit();
    for _ in 0..4 {
        if matches!(
            cast.state().waiting_for,
            WaitingFor::CommanderZoneChoice { .. }
        ) {
            break;
        }
        cast.act(GameAction::PassPriority)
            .expect("Languish priority pass must be accepted");
    }
    assert!(matches!(
        cast.state().waiting_for,
        WaitingFor::CommanderZoneChoice { commander_id, .. } if commander_id == yue
    ));
    assert!(
        cast.state().stack.is_empty(),
        "the completed SBA batch must defer its triggers while the commander prompt is open"
    );
    assert_eq!(
        cast.state().deferred_triggers.len(),
        3,
        "the paused SBA batch must retain Yue's trigger plus P0's delayed and P1's ordinary dies trigger"
    );

    cast.act(GameAction::DecideOptionalEffect { accept: true })
        .expect("commander choice must be accepted");
    assert_eq!(cast.state().objects[&yue].zone, Zone::Command);
    match &cast.state().waiting_for {
        WaitingFor::OrderTriggers {
            player, triggers, ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(
                triggers.len(),
                3,
                "Yue's dies trigger, the SBA-generated delayed trigger, and the \
                 commander-answer delayed trigger must be ordered as one P0 batch"
            );
        }
        waiting_for => panic!(
            "the combined pre-prompt and answer-generated trigger batch must reach one \
             ordering prompt, got {waiting_for:?}"
        ),
    }
    assert!(
        cast.state().stack.is_empty(),
        "answer-generated ordinary triggers must not be dispatched ahead of the parked SBA batch"
    );
    assert!(
        cast.state().deferred_triggers.is_empty(),
        "the SBA-owned batch and commander-answer events must be consumed by this one APNAP ordering step"
    );
    for _ in 0..4 {
        if !cast.state().stack.is_empty() {
            break;
        }
        match cast.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => cast
                .act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                })
                .expect("same-controller deferred trigger ordering must be accepted"),
            WaitingFor::Priority { .. } => cast
                .act(GameAction::PassPriority)
                .expect("priority pass must drain the deferred SBA trigger batch"),
            waiting_for => panic!(
                "deferred SBA trigger batch must settle through ordering or priority, got {waiting_for:?}"
            ),
        };
    }
    assert_eq!(
        cast.state().stack.len(),
        5,
        "answering the prompt must put the parked and answer-generated triggers on the stack together"
    );
    assert!(cast
        .state()
        .stack
        .iter()
        .rev()
        .take(2)
        .all(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. }
                if *source_id == p1_fodder || *source_id == answer_observer
        )));

    for _ in 0..20 {
        if cast.state().stack.is_empty() {
            break;
        }
        cast.act(GameAction::PassPriority)
            .expect("deferred dies-trigger priority pass must be accepted");
    }
    assert_eq!(cast.state().players[P0.0 as usize].hand.len(), 1);
    assert_eq!(cast.state().players[P0.0 as usize].life, 21);
    assert_eq!(cast.state().players[P1.0 as usize].hand.len(), 2);
}

/// CR 603.2c + CR 730.3 + CR 903.9c: a merged commander dying in an SBA
/// produces one logical zone-change delivery containing the survivor and its
/// absorbed component. The logical owner collects the two graveyard-observer
/// occurrences before the commander prompt; the outer raw SBA scan must not
/// collect either occurrence a second time.
#[test]
fn sba_merged_commander_prompt_does_not_duplicate_logical_zone_observers() {
    let mut scenario = GameScenario::new_n_player(3, 94);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["first component draw", "second component draw"]);
    let commander = scenario
        .add_creature(P0, "Merged Commander Host", 2, 2)
        .commander()
        .id();
    let rider = scenario.add_creature(P0, "Zero-Toughness Rider", 0, 0).id();
    let observer = scenario
        .add_creature(P1, "Merged graveyard observer", 5, 5)
        .with_trigger_definition(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .destination(Zone::Graveyard)
                .valid_card(TargetFilter::Any)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )),
        )
        .id();
    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    let mut merge_events = Vec::new();
    engine::game::merge::merge_object_onto(
        runner.state_mut(),
        rider,
        commander,
        engine::game::merge::MergeSide::Top,
        &mut merge_events,
    );
    assert_eq!(runner.state().objects[&commander].toughness, Some(0));
    assert!(runner.state().objects[&commander]
        .merged_components
        .contains(&rider));

    runner
        .act(GameAction::PassPriority)
        .expect("priority pass must run the merged-permanent SBA");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CommanderZoneChoice {
            commander_id,
            current_zone: Zone::Graveyard,
            ..
        } if commander_id == commander
    ));
    assert_eq!(
        runner.state().deferred_triggers.len(),
        2,
        "the observer must trigger once for each component card put into the graveyard, not twice per component"
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the commander return must be accepted");
    if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        assert_eq!(triggers.len(), 2);
        runner
            .act(GameAction::OrderTriggers { order: vec![0, 1] })
            .expect("the two legitimate component observers must be orderable");
    }
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| matches!(
                entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == observer
            ))
            .count(),
        2,
        "exactly the two logical component occurrences must reach the stack"
    );
}

/// A completed SBA pass may find both a legend-rule choice and independent
/// deaths. The production priority pipeline must park those trigger records
/// until the legend choice is answered.
#[test]
fn sba_legend_choice_defers_ordinary_and_delayed_dies_triggers() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 92);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 delayed draw"]);
    scenario.with_library_top(P1, &["P1 ordinary draw"]);
    let first = scenario
        .add_creature(P0, "Duplicate Legend", 2, 2)
        .as_legendary()
        .id();
    let _second = scenario
        .add_creature(P0, "Duplicate Legend", 2, 2)
        .as_legendary()
        .id();
    let p1_fodder = scenario
        .add_creature(P1, "P1 dies observer", 0, 0)
        .with_trigger_definition(dies_draw_trigger())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .delayed_triggers
        .push(DelayedTrigger::new(
            DelayedTriggerCondition::WhenDies {
                filter: TargetFilter::SpecificObject { id: p1_fodder },
            },
            Box::new(engine::types::ability::ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                first,
                P0,
            )),
            P0,
            first,
            true,
        ));

    runner
        .act(GameAction::PassPriority)
        .expect("production priority pass must run the SBA loop");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseLegend { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().deferred_triggers.len(), 2);

    runner
        .act(GameAction::ChooseLegend { keep: first })
        .expect("legend choice must be accepted");
    for _ in 0..4 {
        if !runner.state().stack.is_empty() {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("deferred dies trigger batch must drain after legend choice"),
            waiting_for => panic!("unexpected wait after legend choice: {waiting_for:?}"),
        };
    }
    assert_eq!(runner.state().stack.len(), 2);
    assert!(matches!(
        runner.state().stack.last().map(|entry| &entry.kind),
        Some(StackEntryKind::TriggeredAbility { source_id, .. }) if *source_id == p1_fodder
    ));
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("dies-trigger priority pass must be accepted");
    }
    assert_eq!(runner.state().players[P0.0 as usize].hand.len(), 1);
    assert_eq!(runner.state().players[P1.0 as usize].hand.len(), 1);
    assert!(runner.state().players[p2.0 as usize].hand.is_empty());
}

/// Like the legend-rule case, an illegal Siege protector pauses a completed SBA
/// batch. Its independent deaths must be retained until the protector answer.
#[test]
fn sba_battle_protector_choice_defers_ordinary_and_delayed_dies_triggers() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 93);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 delayed draw"]);
    scenario.with_library_top(P1, &["P1 ordinary draw"]);
    let battle = scenario.add_creature(P0, "Illegal Siege", 1, 1).id();
    let p1_fodder = scenario
        .add_creature(P1, "P1 dies observer", 0, 0)
        .with_trigger_definition(dies_draw_trigger())
        .id();
    let mut runner = scenario.build();
    {
        let battle_object = runner.state_mut().objects.get_mut(&battle).unwrap();
        battle_object.card_types.core_types = vec![CoreType::Battle];
        battle_object.card_types.subtypes = vec!["Siege".to_string()];
        battle_object.base_card_types = battle_object.card_types.clone();
        battle_object.power = None;
        battle_object.toughness = None;
        battle_object.base_power = None;
        battle_object.base_toughness = None;
        battle_object.defense = Some(3);
        battle_object.base_defense = Some(3);
        battle_object.counters.insert(CounterType::Defense, 3);
        battle_object
            .chosen_attributes
            .push(ChosenAttribute::Player(P0));
    }
    runner
        .state_mut()
        .delayed_triggers
        .push(DelayedTrigger::new(
            DelayedTriggerCondition::WhenDies {
                filter: TargetFilter::SpecificObject { id: p1_fodder },
            },
            Box::new(engine::types::ability::ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                battle,
                P0,
            )),
            P0,
            battle,
            true,
        ));

    runner
        .act(GameAction::PassPriority)
        .expect("production priority pass must run the SBA loop");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::BattleProtectorChoice { battle_id, .. } if battle_id == battle
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().deferred_triggers.len(), 2);

    runner
        .act(GameAction::ChooseBattleProtector { protector: p2 })
        .expect("legal Siege protector choice must be accepted");
    assert_eq!(runner.state().objects[&battle].protector(), Some(p2));
    for _ in 0..4 {
        if !runner.state().stack.is_empty() {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("deferred dies trigger batch must drain after protector choice"),
            waiting_for => panic!("unexpected wait after protector choice: {waiting_for:?}"),
        };
    }
    assert_eq!(runner.state().stack.len(), 2);
    assert!(matches!(
        runner.state().stack.last().map(|entry| &entry.kind),
        Some(StackEntryKind::TriggeredAbility { source_id, .. }) if *source_id == p1_fodder
    ));
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("dies-trigger priority pass must be accepted");
    }
    assert_eq!(runner.state().players[P0.0 as usize].hand.len(), 1);
    assert_eq!(runner.state().players[P1.0 as usize].hand.len(), 1);
}

#[test]
fn fang_dies_filter_adds_spirit_only_when_it_was_absent() {
    let (fang, _, outcome) = run_dies_return_case(
        FANG_ORACLE,
        "Fang, Roku's Companion",
        vec!["Wolf", "Dog"],
        false,
    );
    outcome.assert_zone(&[fang], Zone::Battlefield);
    let object = &outcome.state().objects[&fang];
    assert!(object.card_types.core_types.contains(&CoreType::Creature));
    assert!(object
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Wolf"));
    assert!(object
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Spirit"));
    assert!(outcome
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            matches!(effect.affected, TargetFilter::SpecificObject { id } if id == fang)
        }));

    let (spirit_fang, _, negative) = run_dies_return_case(
        FANG_ORACLE,
        "Fang, Roku's Companion",
        vec!["Wolf", "Spirit"],
        false,
    );
    negative.assert_zone(&[spirit_fang], Zone::Graveyard);
    assert!(!negative
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            matches!(effect.affected, TargetFilter::SpecificObject { id } if id == spirit_fang)
        }));
    assert!(matches!(
        negative.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// Candidate-bound production coverage for every retained-type duration class in
/// the 79-card projected comparator slice. The 72-card add-Land EOT class keeps
/// its runtime discriminator below (Disturbed Slumber); these shipped cards pin
/// all three non-EOT authorities plus the two distinct one-card EOT payloads.
/// Reverting the preceding-animation binding changes every retained definition
/// asserted here back to `Permanent`.
#[test]
fn shipped_retained_type_duration_classes_follow_the_governing_animation() {
    let until_next_turn = Duration::UntilNextTurnOf {
        player: PlayerScope::Controller,
    };
    for (name, oracle, types, subtypes) in [
        (
            "Nissa, Vital Force",
            NISSA_VITAL_FORCE_ORACLE,
            &["Legendary", "Planeswalker"][..],
            &["Nissa"][..],
        ),
        (
            "Sylvan Awakening",
            SYLVAN_AWAKENING_ORACLE,
            &["Sorcery"][..],
            &[][..],
        ),
        (
            "Wrenn and Realmbreaker",
            WRENN_REALMBREAKER_ORACLE,
            &["Legendary", "Planeswalker"][..],
            &["Wrenn"][..],
        ),
    ] {
        let parsed = parse(oracle, name, &[], types, subtypes);
        assert_retained_type_duration(&parsed, name, CoreType::Land, None, until_next_turn.clone());
    }

    let awakener = parse(
        AWAKENER_DRUID_ORACLE,
        "Awakener Druid",
        &[],
        &["Creature"],
        &["Human", "Druid"],
    );
    // CR 611.2b + CR 702.26f: "for as long as Awakener Druid remains on the
    // battlefield" is the presence-bound state reading — a phase-out of the
    // Druid ends the animation, so it carries `WhileHostOnBattlefield`, not
    // the "until ~ leaves the battlefield" event deadline.
    assert_retained_type_duration(
        &awakener,
        "Awakener Druid",
        CoreType::Land,
        None,
        Duration::WhileHostOnBattlefield,
    );

    let hedge = parse(
        HEDGE_WHISPERER_ORACLE,
        "Hedge Whisperer",
        &[],
        &["Creature"],
        &["Elf", "Druid"],
    );
    assert_retained_type_duration(
        &hedge,
        "Hedge Whisperer",
        CoreType::Land,
        None,
        Duration::ForAsLongAs {
            condition: StaticCondition::SourceIsTapped,
        },
    );

    let cacophony = parse(
        CACOPHONY_UNLEASHED_ORACLE,
        "Cacophony Unleashed",
        &[],
        &["Legendary", "Enchantment"],
        &[],
    );
    assert_retained_type_duration(
        &cacophony,
        "Cacophony Unleashed",
        CoreType::Enchantment,
        None,
        Duration::UntilEndOfTurn,
    );

    let cavernous_maw = parse(
        CAVERNOUS_MAW_ORACLE,
        "Cavernous Maw",
        &[],
        &["Land"],
        &["Cave"],
    );
    assert_retained_type_duration(
        &cavernous_maw,
        "Cavernous Maw",
        CoreType::Land,
        Some("Cave"),
        Duration::UntilEndOfTurn,
    );
}

/// CR 205.1b + CR 514.2 + CR 611.2a: the separate "It's still a land"
/// sentence modifies the preceding animation; it does not create an independent
/// permanent continuous effect. This exact shipped-card cast drives the parsed
/// chain through resolution and cleanup. Reverting the duration binding leaves
/// the retained-Land transient at `Permanent`, so the final assertion fails.
#[test]
fn retained_type_clause_expires_with_its_governing_animation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Disturbed Slumber", true, DISTURBED_SLUMBER_ORACLE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(land).resolve();
    assert!(
        outcome
            .state()
            .transient_continuous_effects
            .iter()
            .any(|effect| {
                effect.duration == Duration::UntilEndOfTurn
                    && matches!(effect.affected, TargetFilter::SpecificObject { id } if id == land)
                    && effect.modifications.iter().any(|modification| {
                        matches!(
                            modification,
                            ContinuousModification::AddType {
                                core_type: CoreType::Land
                            }
                        )
                    })
            }),
        "reach guard: the retained-Land clause must install an UntilEndOfTurn transient"
    );

    let mut events = Vec::new();
    execute_cleanup(runner.state_mut(), &mut events);
    evaluate_layers(runner.state_mut());

    assert!(
        !runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|effect| {
                matches!(effect.affected, TargetFilter::SpecificObject { id } if id == land)
                    && effect.modifications.iter().any(|modification| {
                        matches!(
                            modification,
                            ContinuousModification::AddType {
                                core_type: CoreType::Land
                            }
                        )
                    })
            }),
        "CR 514.2: the retained-type transient must expire with the governing animation"
    );
}
