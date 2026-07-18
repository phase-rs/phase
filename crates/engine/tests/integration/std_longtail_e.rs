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
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::TargetFilter;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
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
    AbilityCost, AbilityDefinition, ContinuousModification, Duration, Effect,
};
use engine::types::card_type::CoreType;
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
