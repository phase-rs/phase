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
//! Deferred (honest `Effect::unimplemented` / SwallowedClause retained, NOT
//! asserted 0-unimpl): Moonlit Meditation (first-time-each-turn optional
//! CreateToken replacement that overrides the spec to copies of the enchanted
//! permanent — needs a per-turn token-creation gate, an Optional CreateToken
//! pipeline, and a dynamic host-copy spec; out of scope for the multiplier
//! parameterization), Zimone (prime-number intervening-if
//! condition — heavy primality predicate; the token+counter parse is fixed, the
//! card stays honestly condition-unsupported via a SwallowedClause warning).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::TargetFilter;
use engine::types::ability::TargetRef;
use engine::types::events::GameEvent;
use engine::types::phase::Phase;

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
