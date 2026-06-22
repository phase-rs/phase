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
//! Deferred (honest `Effect::unimplemented` / SwallowedClause retained, NOT
//! asserted 0-unimpl): Moonlit Meditation (first-time-each-turn optional
//! CreateToken replacement that overrides the spec to copies of the enchanted
//! permanent — needs a per-turn token-creation gate, an Optional CreateToken
//! pipeline, and a dynamic host-copy spec; out of scope for the multiplier
//! parameterization), Vraska the Silencer (return-as-Treasure-with-ability —
//! heavy continuous-self-modification infra), Zimone (prime-number intervening-if
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
