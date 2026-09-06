//! Issue #7923 — a leading duration must govern EVERY conjunct it prefixes.
//!
//! CR 611.2a read with CR 608.2c: a stated duration "lasts as long as stated by
//! the spell or ability creating it", and its scope is settled by reading the
//! WHOLE printed text and applying the rules of English. Two structurally
//! distinct seams:
//!
//! * **U1** — a stated duration must reach every GOVERNED link of the
//!   `sub_ability` chain a clause's own recognizer built
//!   (`oracle_ir::ast::with_clause_chain_duration` / `apply_duration_to_effect` /
//!   `duration_governs`, called from the generic leading-duration arm and from
//!   `try_parse_cant_cast_spells_effect`).
//! * **U2** — a leading-duration body that chunks into two or more conjuncts of
//!   which the single-clause recognizer consumes only a prefix is EXPANDED into
//!   sibling chunks of the same chain by
//!   `oracle_effect::sequence::expand_leading_duration_chunks`, each carrying the
//!   printed duration as a typed value. Three guards decline the boundaries where
//!   a recovered conjunct is not an independent, understood instruction.
//!
//! ## ANCHOR TAGS — read these before changing an assertion
//!
//! Each test's doc names the anchor its assertions actually have. This matters:
//! a column headed "revert-failing" holding an assertion that passes unchanged at
//! BASE_SHA was found three separate times during review, and the tags exist to
//! stop a fourth.
//!
//! | Tag | Meaning |
//! |---|---|
//! | `[BASE]` | Fails at BASE_SHA as written. |
//! | `[GUARD:n]` | Passes at BASE_SHA. Fails if guard *n* (or the named deletion) is removed FROM THE SHIPPED DESIGN. |
//! | `[GUARD:n+m]` | Same, for a guard SET whose declines OVERLAP: the row passes when any one of the named guards is removed and fails only when ALL of them are removed together. Use this rather than `[COVER]` when a named removal does turn the row red. |
//! | `[NEW-UNIT]` | Unit test over a helper that does not exist at BASE_SHA; anchored against MISCLASSIFICATION, not against BASE. (Those live beside their helpers, in `oracle_ir/ast.rs` and `oracle_effect/sequence.rs`.) |
//! | `[COVER]` | Behaviour-preservation cover. Passes at BASE_SHA BY DESIGN; its doc names the test that carries the revert-failing content. |
//!
//! ## Resolution-driver discipline (binds every RUNTIME test here)
//!
//! `SpellCast::resolve` / `AbilityActivation::resolve` AUTO-ANSWER
//! `WaitingFor::OptionalEffectChoice` from `ResolutionPolicy.optional`, whose
//! default is `Decline`. So:
//!
//! 1. Never assert on the presence or absence of a prompt — assert STATE DELTAS.
//! 2. Always set `.decline_optional()` / `.accept_optional()` EXPLICITLY, even
//!    when choosing the default.
//! 3. Where a test's claim is that two branches differ, drive both and write the
//!    ACCEPT branch first, as the test's own positive control.
//!
//! Every card here is built from VERBATIM Oracle text (`/card-test`), never a
//! paraphrase and never `add_real_card` — none of these cards is in
//! `integration_cards.json.gz` except Memory Vessel and Mondo Gecko, and expanding
//! that fixture is not this PR's business.

use engine::game::combat::{creature_cant_attack, AttackTarget};
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CastingPermission, ContinuousModification, Duration, Effect,
    GameRestriction, ManaSpendPermission, PlayerScope, ProhibitedActivity, StaticCondition,
    SubAbilityLink, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{TransientContinuousEffect, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

// ---------------------------------------------------------------------------
// Verbatim Oracle text (MTGJSON `AtomicCards.json`)
// ---------------------------------------------------------------------------

const XANATHAR: &str = "At the beginning of your upkeep, choose target opponent. Until end of turn, that player can't cast spells, you may look at the top card of their library any time, you may play the top card of their library, and you may spend mana as though it were mana of any color to cast spells this way.";
const KIORA: &str = "[+1]: Until your next turn, prevent all damage that would be dealt to and dealt by target permanent an opponent controls.\n[−1]: Draw a card. You may play an additional land this turn.\n[−5]: You get an emblem with \"At the beginning of your end step, create a 9/9 blue Kraken creature token.\"";
const DOVIN: &str = "Artifact, instant, and sorcery spells your opponents cast cost {1} more to cast.\n[−1]: Until your next turn, prevent all damage that would be dealt to and dealt by target permanent an opponent controls.";
const PRISONERS: &str = "Choose one —\n• Break Their Chains — Destroy target artifact.\n• Interrogate Them — Exile the top three cards of target opponent's library. Choose one of them. Until the end of your next turn, you may play that card, and you may spend mana as though it were mana of any color to cast it.";
const AURELIA: &str = "Flying\nMentor (Whenever this creature attacks, put a +1/+1 counter on target attacking creature with lesser power.)\nAt the beginning of combat on your turn, choose up to one target creature you control. Until end of turn, that creature gets +2/+0, gains trample if it's red, and gains vigilance if it's white.";
const GIANT_OYSTER: &str = "You may choose not to untap this creature during your untap step.\n{T}: For as long as this creature remains tapped, target tapped creature doesn't untap during its controller's untap step, and at the beginning of each of your draw steps, put a -1/-1 counter on that creature. When this creature leaves the battlefield or becomes untapped, remove all -1/-1 counters from the creature.";
const BELLIGERENT: &str = "Whenever The Belligerent attacks, create a Treasure token. Until end of turn, you may look at the top card of your library any time, and you may play lands and cast spells from the top of your library.\nCrew 3";
const OPPORTUNISTIC_DRAGON: &str = "Flying\nWhen this creature enters, choose target Human or artifact an opponent controls. For as long as this creature remains on the battlefield, gain control of that permanent, it loses all abilities, and it can't attack or block.";
const MURDER: &str = "Destroy target creature.";
const TEMPORAL_APERTURE: &str = "{5}, {T}: Shuffle your library, then reveal the top card. Until end of turn, for as long as that card remains on top of your library, play with the top card of your library revealed and you may play that card without paying its mana cost.";
const ONE_RING: &str = "Indestructible\nWhen The One Ring enters, if you cast it, you gain protection from everything until your next turn.\nAt the beginning of your upkeep, you lose 1 life for each burden counter on The One Ring.\n{T}: Put a burden counter on The One Ring, then draw a card for each burden counter on The One Ring.";
const ABEYANCE: &str = "Until end of turn, target player can't cast instant or sorcery spells, and that player can't activate abilities that aren't mana abilities.\nDraw a card.";
const REVENGE: &str = "Until end of turn, target creature gets +6/+6 and gains trample, and all creatures able to block it this turn do so.\nMiracle {G} (You may cast this card for its miracle cost when you draw it if it's the first card you drew this turn.)";

// ---------------------------------------------------------------------------
// Chain-walk helpers
// ---------------------------------------------------------------------------

/// Flatten a def's `sub_ability` chain, head first — the same walk
/// `with_clause_chain_duration` performs.
fn chain(head: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut out = Vec::new();
    let mut cursor = Some(head);
    while let Some(def) = cursor {
        out.push(def);
        cursor = def.sub_ability.as_deref();
    }
    out
}

/// A trigger's executed body. `TriggerDefinition::execute` is
/// `Option<Box<AbilityDefinition>>`; every trigger under test here has one.
fn trigger_body(t: &engine::types::TriggerDefinition) -> &AbilityDefinition {
    t.execute.as_deref().expect("trigger has an executed body")
}

/// Positive reach guard used by every SHAPE test here: the parse must have
/// SUCCEEDED. Without this, a negative assertion ("no top-level PutCounter",
/// "byte-identical to BASE") passes vacuously on a chain that failed to parse.
fn assert_no_unimplemented(links: &[&AbilityDefinition], what: &str) {
    assert!(
        !links
            .iter()
            .any(|d| matches!(&*d.effect, Effect::Unimplemented { .. })),
        "{what}: chain must carry no Effect::Unimplemented placeholder: {:#?}",
        links.iter().map(|d| &d.effect).collect::<Vec<_>>()
    );
}

fn statics_of(effect: &Effect) -> &[engine::types::ability::StaticDefinition] {
    match effect {
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities,
        _ => &[],
    }
}

/// The live `TransientContinuousEffect`s a resolution installed on `obj` — the
/// runtime carrier `Effect::GenericEffect`'s embedded duration becomes.
fn gecko_grants(
    state: &engine::types::game_state::GameState,
    obj: ObjectId,
) -> Vec<&TransientContinuousEffect> {
    state
        .transient_continuous_effects
        .iter()
        .filter(|e| matches!(e.affected, TargetFilter::SpecificObject { id } if id == obj))
        .collect()
}

/// The single installed `PlayFromExile` permission anywhere in the game, with its
/// host — the runtime carrier `cast_from_zone::resolve` writes and
/// `layers::prune_end_of_turn_casting_permissions` reads.
fn find_play_permission(
    state: &engine::types::game_state::GameState,
) -> Option<(ObjectId, Duration)> {
    state.objects.iter().find_map(|(id, o)| {
        o.casting_permissions.iter().find_map(|p| match p {
            CastingPermission::PlayFromExile { duration, .. } => Some((*id, duration.clone())),
            _ => None,
        })
    })
}

/// Drive `turns` whole turns through the real turn machinery, ACCEPTING every
/// optional ("you may") effect and taking the first legal target at any target
/// window. §G1 rule 2: the policy is explicit, never defaulted — Xanathar's play
/// permission hangs off an `optional: true` Dig as a `ContinuationStep`, so a
/// declined optional would skip the link under test and the test would pass
/// vacuously.
fn run_turns_accepting_optionals(runner: &mut GameRunner, turns: u32) {
    let start = runner.state().turn_number;
    for _ in 0..600 {
        if runner.state().turn_number >= start + turns
            && runner.stack_names().is_empty()
            && !matches!(runner.state().phase, Phase::Upkeep)
        {
            break;
        }
        let r = match runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => runner.choose_first_legal_target(),
            WaitingFor::OptionalEffectChoice { .. } => {
                runner.act(GameAction::DecideOptionalEffect { accept: true })
            }
            WaitingFor::DeclareAttackers { .. } => runner.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            _ => runner.act(GameAction::PassPriority),
        };
        if r.is_err() {
            break;
        }
    }
    // REACH GUARD: a stalled walk must not silently make a later assertion vacuous.
    assert!(
        runner.state().turn_number >= start + turns,
        "turn walk stalled at turn {} (phase {:?}, waiting {:?})",
        runner.state().turn_number,
        runner.state().phase,
        runner.state().waiting_for
    );
}

/// Whether a live `GameRestriction` in `state.restrictions` prohibits `player`
/// from an activity matching `pred`.
fn player_is_prohibited(
    state: &engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
    pred: impl Fn(&ProhibitedActivity) -> bool,
) -> bool {
    state.restrictions.iter().any(|r| match r {
        GameRestriction::ProhibitActivity {
            affected_players,
            activity,
            ..
        } => {
            pred(activity)
                && matches!(
                    affected_players,
                    engine::types::ability::RestrictionPlayerScope::SpecificPlayer(p)
                        if *p == player
                )
        }
        _ => false,
    })
}

/// Advance past the current turn's cleanup step (CR 514.2 — where all "until end
/// of turn" effects end) into the next turn, driving whatever turn-based prompt
/// the engine surfaces. Bounded; asserts the boundary was actually crossed, so a
/// stalled walk can never make an expiry assertion pass vacuously.
fn cross_turn_boundary(runner: &mut GameRunner) {
    let start = runner.state().turn_number;
    for _ in 0..400 {
        if runner.state().turn_number > start {
            break;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        if runner.act(action).is_err() {
            break;
        }
    }
    // REACH GUARD: without this an expiry assertion passes vacuously on a walk
    // that never left the turn (`advance_to_phase` stalls silently when a
    // `DeclareAttackers` window is open).
    assert!(
        runner.state().turn_number > start,
        "turn boundary was not crossed: still turn {start}, phase {:?}, waiting {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
}

/// Ample colorless-source mana so any cast in this file auto-pays from the pool
/// (`/card-test`: pool-funded casts never surface a `ManaPayment` window).
fn ample_mana() -> Vec<ManaUnit> {
    (0..10)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// Whether a LIVE static definition on `obj` carries `mode` — the runtime read of
/// a `ContinuousModification::AddStaticMode` that a layer pass installed.
fn object_has_static_mode(
    state: &engine::types::game_state::GameState,
    obj: ObjectId,
    pred: impl Fn(&StaticMode) -> bool,
) -> bool {
    state.objects[&obj]
        .static_definitions
        .as_slice()
        .iter()
        .any(|s| pred(&s.mode))
}

// ===========================================================================
// U1 — a stated duration reaches every governed link of a recognizer-built chain
// whose links carry NO duration of their own.
//
// SCOPE, stated precisely because the unqualified form is FALSE: the sub-link
// walk in `with_clause_chain_duration` gates on the link's CARRIER
// (`def.duration.is_none() || Some(Permanent)`), so a link whose recognizer
// INJECTED a duration default is indistinguishable from one that printed a
// window, and the walk declines. Measured at this head, FOUR corpus cards sit in
// that gap, in TWO distinct shapes:
//
//   * Dovin Baan, Edifice of Authority, Mythos of Vadrok — the "and its/their
//     activated abilities can't be activated" link is injected `UntilEndOfTurn`
//     by `oracle_effect/subject.rs`'s prohibition recognizer, so the link's
//     CARRIER holds a value indistinguishable from a printed window and the walk
//     declines. Head is `UntilNextTurnOf{Controller}`; the prohibition ends a
//     full turn early.
//
//   * Teferi's Protection — a DIFFERENT recognizer and, measured, a DIFFERENT
//     CAUSE. Its "you gain protection from everything" link carries
//     `def.duration: None` with an injected `UntilEndOfTurn` on the EMBEDDED
//     `GenericEffect.duration`, under the same `UntilNextTurnOf{Controller}`
//     head, so protection from everything expires a turn early.
//
//     Note carefully what this is NOT: a `None` carrier PASSES the sub-link
//     walk's gate (probed at this head, Abeyance's `AddRestriction` and Kiora's
//     second `PreventDamage` link both have `None` carriers and both ARE stamped
//     by that walk). So the carrier gate is not what fails here — the head window
//     simply never reaches this link. The precise mechanism is NOT diagnosed in
//     this PR; only the parse state and the symptom above are measured.
//
// None of the four is a regression — all are byte-identical at BASE_SHA.
//
// The three PROHIBITION cards additionally never reach the severed-conjunct seam
// this PR adds, because `starts_clause_text_or_conjugated` excludes "its" (explicit
// list) and "their" (the `ends_with('s')` pre-filter), so those sentences never
// split. That reasoning does NOT extend to Teferi's Protection: `tag("you ")` IS a
// clause-start token, so its sentence does split.
//
// SCOPING NOTE for whoever picks this up — both halves matter:
//   1. The `subject.rs` verbatim-emit remedy (the fix
//      `try_parse_gain_all_activated_abilities_of_target` received here) closes
//      shape 1 ONLY.
//   2. Applying that remedy across the injected-default CLASS (~16
//      `.or(Some(Duration::UntilEndOfTurn))` / `unwrap_or(Duration::UntilEndOfTurn)`
//      sites in `subject.rs`, `imperative.rs`, `oracle_effect/mod.rs`) is NOT
//      sufficient for shape 2: leaving the embedded field `None` just moves the
//      default downstream, where `game/effects/effect.rs` resolves
//      `ability.duration.or(duration).unwrap_or(UntilEndOfTurn)` to end-of-turn
//      anyway. Shape 2 needs the printed head window to actually REACH the link,
//      which is precisely what tasks #138/#144 in `game/effects/effect.rs` track.
// ===========================================================================

/// **V-U1a — `[BASE]`, SHAPE.** CR 611.2a.
///
/// Xanathar's whole "Until end of turn, …" sentence is owned by
/// `try_parse_cant_cast_spells_effect`, which peels the leading duration itself
/// and builds its own `sub_ability` chain from the tail conjuncts. At BASE_SHA it
/// wrote the duration into its `ParsedEffectClause` LITERAL, so the duration
/// reached the HEAD ONLY: the `CastFromZone` play-permission and its trailing
/// mana-spend `GenericEffect` were both emitted with `duration: None` — i.e. a
/// permission that is NEVER pruned (CR 611.2a: "If no duration is stated, it lasts
/// until the end of the game").
///
/// FAILS AT BASE_SHA: both of those links carry `None` there.
#[test]
fn xanathar_leading_duration_reaches_governed_chain_links() {
    let parsed = parse_oracle_text(
        XANATHAR,
        "Xanathar, Guild Kingpin",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Beholder".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1, "one upkeep trigger");
    let links = chain(trigger_body(&parsed.triggers[0]));

    // Positive reach guard: the clause parsed at all, and link 1 is the
    // prohibition the recognizer under test produces.
    assert_no_unimplemented(&links, "Xanathar");
    assert!(
        links.iter().any(|d| matches!(
            &*d.effect,
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    activity: ProhibitedActivity::CastSpells { .. },
                    ..
                },
            }
        )),
        "the `can't cast spells` prohibition must be present: {links:#?}"
    );

    // THE REVERT-FAILING ASSERTIONS.
    let cast_from_zone = links
        .iter()
        .find(|d| matches!(&*d.effect, Effect::CastFromZone { .. }))
        .expect("the `you may play the top card of their library` permission");
    assert_eq!(
        cast_from_zone.duration,
        Some(Duration::UntilEndOfTurn),
        "CR 611.2a: the printed `Until end of turn` must reach the play permission; \
         at BASE_SHA it is None and the permission is never pruned"
    );

    let trailing = links
        .last()
        .expect("the trailing mana-spend GenericEffect is the chain leaf");
    assert!(
        matches!(&*trailing.effect, Effect::GenericEffect { .. }),
        "chain leaf is the mana-spend GenericEffect, got {:?}",
        trailing.effect
    );
    assert_eq!(
        trailing.duration,
        Some(Duration::UntilEndOfTurn),
        "CR 611.2a: the duration must reach the LAST governed link too, not just the first"
    );

    // HOSTILE ROW: the intermediate `Dig` is a ONE-SHOT the duration does not
    // govern (CR 611.2a governs continuous effects). It must stay unstamped —
    // this is what `duration_governs`'s `matches!` is for, and it is the first
    // branch a wrongly-unrestricted walk would blow through.
    let dig = links
        .iter()
        .find(|d| matches!(&*d.effect, Effect::Dig { .. }))
        .expect("the `look at the top card` Dig link");
    assert_eq!(
        dig.duration, None,
        "a one-shot Dig must NOT be stamped — duration_governs excludes it"
    );
}

/// **V5 — `[GUARD:unset-sentinel]`, SHAPE.** CR 611.2a.
///
/// **ANCHORED AGAINST NARROWING THE GUARD, NOT AGAINST BASE_SHA — it passes at
/// BASE_SHA and is a POSITIVE REACH GUARD, not a discriminator.** The named removal
/// that turns it red: narrowing `oracle_ir::ast::duration_is_unset_sentinel` so that
/// `None` is no longer in the unset set. The link's embedded window then stays
/// `None`, the leading "Until end of turn," never reaches it, and the play
/// permission lasts for the rest of the game (CR 611.2a's second sentence) instead
/// of being pruned at cleanup (CR 514.2).
///
/// Its value is that it drives a REAL CARD through the production parser, so it
/// covers the one way this PR's guard could break the PR's own headline fix.
/// **Not redundant** with `xanathar_leading_duration_reaches_governed_chain_links`,
/// which binds an `&AbilityDefinition` and asserts the CARRIER; it never reads the
/// embedded field.
///
/// The mixed-duration rows — a WRITTEN embedded window under a DIFFERING governing
/// prefix — are unit rows in `oracle_ir::ast::duration_distribution_tests_7923`
/// (`narrower_printed_window_survives_a_wider_outer_duration`, and links 4/5 of
/// `chain_duration_walks_governed_links_and_yields_to_explicit`), constructed
/// directly for the reason that file's `PreventDamage` row already gives. The PR
/// body records which effect types a card can supply that shape for today.
#[test]
fn xanathar_unset_cast_window_still_takes_the_leading_duration() {
    let parsed = parse_oracle_text(
        XANATHAR,
        "Xanathar, Guild Kingpin",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Beholder".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1, "one upkeep trigger");
    let links = chain(trigger_body(&parsed.triggers[0]));

    // Positive reach guard: the clause parsed at all.
    assert_no_unimplemented(&links, "Xanathar");

    let cast_from_zone = links
        .iter()
        .find(|d| matches!(&*d.effect, Effect::CastFromZone { .. }))
        .expect("the `you may play the top card of their library` permission");

    // THE ASSERTION THE NAMED REMOVAL TURNS RED: this link reaches
    // `duration_is_unset_sentinel` with its EMBEDDED field unset, so the leading
    // "Until end of turn," must be written into it.
    match &*cast_from_zone.effect {
        Effect::CastFromZone { duration, .. } => assert_eq!(
            duration.as_ref(),
            Some(&Duration::UntilEndOfTurn),
            "CR 611.2a: an unset embedded window takes the governing leading duration; \
             narrowing `duration_is_unset_sentinel` to exclude `None` leaves it None and \
             the play permission is never pruned"
        ),
        other => panic!("expected CastFromZone, got {other:?}"),
    }
}

/// **V6 — `[GUARD:no-injected-default]`, SHAPE.** CR 611.2a.
///
/// **ANCHORED AGAINST REINTRODUCING AN INJECTED DEFAULT, NOT AGAINST BASE_SHA — it
/// passes at BASE_SHA and is a POSITIVE REACH GUARD, not a discriminator.** The
/// named removal that turns it red: restoring
/// `let duration = duration.or(Some(Duration::UntilEndOfTurn));` in
/// `oracle_effect::imperative::try_parse_gain_keyword`.
///
/// `try_parse_gain_keyword` calls `strip_trailing_duration` FIRST, so for this card
/// the printed "until your next turn" is hoisted onto the CARRIER and the
/// recognizer's own `duration` is left `None`. Injecting `UntilEndOfTurn` there
/// mints a value BYTE-IDENTICAL to a printed one, so
/// `oracle_ir::ast::duration_is_unset_sentinel` can no longer classify the embedded
/// field as unset, the distribution declines, and the embedded window permanently
/// disagrees with the printed text.
///
/// Measured against the corpus: that injection moved 6 nodes across 5 cards — The
/// One Ring, A-The One Ring, The Stasis Coffin, Noble Heritage (x2) and Blossoming
/// Calm — from the printed `UntilNextTurnOf { Controller }` to `UntilEndOfTurn`.
///
/// **SCOPE — THIS TEST MAKES NO RUNTIME CLAIM, DELIBERATELY.**
/// `game/effects/effect.rs` resolves `ability.duration.or(embedded)`, so the
/// CARRIER wins and the INSTALLED window is correct either way. What this row
/// guards is EXPORTED PROVENANCE — card-data, the coverage report, the semantic
/// audit and the client's parse overlay all read the embedded field.
#[test]
fn one_ring_hoisted_keyword_window_reaches_the_embedded_field() {
    let parsed = parse_oracle_text(
        ONE_RING,
        "The One Ring",
        &[],
        &["Legendary".to_string(), "Artifact".to_string()],
        &[],
    );

    let printed = Duration::UntilNextTurnOf {
        player: PlayerScope::Controller,
    };

    // Positive reach guard: the protection trigger parsed at all, and is found by
    // SHAPE rather than by index so an added/reordered trigger cannot silently make
    // the assertion below vacuous.
    let grant = parsed
        .triggers
        .iter()
        .map(trigger_body)
        .flat_map(|body| chain(body).into_iter())
        .find(|d| matches!(&*d.effect, Effect::GenericEffect { .. }))
        .expect("the `you gain protection from everything until your next turn` grant");
    assert_no_unimplemented(&[grant], "The One Ring");

    // Positive reach guard: the hoist actually happened — the printed window is on
    // the carrier. Without this, the embedded assertion could pass on a clause that
    // never routed through `strip_trailing_duration` at all.
    assert_eq!(
        grant.duration.as_ref(),
        Some(&printed),
        "the printed `until your next turn` is hoisted onto the carrier"
    );

    // THE ASSERTION THE NAMED REMOVAL TURNS RED: the embedded field is unset when
    // the distribution runs, so the printed window must be written into it. With
    // the injected default restored it reads `UntilEndOfTurn` and contradicts the
    // card's printed text.
    match &*grant.effect {
        Effect::GenericEffect { duration, .. } => assert_eq!(
            duration.as_ref(),
            Some(&printed),
            "CR 611.2a: the embedded window must be the PRINTED `until your next turn`; \
             an injected `UntilEndOfTurn` default in `try_parse_gain_keyword` is \
             indistinguishable from a printed one and strands the real window"
        ),
        other => panic!("expected GenericEffect, got {other:?}"),
    }
}

/// **V7 — `[BASE]`, SHAPE.** CR 611.2a.
///
/// Temporal Aperture prints TWO bounds on one permission: an outer "Until end of
/// turn," and an inner "for as long as that card remains on top of your library".
/// The engine has no `Duration` that expresses their conjunction — `ForAsLongAs`
/// carries a condition, never a condition AND a window — and the inner condition
/// parses to `StaticCondition::Unrecognized`, which nothing can evaluate.
///
/// `cast_from_zone::resolve` reads the EMBEDDED duration and never
/// `ability.duration`, so accepting the unevaluable inner window would MASK the
/// printed outer bound: nothing would end the permission at end of turn and it
/// could stay live past it. That is strictly worse than the pre-#7959 behaviour,
/// which at least expired on time while dropping the condition.
///
/// So the shape is STRICT-FAILED. Coverage honesty (CLAUDE.md) requires the gap be
/// preserved as `Effect::unimplemented` rather than shipping a permission whose
/// lifetime is wrong; modelling the conjunction needs a new `Duration` variant and
/// is a serialized-shape change, deliberately out of scope here.
///
/// FAILS AT BASE_SHA: there the node is a live `CastFromZone` carrying the
/// unevaluable window.
#[test]
fn temporal_aperture_unevaluable_inner_window_strict_fails() {
    let parsed = parse_oracle_text(
        TEMPORAL_APERTURE,
        "Temporal Aperture",
        &[],
        &["Artifact".to_string()],
        &[],
    );

    assert_eq!(parsed.abilities.len(), 1, "one activated ability");
    let links = chain(&parsed.abilities[0]);

    // POSITIVE REACH GUARD: the ability really parsed and the chain really reached
    // the permission clause. Without this, "no CastFromZone carries an unevaluable
    // window" would pass vacuously on a card that failed to parse at all — the
    // canonical vacuous-negative in this file.
    assert!(
        links.len() > 1,
        "the shuffle/reveal chain parsed into multiple links, got {}",
        links.len()
    );
    assert!(
        links
            .iter()
            .any(|d| matches!(&*d.effect, Effect::Shuffle { .. })),
        "the `Shuffle your library` head still parses — only the permission is gapped"
    );

    // THE ASSERTION: no surviving `CastFromZone` may carry a lifetime the engine
    // cannot evaluate. At BASE_SHA the permission node holds
    // `ForAsLongAs(Unrecognized("that card remains on top of your library"))`.
    for def in &links {
        if let Effect::CastFromZone { duration, .. } = &*def.effect {
            assert!(
                !matches!(
                    duration,
                    Some(Duration::ForAsLongAs {
                        condition: StaticCondition::Unrecognized { .. }
                    })
                ),
                "CR 611.2a: a CastFromZone whose embedded window is an unevaluable \
                 condition MASKS the printed outer bound — `cast_from_zone::resolve` \
                 reads only this field, so the permission never expires. It must be \
                 strict-failed, not accepted: {duration:?}"
            );
        }
    }

    // And the gap is PRESERVED rather than silently dropped: the clause is still
    // present as an honest `Effect::Unimplemented` marker, so coverage does not
    // count this card as supported.
    assert!(
        links
            .iter()
            .any(|d| matches!(&*d.effect, Effect::Unimplemented { .. })),
        "the unsupported permission is preserved as a strict-failure marker"
    );
}

/// **V-U1g — `[BASE]`, SHAPE.** CR 611.2a; CR 615 is the
/// prevention-effects section `Effect::PreventDamage` implements.
///
/// B4 — `PreventDamage`'s `duration_governs` MEMBERSHIP writes the printed window
/// onto `AbilityDefinition.duration`, the carrier `prevent_damage::resolve` reads
/// as the `.or_else` arm of `expiry_from_duration(prevention_duration)`. It must
/// NOT write the embedded `prevention_duration` field: an UNGUARDED arm in
/// `apply_duration_to_effect` overwrites unconditionally, so an arm there would
/// clobber a narrower printed window and permanently disable that fallback — and
/// `PreventDamage` has no unset sentinel to guard on (unlike the guarded
/// `GainActivatedAbilitiesOfTarget` arm), so no guard can rescue it. Both halves are asserted here, so
/// an added arm turns this test red.
///
/// **SCOPE — THIS TEST MAKES NO RUNTIME CLAIM, DELIBERATELY.** Measured at
/// BASE_SHA: both of Kiora's prevention shields are hosted on the TARGET OBJECT's
/// live `replacement_definitions` with `base_replacement_definitions` empty, and
/// CR 613.1's top-of-pass reset (`layers.rs::seed_live_characteristics_from_base`)
/// discards BOTH — before the activation turn's own combat damage, and again
/// across the turn boundary (live 2 → 0; the opponent's 3/3 still deals its full
/// 3, identical to a paired no-activation control). Neither half survives; they go
/// together. That observation gap is a SEPARATE, PRE-EXISTING defect this PR does
/// not touch (`prevent_damage.rs`'s own comment already names it). This change puts
/// the right value on the right carrier; it cannot fix the flush.
///
/// FAILS AT BASE_SHA: there the sub node exports `duration: null`.
#[test]
fn kiora_prevention_sibling_carries_printed_window() {
    for (name, text, subtype, ability_idx) in [
        ("Kiora, the Crashing Wave", KIORA, "Kiora", 0usize),
        ("Dovin, Hand of Control", DOVIN, "Dovin", 0usize),
    ] {
        let parsed = parse_oracle_text(
            text,
            name,
            &[],
            &["Legendary".to_string(), "Planeswalker".to_string()],
            &[subtype.to_string()],
        );
        let head = &parsed.abilities[ability_idx];
        let links = chain(head);
        assert_no_unimplemented(&links, name);

        let printed = Duration::UntilNextTurnOf {
            player: PlayerScope::Controller,
        };

        // Positive reach guard: the chain did NOT collapse to one node, and the
        // head already carries the printed window at BASE. Without this, "the sub
        // carries the window" could be satisfied by a one-node chain.
        let prevents: Vec<&&AbilityDefinition> = links
            .iter()
            .filter(|d| matches!(&*d.effect, Effect::PreventDamage { .. }))
            .collect();
        assert_eq!(
            prevents.len(),
            2,
            "{name}: `dealt to AND dealt by` must lower to TWO PreventDamage links: {links:#?}"
        );
        assert_eq!(
            prevents[0].duration,
            Some(printed.clone()),
            "{name}: the head half carries the printed window at BASE already"
        );
        assert_eq!(
            prevents[1].sub_link,
            SubAbilityLink::SequentialSibling,
            "{name}: the second half is a sequential sibling of the first"
        );

        // THE REVERT-FAILING ASSERTION.
        assert_eq!(
            prevents[1].duration,
            Some(printed),
            "{name}: CR 611.2a — the `and dealt by` half must carry the printed \
             `Until your next turn`; at BASE_SHA it is None and the shield is created \
             with the engine's end-of-turn `is_shield` default instead"
        );

        // B4's other half: the embedded field must stay untouched. An added
        // `apply_duration_to_effect` arm for `PreventDamage` turns this red.
        for (i, d) in prevents.iter().enumerate() {
            match &*d.effect {
                Effect::PreventDamage {
                    prevention_duration,
                    ..
                } => assert_eq!(
                    *prevention_duration, None,
                    "{name}: link {i}'s embedded prevention_duration must stay None — \
                     `PreventDamage` deliberately has NO apply_duration_to_effect arm"
                ),
                other => panic!("{name}: expected PreventDamage, got {other:?}"),
            }
        }
    }
}

// ===========================================================================
// U2 — leading-duration conjunct expansion
// ===========================================================================

/// **V-U2c — `[BASE]`, SHAPE.** CR 611.2a + CR 608.2c.
///
/// A recovered conjunct may be ABSORBED onto the prior clause rather than emitted
/// as its own link. You Find Some Prisoners' "…, and you may spend mana as though
/// it were mana of any color to cast it" rider lands on the grant produced by the
/// preceding conjunct.
///
/// FAILS AT BASE_SHA: `mana_spend_permission` is absent there (the conjunct is
/// silently dropped).
#[test]
fn you_find_some_prisoners_recovers_mana_rider() {
    let parsed = parse_oracle_text(
        PRISONERS,
        "You Find Some Prisoners",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let grant = parsed
        .abilities
        .iter()
        .flat_map(chain)
        .find(|d| matches!(&*d.effect, Effect::GrantCastingPermission { .. }))
        .expect("the `you may play that card` grant");

    match &*grant.effect {
        Effect::GrantCastingPermission {
            permission:
                CastingPermission::PlayFromExile {
                    duration,
                    mana_spend_permission,
                    ..
                },
            ..
        } => {
            // Positive reach guard: the grant still carries its own printed
            // window, so the rider was absorbed onto a LIVE grant, not onto a
            // stub.
            assert_eq!(
                *duration,
                Duration::UntilEndOfNextTurnOf {
                    player: PlayerScope::Controller
                },
                "the grant keeps its printed `Until the end of your next turn`"
            );
            // THE REVERT-FAILING ASSERTION.
            assert_eq!(
                *mana_spend_permission,
                Some(ManaSpendPermission::AnyTypeOrColor),
                "CR 611.2a + CR 608.2c: the `spend mana as though …` conjunct must be \
                 recovered onto the grant; at BASE_SHA it is silently dropped"
            );
        }
        other => panic!("expected a PlayFromExile grant, got {other:?}"),
    }
}

/// **V-U2d integration half — `[GUARD:1+3]`, SHAPE, NEGATIVE.** CR 608.2c.
///
/// **THE PLAN LABELLED THIS ROW `[GUARD:1]`; MEASURED, THAT NAMES TOO FEW GUARDS.**
/// Removing guard 1 (`recovered_conjunct_continues_prior_subject`) ALONE leaves Aurelia
/// byte-identical corpus-wide and this test GREEN. The guard does fire 19 times across
/// 12 cards — the plan's number, reproduced exactly — but at 17 of those boundaries
/// `same_consumption` declines anyway, and at Aurelia's 2 `same_consumption` ACCEPTS,
/// leaving guard 3 (`recovered_conjunct_is_unparsed`) to decline them in the shipped
/// order. **THE REMOVAL THAT TURNS THIS TEST RED IS GUARDS 1 AND 3 TOGETHER:** with
/// both gone, Aurelia's chain carries TWO modification-bearing links instead of one,
/// failing the `mods.len() >= 3` reach guard and the `modifying.len() == 1` assertion
/// below (measured — the recovered rider comes back with `affected: SelfRef`, AURELIA
/// gaining trample when the TARGET is red). That named removal is why the tag is
/// `[GUARD:1+3]` and not `[COVER]`.
///
/// The two guards are not interchangeable and only COINCIDE on Aurelia today: guard 1
/// states a grammar fact (CR 608.2c), guard 3 a parser-coverage fact whose population is
/// expected to shrink. Beyond the guard set, this row also pins that the PR does not
/// make Aurelia's (pre-existing, out-of-scope) condition-scoping defect worse. **The
/// revert-failing content for the expansion predicate itself lives in
/// `opportunistic_dragon_riders_bind_stolen_permanent` and
/// `revenge_of_the_hunted_recovers_lure_conjunct`; guard 1's own property is
/// additionally anchored against MISCLASSIFICATION by the `[NEW-UNIT]` row
/// `oracle_effect::sequence::leading_duration_guard_tests_7923::recovered_conjunct_continuation_guard`.**
#[test]
fn aurelia_bare_conjugated_riders_are_not_reparented() {
    let parsed = parse_oracle_text(
        AURELIA,
        "Aurelia, Exemplar of Justice",
        &["Flying".to_string(), "Mentor".to_string()],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Angel".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| {
            chain(trigger_body(t)).iter().any(|d| {
                statics_of(&d.effect).iter().any(|s| {
                    s.modifications.iter().any(|m| {
                        matches!(m, ContinuousModification::AddKeyword { .. })
                            || matches!(m, ContinuousModification::AddPower { .. })
                    })
                })
            })
        })
        .expect("the begin-combat pump trigger");
    let links = chain(trigger_body(trigger));
    assert_no_unimplemented(&links, "Aurelia");

    // POSITIVE REACH GUARD — "identical" must not be "empty". The merged
    // GenericEffect really does carry the three modifications, so the negative
    // below is about re-parenting and not about a failed parse.
    let merged = links
        .iter()
        .find(|d| !statics_of(&d.effect).is_empty())
        .expect("the merged continuous-modification clause");
    let mods: Vec<&ContinuousModification> = statics_of(&merged.effect)
        .iter()
        .flat_map(|s| s.modifications.iter())
        .collect();
    assert!(
        mods.iter()
            .any(|m| matches!(m, ContinuousModification::AddPower { .. })),
        "the merged clause carries the +2/+0: {mods:#?}"
    );
    assert!(
        mods.len() >= 3,
        "the merged clause carries all three riders (+2/+0, trample, vigilance): {mods:#?}"
    );

    // THE GUARD ASSERTION: exactly ONE modification-bearing link. Removing
    // guards 1 AND 3 together splits the two conjugated riders out as siblings
    // (removing either one alone does not — see the doc comment).
    let modifying: Vec<&&AbilityDefinition> = links
        .iter()
        .filter(|d| !statics_of(&d.effect).is_empty())
        .collect();
    assert_eq!(
        modifying.len(),
        1,
        "CR 608.2c: the conjugated continuations `gains trample if it's red` / \
         `gains vigilance if it's white` must stay MERGED into the prior clause. \
         Splitting them re-parents them onto the chain default (SelfRef = Aurelia): \
         {links:#?}"
    );
    // And none of them binds SelfRef, which is the concrete misbinding.
    for s in statics_of(&merged.effect) {
        assert_ne!(
            s.affected,
            Some(TargetFilter::SelfRef),
            "a recovered rider bound to SelfRef would give AURELIA the keyword"
        );
    }
}

/// **V-U2f integration half — `[GUARD:2]`, SHAPE, NEGATIVE.**
/// CR 603.1 + CR 603.7.
///
/// **REVERT-FAILING AGAINST REMOVAL OF GUARD 2, NOT AGAINST BASE_SHA.** Giant
/// Oyster's chain contains no top-level `PutCounter`. With U2 landed and
/// `head_ends_with_dangling_phase_trigger` removed it gains
/// `PutCounter{ForAsLongAs SourceIsTapped, ParentTarget}` between the `CantUntap`
/// static and the `CreateDelayedTrigger` — because the boundary the splitter
/// found severs a mid-sentence "[At] [event], [effect]" delayed trigger in half
/// and emits its BODY as a one-shot the card never authorizes.
#[test]
fn giant_oyster_delayed_trigger_body_is_not_split() {
    let parsed = parse_oracle_text(
        GIANT_OYSTER,
        "Giant Oyster",
        &[],
        &["Creature".to_string()],
        &["Oyster".to_string()],
    );
    let activated = parsed
        .abilities
        .iter()
        .find(|a| {
            chain(a)
                .iter()
                .any(|d| matches!(&*d.effect, Effect::CreateDelayedTrigger { .. }))
        })
        .expect("the {T} activated ability");
    let links = chain(activated);
    assert_no_unimplemented(&links, "Giant Oyster");

    // POSITIVE REACH GUARDS — the chain is intact and specific, so the negative
    // below is not vacuous.
    let cant_untap = links
        .iter()
        .find(|d| !statics_of(&d.effect).is_empty())
        .expect("the `doesn't untap` continuous effect");
    assert!(
        matches!(cant_untap.duration, Some(Duration::ForAsLongAs { .. })),
        "the CantUntap static carries ForAsLongAs{{SourceIsTapped}}, got {:?}",
        cant_untap.duration
    );
    assert!(
        links
            .iter()
            .any(|d| matches!(&*d.effect, Effect::CreateDelayedTrigger { .. })),
        "the delayed trigger itself must still be built: {links:#?}"
    );

    // THE GUARD ASSERTION.
    assert!(
        !links
            .iter()
            .any(|d| matches!(&*d.effect, Effect::PutCounter { .. })),
        "CR 603.7: `at the beginning of each of your draw steps` DANGLES a delayed \
         trigger head — its body must NOT be emitted as a top-level one-shot: {links:#?}"
    );
}

/// **V-U2j integration half — `[GUARD:3]`, SHAPE, NEGATIVE.** No CR annotation,
/// deliberately: the guard states what the PARSER'S OWN OUTPUT is not evidence
/// for, exactly as `severed_prefix_end`'s existing whole-body statement does.
///
/// **REVERT-FAILING AGAINST REMOVAL OF GUARD 3, NOT AGAINST BASE_SHA.** The
/// Belligerent is byte-identical to BASE. With U2 landed and
/// `recovered_conjunct_is_unparsed` removed, the chain gains an `optional: true`
/// `Unimplemented{play, "play lands"}` node at `duration: UntilEndOfTurn`, with
/// the `CastFromZone` demoted to its `ContinuationStep` sub — i.e. the chain gets
/// restructured around an admission of ignorance, which is not recovery.
///
/// COST, STATED PLAINLY: "you may play lands" stays silently dropped, exactly as
/// at BASE_SHA. That is a PRESERVED limitation, not a new regression. The fix is
/// the `play lands` permission grammar, which retires this decline and the
/// 13-card deferred no-op-optional-prompt class together.
#[test]
fn belligerent_unparsed_conjunct_is_not_recovered() {
    let parsed = parse_oracle_text(
        BELLIGERENT,
        "The Belligerent",
        &["Crew".to_string(), "Treasure".to_string()],
        &["Legendary".to_string(), "Artifact".to_string()],
        &["Vehicle".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1, "one attack trigger");
    let links = chain(trigger_body(&parsed.triggers[0]));

    // POSITIVE REACH GUARDS — "identical to BASE" must be non-trivial and
    // SPECIFIC, or this test would pass on an empty chain.
    assert_no_unimplemented(&links, "The Belligerent");
    assert!(
        matches!(&*links[0].effect, Effect::Token { .. }),
        "head is the Treasure token, got {:?}",
        links[0].effect
    );
    let dig = links
        .iter()
        .find(|d| matches!(&*d.effect, Effect::Dig { .. }))
        .expect("the `look at the top card` Dig");
    assert_eq!(
        dig.duration,
        Some(Duration::UntilEndOfTurn),
        "the Dig carries the printed Until end of turn"
    );
    let cast = dig
        .sub_ability
        .as_deref()
        .expect("the CastFromZone is the Dig's sub");
    assert!(
        matches!(&*cast.effect, Effect::CastFromZone { .. }),
        "the `cast spells from the top of your library` permission is present, got {:?}",
        cast.effect
    );
    assert_eq!(
        cast.sub_link,
        SubAbilityLink::ContinuationStep,
        "and it is the Dig's continuation step"
    );

    // THE GUARD ASSERTION: exactly three links, none optional. Removing guard 3
    // inserts a fourth (`optional: true` `Unimplemented{play}`).
    assert_eq!(
        links.len(),
        3,
        "the chain is Token -> Dig -> CastFromZone and nothing else: {links:#?}"
    );
    assert!(
        !links.iter().any(|d| d.optional),
        "no `optional: true` node may appear — that is the shape guard 3 prevents: {links:#?}"
    );
}

/// **V-U2g — `[GUARD:U2]`, SHAPE.**
///
/// **ANCHORED AGAINST REVERTING U2's PRE-PASS WHILE KEEPING THE DELETION, NOT
/// AGAINST BASE_SHA.** The narrow `leading_host_lifetime_split` gate in
/// `parse_effect_chain_ir` is deleted; Opportunistic Dragon's post-fix chain is
/// produced by `expand_leading_duration_chunks` instead. With the gate deleted AND
/// U2 reverted, the Dragon reverts to `GainControl` alone.
///
/// The corpus counter is the other half of this row: the gate measured ZERO hits
/// across 35,798 cards once the pre-pass runs first, and leaving it live alongside
/// U2 would double-strip the Dragon's chunk.
#[test]
fn leading_host_lifetime_gate_is_subsumed() {
    let parsed = parse_oracle_text(
        OPPORTUNISTIC_DRAGON,
        "Opportunistic Dragon",
        &["Flying".to_string()],
        &["Creature".to_string()],
        &["Dragon".to_string()],
    );
    let links = chain(trigger_body(&parsed.triggers[0]));
    assert_no_unimplemented(&links, "Opportunistic Dragon");

    let gain = links
        .iter()
        .find(|d| matches!(&*d.effect, Effect::GainControl { .. }))
        .expect("the GainControl head");
    assert_eq!(
        gain.duration,
        // #8180 retyped "remains on the battlefield" from `UntilHostLeavesPlay` to
        // `WhileHostOnBattlefield`; both answer `ends_when_host_leaves_play`, so the
        // lifetime is unchanged and only the typed value moved. This row still pins
        // that the PRE-PASS produced a duration here at all -- which is the claim the
        // deleted `leading_host_lifetime_split` gate used to carry.
        Some(Duration::WhileHostOnBattlefield),
        "the printed `For as long as this creature remains on the battlefield`"
    );

    // The two recovered riders, produced by the PRE-PASS and not by the gate.
    let riders: Vec<&&AbilityDefinition> = links
        .iter()
        .filter(|d| !statics_of(&d.effect).is_empty())
        .collect();
    assert_eq!(
        riders.len(),
        2,
        "both `it loses all abilities` and `it can't attack or block` are recovered: {links:#?}"
    );
    for r in &riders {
        assert_eq!(
            r.duration,
            Some(Duration::WhileHostOnBattlefield),
            "every recovered rider carries the printed host-lifetime duration"
        );
        for s in statics_of(&r.effect) {
            assert_eq!(
                s.affected,
                Some(TargetFilter::ParentTarget),
                "CR 608.2c: a rider must bind the STOLEN permanent (ParentTarget), \
                 never the Dragon — a nested chain would bind SelfRef here"
            );
        }
    }
    let all_mods: Vec<&ContinuousModification> = riders
        .iter()
        .flat_map(|r| statics_of(&r.effect))
        .flat_map(|s| s.modifications.iter())
        .collect();
    assert!(
        all_mods
            .iter()
            .any(|m| matches!(m, ContinuousModification::RemoveAllAbilities)),
        "`it loses all abilities` -> RemoveAllAbilities: {all_mods:#?}"
    );
    for mode in [StaticMode::CantAttack, StaticMode::CantBlock] {
        assert!(
            all_mods.iter().any(
                |m| matches!(m, ContinuousModification::AddStaticMode { mode: got } if *got == mode)
            ),
            "`it can't attack or block` -> AddStaticMode {mode:?}: {all_mods:#?}"
        );
    }
}

/// CR 611.2a: `try_parse_gain_all_activated_abilities_of_target` emits the PARSED
/// duration verbatim, so a card printing no window leaves `None` — a TRUE unset
/// sentinel a governing leading duration can then stamp.
///
/// REVERT-FAILING: with that site restored to `duration.or(Some(UntilEndOfTurn))`,
/// the injected default is byte-identical to a PRINTED "until end of turn", so
/// `apply_duration_to_effect`'s unset-sentinel guard declines, the outer window is
/// silently dropped, and row 2 below reads `UntilEndOfTurn` instead of
/// `UntilNextTurnOf{Controller}` — a grant that expires a whole turn early.
///
/// Constructed directly: all three corpus cards reaching this site (Quicksilver
/// Elemental, Havengul Lich, Grell Philosopher) print a trailing window, which is
/// why removing the injected default is corpus-neutral.
#[test]
fn gain_all_activated_abilities_yields_to_a_governing_leading_duration() {
    fn granted_duration(parsed: &engine::parser::oracle::ParsedAbilities) -> Option<Duration> {
        parsed
            .abilities
            .iter()
            .chain(parsed.triggers.iter().map(trigger_body))
            .flat_map(chain)
            .find_map(|d| match &*d.effect {
                Effect::GainActivatedAbilitiesOfTarget { duration, .. } => Some(duration.clone()),
                _ => None,
            })
            .expect("a GainActivatedAbilitiesOfTarget node must be present")
    }

    // Row 1 — POSITIVE REACH GUARD: a PRINTED window still reaches the node
    // unchanged. Without this, row 2 could pass because the site stopped emitting
    // a duration at all.
    let printed = parse_oracle_text(
        "{U}: This creature gains all activated abilities of target creature until end of turn.",
        "Quicksilver Elemental",
        &[],
        &["Creature".to_string()],
        &["Shapeshifter".to_string()],
    );
    assert_eq!(
        granted_duration(&printed),
        Some(Duration::UntilEndOfTurn),
        "a printed trailing window must survive verbatim"
    );

    // Row 2 — THE DISCRIMINATING ROW: no printed window, so the leading duration
    // governs and must be the one that lands.
    let governed = parse_oracle_text(
        "Until your next turn, target creature gains all activated abilities of target creature.",
        "Constructed Governed Grant",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert_eq!(
        granted_duration(&governed),
        Some(Duration::UntilNextTurnOf {
            player: PlayerScope::Controller
        }),
        "the governing leading duration must reach the grant — an injected \
         UntilEndOfTurn default at the parser site would silently outrank it"
    );
}

/// **V-U2e — `[COVER]`, SHAPE, table-driven.**
///
/// **PASSES AT BASE_SHA UNCHANGED, BY DESIGN — this is OVER-SPLITTING cover.**
/// Each row asserts its exact post-fix chain shape, which EQUALS its BASE shape;
/// the row fails only if `severed_prefix_end` becomes over-broad. The
/// revert-failing content for the predicate lives in
/// `opportunistic_dragon_riders_bind_stolen_permanent` and
/// `revenge_of_the_hunted_recovers_lure_conjunct`.
///
/// Every row asserts an EXACT chain-link count and an EXACT total
/// `ContinuousModification` count, plus the `Effect::Unimplemented` count. The link
/// count is what stops the row degenerating into a vacuous equality: severing a
/// merged conjunct necessarily ADDS a link, so an over-splitting regression turns
/// EVERY row red — not merely the three carrying a bespoke per-card shape check
/// below. (Before this was tightened, the loop asserted only `!links.is_empty()`
/// plus the Unimplemented count, so five of the eight rows would have passed
/// unchanged through exactly the regression they exist to catch.)
#[test]
fn leading_duration_merge_cards_unchanged() {
    // (name, oracle, types, subtypes, keywords)
    struct Row {
        name: &'static str,
        text: &'static str,
        types: &'static [&'static str],
        subtypes: &'static [&'static str],
        keywords: &'static [&'static str],
        /// `Effect::Unimplemented` count at BASE_SHA, asserted UNCHANGED. Zero for
        /// every row but Kitesail Larcenist, whose granted activated ability
        /// (`"{T}, Sacrifice this artifact: Add one mana of any color"`) is a
        /// PRE-EXISTING honest-red residual this PR neither repairs nor worsens.
        /// Asserted as an exact count rather than waived, so an over-splitting
        /// regression that ADDED a placeholder here would still turn the row red.
        unimplemented: usize,
        /// Exact chain-link count. THIS is the row's anti-over-splitting assertion:
        /// severing a merged conjunct necessarily ADDS a link, so a regression in
        /// `severed_prefix_end` turns every row red, not merely the three that
        /// carry a bespoke per-card shape check below.
        links: usize,
        /// Exact total `ContinuousModification` count across all links — catches a
        /// regression that keeps the link count but drops or redistributes the
        /// modifications. Zero for the two rows whose effects are not
        /// modification-bearing (Stolen Strategy: casting permission; Embiggen:
        /// dynamic P/T), where `links` alone carries the row.
        mods: usize,
    }
    let rows = [
        Row { name: "Jump Scare", text: "Until end of turn, target creature gets +2/+2, gains flying, and becomes a Horror enchantment creature in addition to its other types.", types: &["Instant"], subtypes: &[], keywords: &[], unimplemented: 0, links: 1, mods: 6 },
        Row { name: "Stolen Strategy", text: "At the beginning of your upkeep, exile the top card of each opponent's library. Until end of turn, you may cast spells from among those exiled cards, and you may spend mana as though it were mana of any color to cast those spells.", types: &["Enchantment"], subtypes: &[], keywords: &[], unimplemented: 0, links: 2, mods: 0 },
        Row { name: "Titanic Ultimatum", text: "Until end of turn, creatures you control get +5/+5 and gain first strike, trample, and lifelink.", types: &["Sorcery"], subtypes: &[], keywords: &[], unimplemented: 0, links: 1, mods: 5 },
        Row { name: "Embiggen", text: "Until end of turn, target non-Brushwagg creature gets +1/+1 for each supertype, card type, and subtype it has.", types: &["Instant"], subtypes: &[], keywords: &[], unimplemented: 0, links: 1, mods: 0 },
        Row { name: "Sylvan Awakening", text: "Until your next turn, all lands you control become 2/2 Elemental creatures with reach, indestructible, and haste. They're still lands.", types: &["Sorcery"], subtypes: &[], keywords: &["Indestructible"], unimplemented: 0, links: 2, mods: 9 },
        Row { name: "Kitesail Larcenist", text: "Flying, ward {1}\nWhen this creature enters, for each player, choose up to one other target artifact or creature that player controls. For as long as this creature remains on the battlefield, the chosen permanents become Treasure artifacts with \"{T}, Sacrifice this artifact: Add one mana of any color\" and lose all other abilities.", types: &["Creature"], subtypes: &["Human", "Pirate"], keywords: &["Flying", "Ward"], unimplemented: 1, links: 2, mods: 5 },
        Row { name: "Dominaria's Judgment", text: "Until end of turn, creatures you control gain protection from white if you control a Plains, from blue if you control an Island, from black if you control a Swamp, from red if you control a Mountain, and from green if you control a Forest.", types: &["Instant"], subtypes: &[], keywords: &[], unimplemented: 0, links: 1, mods: 5 },
        Row { name: "Arm the Cathars", text: "Until end of turn, target creature gets +3/+3, up to one other target creature gets +2/+2, and up to one other target creature gets +1/+1. Those creatures gain vigilance until end of turn.", types: &["Sorcery"], subtypes: &[], keywords: &[], unimplemented: 0, links: 2, mods: 1 },
    ];

    for row in rows {
        let parsed = parse_oracle_text(
            row.text,
            row.name,
            &row.keywords
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            &row.types.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &row.subtypes
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        );
        let links: Vec<&AbilityDefinition> = parsed
            .abilities
            .iter()
            .chain(parsed.triggers.iter().map(trigger_body))
            .flat_map(chain)
            .collect();
        assert!(!links.is_empty(), "{}: parsed to nothing", row.name);
        assert_eq!(
            links.len(),
            row.links,
            "{}: chain-link count must be UNCHANGED — a higher count is the \
             over-splitting regression this row exists to catch: {links:#?}",
            row.name
        );
        let mods: usize = links
            .iter()
            .flat_map(|d| statics_of(&d.effect))
            .flat_map(|s| s.modifications.iter())
            .count();
        assert_eq!(
            mods, row.mods,
            "{}: total ContinuousModification count must be UNCHANGED: {links:#?}",
            row.name
        );
        let unimplemented = links
            .iter()
            .filter(|d| matches!(&*d.effect, Effect::Unimplemented { .. }))
            .count();
        assert_eq!(
            unimplemented, row.unimplemented,
            "{}: Effect::Unimplemented count must be UNCHANGED from BASE_SHA: {links:#?}",
            row.name
        );
    }

    // Per-card POSITIVE shapes. Each is the merge the predicate must NOT split.
    let jump_scare = parse_oracle_text(
        "Until end of turn, target creature gets +2/+2, gains flying, and becomes a Horror enchantment creature in addition to its other types.",
        "Jump Scare", &[], &["Instant".to_string()], &[],
    );
    let js_links: Vec<&AbilityDefinition> = jump_scare.abilities.iter().flat_map(chain).collect();
    let js_modifying: Vec<&&AbilityDefinition> = js_links
        .iter()
        .filter(|d| !statics_of(&d.effect).is_empty())
        .collect();
    assert_eq!(
        js_modifying.len(),
        1,
        "Jump Scare: all mods stay in ONE GenericEffect: {js_links:#?}"
    );
    assert!(
        statics_of(&js_modifying[0].effect)
            .iter()
            .flat_map(|s| s.modifications.iter())
            .count()
            >= 3,
        "Jump Scare: +2/+2, flying and the type change are all present"
    );

    let judgment = parse_oracle_text(
        "Until end of turn, creatures you control gain protection from white if you control a Plains, from blue if you control an Island, from black if you control a Swamp, from red if you control a Mountain, and from green if you control a Forest.",
        "Dominaria's Judgment", &[], &["Instant".to_string()], &[],
    );
    let dj_links: Vec<&AbilityDefinition> = judgment.abilities.iter().flat_map(chain).collect();
    let dj_keywords = dj_links
        .iter()
        .flat_map(|d| statics_of(&d.effect))
        .flat_map(|s| s.modifications.iter())
        .filter(|m| matches!(m, ContinuousModification::AddKeyword { .. }))
        .count();
    assert_eq!(
        dj_keywords, 5,
        "Dominaria's Judgment: five AddKeyword protections, and it must NOT degrade \
         to a static: {dj_links:#?}"
    );

    let stolen = parse_oracle_text(
        "At the beginning of your upkeep, exile the top card of each opponent's library. Until end of turn, you may cast spells from among those exiled cards, and you may spend mana as though it were mana of any color to cast those spells.",
        "Stolen Strategy", &[], &["Enchantment".to_string()], &[],
    );
    let ss_links: Vec<&AbilityDefinition> = stolen
        .triggers
        .iter()
        .map(trigger_body)
        .flat_map(chain)
        .collect();
    assert!(
        ss_links.iter().any(|d| matches!(
            &*d.effect,
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    mana_spend_permission: Some(_),
                    ..
                },
                ..
            }
        )),
        "Stolen Strategy: the mana-spend rider stays MERGED onto the grant: {ss_links:#?}"
    );

    // Arm the Cathars parses to ONE chunk before the predicate — `sub.len() < 2`
    // early-returns, so the predicate must never even be reached.
    let cathars = parse_oracle_text(
        "Until end of turn, target creature gets +3/+3, up to one other target creature gets +2/+2, and up to one other target creature gets +1/+1. Those creatures gain vigilance until end of turn.",
        "Arm the Cathars", &[], &["Sorcery".to_string()], &[],
    );
    assert!(
        !cathars.abilities.is_empty(),
        "Arm the Cathars must still parse"
    );

    // Xanathar and Abeyance: the PREDICATE must DECLINE — their recognizers
    // represent the tail as their OWN chain, which is U1's case, not U2's.
    let xan = parse_oracle_text(
        XANATHAR,
        "Xanathar, Guild Kingpin",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Beholder".to_string()],
    );
    assert_eq!(
        chain(trigger_body(&xan.triggers[0])).len(),
        5,
        "Xanathar's chain is the recognizer's own five links — the predicate must not \
         re-chunk it"
    );
    let abey = parse_oracle_text(ABEYANCE, "Abeyance", &[], &["Instant".to_string()], &[]);
    assert_eq!(
        chain(&abey.abilities[0]).len(),
        3,
        "Abeyance's chain is the recognizer's own three links"
    );
}

// ===========================================================================
// RUNTIME rows — §G1's resolution-driver discipline applies to every one:
// never assert on a prompt, always set the optional policy explicitly, and
// where the claim is that two branches differ, drive both with the ACCEPT
// branch written first as the positive control.
// ===========================================================================

/// **V-U2a — `[BASE]`, RUNTIME. THE discriminating test.**
/// CR 611.2a + CR 608.2c.
///
/// Opportunistic Dragon's printed sentence is
/// "For as long as this creature remains on the battlefield, gain control of that
/// permanent, it loses all abilities, and it can't attack or block." At BASE_SHA
/// the leading duration glues the whole body into one chunk and the single-clause
/// arm keeps only `GainControl`, silently dropping BOTH riders: the stolen
/// permanent keeps every ability and can attack and block freely.
///
/// FAILS AT BASE_SHA: the two rider assertions are `GainControl`-only there.
///
/// The riders must bind the STOLEN permanent (`ParentTarget`), not the Dragon —
/// which is precisely why the expansion is a chunk-level PRE-PASS rather than a
/// nested `parse_effect_chain_ir` (a nested chain restarts anaphor state and binds
/// `SelfRef`, i.e. it would blank the DRAGON).
#[test]
fn opportunistic_dragon_riders_bind_stolen_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, ample_mana());
    let dragon = scenario
        .add_creature_to_hand_from_oracle(P0, "Opportunistic Dragon", 4, 3, OPPORTUNISTIC_DRAGON)
        .id();
    // The victim: a Human whose ONE ability is a tap-to-draw, so a surviving
    // ability is directly observable, and which is otherwise able to attack.
    let victim = scenario
        .add_creature_from_oracle(P1, "Stolen Human", 3, 3, "{T}: Draw a card.")
        .with_subtypes(vec!["Human"])
        .id();
    // HOSTILE FIXTURE: a second, UNSTOLEN Human of the same shape. If the riders
    // leaked past `ParentTarget` onto a filter, this one would be hit too.
    let bystander = scenario
        .add_creature_from_oracle(P1, "Untouched Human", 3, 3, "{T}: Draw a card.")
        .with_subtypes(vec!["Human"])
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(dragon)
        .target_object(victim)
        .decline_optional()
        .resolve();

    // POSITIVE REACH GUARD (a): control actually changed. Without this the two
    // rider assertions below could pass on a trigger that never resolved.
    assert_eq!(
        outcome.controller(victim),
        P0,
        "the ETB trigger must have resolved and moved control of the victim"
    );
    // POSITIVE REACH GUARD (b): the ANTECEDENT guard — the Dragon itself keeps
    // flying and is NOT blanked. This is the misbinding the pre-pass placement
    // exists to prevent.
    let dragon_obj = &outcome.state().objects[&dragon];
    assert!(
        has_keyword(dragon_obj, &Keyword::Flying),
        "the DRAGON must keep flying — a rider bound to SelfRef would blank it"
    );
    assert!(
        !creature_cant_attack(outcome.state(), dragon),
        "the DRAGON must still be able to attack"
    );

    // THE REVERT-FAILING ASSERTIONS.
    let victim_obj = &outcome.state().objects[&victim];
    assert!(
        victim_obj.abilities.is_empty(),
        "CR 611.2a: `it loses all abilities` must apply to the STOLEN permanent; \
         at BASE_SHA the conjunct is dropped and it keeps `{{T}}: Draw a card`: \
         abilities={:?}",
        victim_obj.abilities
    );
    assert!(
        creature_cant_attack(outcome.state(), victim),
        "CR 611.2a: `it can't attack or block` must apply to the stolen permanent; \
         at BASE_SHA the conjunct is dropped"
    );

    // HOSTILE ROW: the bystander is untouched on every axis.
    let bystander_obj = &outcome.state().objects[&bystander];
    assert_eq!(
        outcome.controller(bystander),
        P1,
        "the unstolen Human's controller is unchanged"
    );
    assert!(
        !bystander_obj.abilities.is_empty(),
        "the unstolen Human keeps its ability — the riders are ParentTarget-scoped"
    );
    assert!(
        !creature_cant_attack(outcome.state(), bystander),
        "the unstolen Human can still attack"
    );
}

/// **V-U2a (b) — `[BASE]`, RUNTIME.** CR 611.2a: `WhileHostOnBattlefield`
/// (retyped from `UntilHostLeavesPlay` by #8180; same `ends_when_host_leaves_play`
/// boundary).
///
/// All three effects share ONE printed duration, so they must expire TOGETHER
/// when the Dragon leaves the battlefield. At BASE_SHA only `GainControl` exists,
/// so only control reverts and the "riders revert too" claim is unobservable.
#[test]
fn opportunistic_dragon_riders_expire_with_the_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, ample_mana());
    let dragon = scenario
        .add_creature_to_hand_from_oracle(P0, "Opportunistic Dragon", 4, 3, OPPORTUNISTIC_DRAGON)
        .id();
    let victim = scenario
        .add_creature_from_oracle(P1, "Stolen Human", 3, 3, "{T}: Draw a card.")
        .with_subtypes(vec!["Human"])
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(dragon)
        .target_object(victim)
        .decline_optional()
        .resolve();
    // POSITIVE CONTROL: all three effects are live before the host leaves.
    assert_eq!(outcome.controller(victim), P0);
    assert!(outcome.state().objects[&victim].abilities.is_empty());
    assert!(creature_cant_attack(outcome.state(), victim));

    // Destroy the host through the production cast/zone-change pipeline, then
    // let normal priority and layer processing settle its host-lifetime effects.
    runner.cast(murder).target_object(dragon).resolve();
    runner.advance_until_stack_empty();

    // All three revert TOGETHER — one printed duration, one expiry.
    assert_eq!(
        runner.state().objects[&victim].controller,
        P1,
        "control reverts when the host leaves (this half works at BASE too)"
    );
    let victim_obj = &runner.state().objects[&victim];
    assert!(
        !victim_obj.abilities.is_empty(),
        "CR 611.2a: `it loses all abilities` expires with the SAME host-lifetime \
         duration — the ability must come back"
    );
    assert!(
        !creature_cant_attack(runner.state(), victim),
        "CR 611.2a: `it can't attack or block` expires with the same duration"
    );
}

/// **V-U2b — `[BASE]`, RUNTIME.** CR 611.2a + CR 608.2c.
///
/// Revenge of the Hunted: "Until end of turn, target creature gets +6/+6 and gains
/// trample, and all creatures able to block it this turn do so." At BASE_SHA the
/// lure conjunct is silently dropped — and r1's raw-equality predicate could not
/// recover it, which is why `same_consumption` normalizes `StaticDefinition`'s
/// pure-provenance `description` and NOTHING else.
///
/// FAILS AT BASE_SHA: `MustBeBlockedByAll` is absent there.
#[test]
fn revenge_of_the_hunted_recovers_lure_conjunct() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, ample_mana());
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Revenge of the Hunted", false, REVENGE)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_object(bear)
        .decline_optional()
        .resolve();

    // POSITIVE REACH GUARD: the head still MERGES — +6/+6 and trample both land.
    // This is what guards against OVER-splitting: if the predicate cut too early
    // the head would lose the trample.
    outcome.assert_power_toughness(bear, 8, 8);
    assert!(
        has_keyword(&outcome.state().objects[&bear], &Keyword::Trample),
        "the head conjunct's trample must survive the split"
    );

    // THE REVERT-FAILING ASSERTION: the recovered lure conjunct.
    assert!(
        object_has_static_mode(outcome.state(), bear, |m| matches!(
            m,
            StaticMode::MustBeBlockedByAll { .. }
        )),
        "CR 608.2c: `and all creatures able to block it this turn do so` must be \
         recovered; at BASE_SHA it is silently dropped"
    );

    // HOSTILE ROW: after cleanup the lure, the +6/+6 AND the trample are all gone
    // TOGETHER — one printed duration (CR 514.2).
    cross_turn_boundary(&mut runner);
    let after = runner.state();
    assert!(
        !object_has_static_mode(after, bear, |m| matches!(
            m,
            StaticMode::MustBeBlockedByAll { .. }
        )),
        "CR 514.2: the recovered lure must expire at cleanup with its siblings"
    );
    assert!(
        !has_keyword(&after.objects[&bear], &Keyword::Trample),
        "CR 514.2: the trample expires too"
    );
    let obj = &after.objects[&bear];
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(2), Some(2)),
        "CR 514.2: the +6/+6 expires too — all three end together"
    );
}

/// **V-U1b — `[BASE]`, RUNTIME.** CR 611.2a + CR 514.2.
///
/// The permission actually EXPIRES. Xanathar's "you may play the top card of their
/// library" lowers to `Effect::CastFromZone { mode: Play }`, and
/// `cast_from_zone::resolve`'s install site computes the installed
/// `CastingPermission::PlayFromExile.duration` as
/// `effect.duration.unwrap_or_else(|| <Graveyard|Hand> ? UntilEndOfTurn : Permanent)`.
/// The card is in EXILE by then, so at BASE_SHA — where U1's stamp never reached
/// this link — the permission is installed as `Duration::Permanent`, which
/// `layers::prune_end_of_turn_casting_permissions` explicitly RETAINS. The printed
/// "Until end of turn" permission therefore lasted for the rest of the game.
///
/// FAILS AT BASE_SHA on both assertions: the permission is installed as
/// `Permanent` and is still present after the turn boundary.
///
/// SCOPE NOTE, stated rather than papered over: Xanathar's `Dig`/`CastFromZone`
/// links currently lower with `player: Controller` / `target: Any`, so the
/// permission lands on the ACTIVATOR's own top card rather than the targeted
/// opponent's. That mis-scoping is PRE-EXISTING and untouched by this PR (it is
/// identical at BASE_SHA), so this test deliberately asserts on the permission's
/// DURATION wherever it lands, and makes no claim about whose library it reads.
#[test]
fn xanathar_play_permission_expires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Xanathar, Guild Kingpin", 5, 6, XANATHAR)
        .with_subtypes(vec!["Beholder"]);
    for i in 0..30 {
        scenario.add_card_to_library_top(P0, &format!("MyLib{i}"));
        scenario.add_card_to_library_top(P1, &format!("OppLib{i}"));
    }
    let mut runner = scenario.build();

    // Cycle to the controller's NEXT upkeep so the printed trigger fires through
    // the real turn machinery, and ACCEPT its optional links explicitly (§G1
    // rule 2): the `you may look` Dig is `optional: true` and the play permission
    // is its `ContinuationStep`, so a decline would skip the link under test
    // entirely and this test would pass vacuously.
    run_turns_accepting_optionals(&mut runner, 2);

    // POSITIVE REACH GUARD: the permission is actually INSTALLED, and installed
    // with the printed window. Without this the "gone" assertion below would pass
    // on a trigger that never resolved.
    let (host, installed) = find_play_permission(runner.state())
        .expect("the `you may play the top card` permission must be installed");
    assert_eq!(
        installed,
        Duration::UntilEndOfTurn,
        "CR 611.2a: the permission must be installed with the PRINTED window; at \
         BASE_SHA `effect.duration` is None and the install site falls back to \
         Duration::Permanent, which prune_end_of_turn_casting_permissions RETAINS"
    );
    let _ = host;

    // THE REVERT-FAILING ASSERTION.
    cross_turn_boundary(&mut runner);
    assert!(
        find_play_permission(runner.state()).is_none(),
        "CR 514.2: the play permission must be pruned at cleanup; at BASE_SHA it is \
         installed as Duration::Permanent and survives the rest of the game"
    );
}

/// **V-U1c SHAPE half — `[BASE]`.** CR 611.2a.
///
/// The rule is NOT `CastFromZone`-specific: Abeyance's SECOND prohibition ("and
/// that player can't activate abilities that aren't mana abilities") is a
/// `SequentialSibling` built by the same recognizer, and must carry the printed
/// duration too.
///
/// **THIS SHAPE TEST CARRIES V-U1c's REVERT-FAILING CONTENT.** The paired RUNTIME
/// test below is `[COVER]` and says why.
///
/// FAILS AT BASE_SHA: the second `AddRestriction` link carries `duration: null`.
#[test]
fn abeyance_second_prohibition_carries_the_printed_duration() {
    let parsed = parse_oracle_text(ABEYANCE, "Abeyance", &[], &["Instant".to_string()], &[]);
    let links = chain(&parsed.abilities[0]);
    assert_no_unimplemented(&links, "Abeyance");

    let prohibitions: Vec<&&AbilityDefinition> = links
        .iter()
        .filter(|d| matches!(&*d.effect, Effect::AddRestriction { .. }))
        .collect();
    // POSITIVE REACH GUARD: BOTH prohibitions exist and the FIRST already carries
    // the duration at BASE — so "the second carries it" is a real delta.
    assert_eq!(
        prohibitions.len(),
        2,
        "both printed prohibitions must be built: {links:#?}"
    );
    assert_eq!(
        prohibitions[0].duration,
        Some(Duration::UntilEndOfTurn),
        "the head prohibition carries the printed duration at BASE already"
    );

    // THE REVERT-FAILING ASSERTION.
    assert_eq!(
        prohibitions[1].duration,
        Some(Duration::UntilEndOfTurn),
        "CR 611.2a: the printed `Until end of turn` governs BOTH conjuncts; at \
         BASE_SHA the second AddRestriction is emitted with duration: None"
    );

    // HOSTILE ROW: the trailing `Draw` is a printed SequentialSibling one-shot the
    // duration does NOT govern — `duration_governs` must keep it unstamped.
    let draw = links
        .iter()
        .find(|d| matches!(&*d.effect, Effect::Draw { .. }))
        .expect("the `Draw a card.` sentence");
    assert_eq!(
        draw.sub_link,
        SubAbilityLink::SequentialSibling,
        "`Draw a card.` is its own printed sentence"
    );
    assert_eq!(
        draw.duration, None,
        "a one-shot Draw must never be stamped — duration_governs excludes it"
    );
}

/// **V-U1c RUNTIME half — `[COVER]`.** CR 514.2.
///
/// **PASSES AT BASE_SHA UNCHANGED, BY DESIGN AND BY MEASUREMENT — and the plan's
/// matrix labelled this row `[BASE]`, which is WRONG.** Measured on the corpus
/// export, Abeyance's ONLY change is `AbilityDefinition.duration` on the second
/// link; the `GameRestriction::ProhibitActivity.expiry` value that
/// `add_restriction::resolve` actually pushes into `state.restrictions` is
/// `RestrictionExpiry::EndOfTurn` at BOTH BASE and HEAD. And
/// `add_restriction::fill_runtime_fields` only OVERRIDES that expiry for
/// `Duration::UntilNextTurnOf{Controller}` and `Duration::UntilEndOfNextTurnOf{Controller}`
/// — a `Some(Duration::UntilEndOfTurn)` falls through its `_ => {}` arm. So the
/// pushed restriction is value-identical on both sides and this row's runtime
/// outcome cannot differ.
///
/// It is still the right test to write: it is the empirical backstop proving the
/// AST repair costs no behaviour, and its positive controls show the prohibitions
/// genuinely bite rather than being asserted into existence. **The revert-failing
/// content lives in `abeyance_second_prohibition_carries_the_printed_duration`.**
#[test]
fn abeyance_prohibitions_bite_and_end_at_the_turn_boundary() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, ample_mana());
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Abeyance", true, ABEYANCE)
        .id();
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("MyLib{i}"));
        scenario.add_card_to_library_top(P1, &format!("OppLib{i}"));
    }
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .decline_optional()
        .resolve();

    // POSITIVE CONTROLS: both printed prohibitions actually bite on P1 — the
    // cast ban AND the activate ban. Without these the expiry assertion below
    // would pass on a resolution that installed nothing.
    assert!(
        player_is_prohibited(outcome.state(), P1, |a| matches!(
            a,
            ProhibitedActivity::CastSpells { .. }
        )),
        "the printed cast prohibition must bite on the targeted player"
    );
    assert!(
        player_is_prohibited(outcome.state(), P1, |a| matches!(
            a,
            ProhibitedActivity::ActivateAbilities { .. }
        )),
        "the printed activate-abilities prohibition must bite on the targeted player \
         — this is the SECOND conjunct, the one U1 repairs at the AST layer"
    );
    // The trailing printed sentence still resolves and is NOT swallowed.
    outcome.assert_hand_drawn(P0, 1);

    // Both end together at the turn boundary (CR 514.2).
    cross_turn_boundary(&mut runner);
    assert!(
        !player_is_prohibited(runner.state(), P1, |a| matches!(
            a,
            ProhibitedActivity::CastSpells { .. }
        )),
        "CR 514.2: the cast prohibition ends at cleanup"
    );
    assert!(
        !player_is_prohibited(runner.state(), P1, |a| matches!(
            a,
            ProhibitedActivity::ActivateAbilities { .. }
        )),
        "CR 514.2: the activate prohibition ends at cleanup"
    );
}

/// **V-U1d — `[BASE]`, RUNTIME.** CR 611.2a + CR 514.2.
///
/// The `Permanent` sentinel YIELDS to a stated duration. Mondo Gecko's activated
/// ability lowers to `Choose{Color}` with a `ContinuationStep`
/// `Effect::GenericEffect` sub whose EMBEDDED duration is `Duration::Permanent` —
/// the become-X builder's "no duration stated" sentinel, named in
/// `with_clause_duration`'s own comment. At BASE_SHA the printed
/// "Until end of turn" never reached that sub, so the colour change and the
/// hexproof-from-that-colour were installed PERMANENTLY.
///
/// FAILS AT BASE_SHA: the grant is still live after the turn boundary.
///
/// PROVENANCE (G9): Mondo Gecko's `Permanent` comes from `build_become_clause`,
/// i.e. it takes `apply_duration_to_effect`'s `GenericEffect` arm — NOT the
/// `GainActivatedAbilitiesOfTarget` arm, which no corpus card reaches and which is
/// unit-covered only (`ast::duration_distribution_tests_7923`).
#[test]
fn stated_duration_yields_to_the_permanent_sentinel() {
    const MONDO: &str = "{1}, Discard a card: Until end of turn, Mondo Gecko becomes the color of your choice and gains hexproof from that color.\nWhenever Mondo Gecko deals combat damage to a player, draw a card for each color among permanents you control.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, ample_mana());
    let gecko = scenario
        .add_creature_from_oracle(P0, "Mondo Gecko", 2, 3, MONDO)
        .with_subtypes(vec!["Lizard", "Mutant"])
        .id();
    let fodder = scenario.add_card_to_hand(P0, "Discard Fodder");
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("MyLib{i}"));
        scenario.add_card_to_library_top(P1, &format!("OppLib{i}"));
    }
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: gecko,
            ability_index: 0,
        })
        .expect("activate Mondo Gecko");
    // Drive the announced cost and the printed colour choice explicitly. §G1
    // rule 2 in spirit: no prompt is left to a default.
    for _ in 0..24 {
        let r = match &runner.state().waiting_for {
            WaitingFor::PayCost { .. } => runner.act(GameAction::SelectCards {
                cards: vec![fodder],
            }),
            WaitingFor::NamedChoice { .. } => runner.act(GameAction::ChooseOption {
                choice: "Blue".to_string(),
            }),
            WaitingFor::OptionalEffectChoice { .. } => {
                runner.act(GameAction::DecideOptionalEffect { accept: true })
            }
            _ if runner.stack_names().is_empty() => break,
            _ => runner.act(GameAction::PassPriority),
        };
        if r.is_err() {
            break;
        }
    }

    // POSITIVE REACH GUARD: the become-clause sub actually RESOLVED and installed
    // its continuous effect on the Gecko, carrying the printed window. Without
    // this the expiry assertion below would pass on an activation that fizzled.
    let live: Vec<&TransientContinuousEffect> = gecko_grants(runner.state(), gecko);
    assert_eq!(
        live.len(),
        1,
        "the become-colour grant must be installed on the Gecko: {:?}",
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .collect::<Vec<_>>()
    );
    assert!(
        live[0].modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::HexproofFrom(_)
            }
        )),
        "the installed grant is the printed hexproof-from-that-colour one: {:?}",
        live[0].modifications
    );

    // THE REVERT-FAILING ASSERTION, on the runtime carrier itself.
    assert_eq!(
        live[0].duration,
        Duration::UntilEndOfTurn,
        "CR 611.2a: the printed `Until end of turn` must reach the become-clause \
         sub; at BASE_SHA its embedded duration stays Duration::Permanent and the \
         installed effect is permanent"
    );

    cross_turn_boundary(&mut runner);
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert!(
        gecko_grants(runner.state(), gecko).is_empty(),
        "CR 514.2: the grant must end at cleanup; a `Duration::Permanent` \
         grant (BASE_SHA's shape) survives the boundary: {:?}",
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .collect::<Vec<_>>()
    );
}

/// Attack with `attacker` and drive the resulting trigger with an EXPLICIT
/// optional policy (§G1 rules 1–2: never assert on a prompt, never default the
/// policy). Returns `(hand size, library size)` for `P0` once the stack settles.
fn attack_and_answer_optional(
    runner: &mut GameRunner,
    attacker: ObjectId,
    accept: bool,
) -> (usize, usize) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare the attack");
    for _ in 0..40 {
        let r = match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner.act(GameAction::DecideOptionalEffect { accept })
            }
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            _ if runner.stack_names().is_empty() => break,
            _ => runner.act(GameAction::PassPriority),
        };
        if r.is_err() {
            break;
        }
    }
    let st = runner.state();
    (
        st.objects
            .values()
            .filter(|o| o.owner == P0 && o.zone == Zone::Hand)
            .count(),
        st.objects
            .values()
            .filter(|o| o.owner == P0 && o.zone == Zone::Library)
            .count(),
    )
}

fn ambergris_scenario() -> (GameRunner, ObjectId) {
    // VERBATIM, BOTH LINES (`/card-test`). The `Haste` line lowers to a keyword
    // and never touches the chain under test, so it is carried rather than
    // paraphrased away.
    const AMBERGRIS: &str = "Haste\nWhenever Ambergris, Agent of Progress attacks, you may discard your hand and draw three cards.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let amber = scenario
        .add_creature_from_oracle(P0, "Ambergris, Agent of Progress", 4, 3, AMBERGRIS)
        .with_subtypes(vec!["Dwarf", "Cleric"])
        .id();
    for i in 0..5 {
        scenario.add_card_to_hand(P0, &format!("Hand{i}"));
    }
    for i in 0..20 {
        scenario.add_card_to_library_top(P0, &format!("Lib{i}"));
        scenario.add_card_to_library_top(P1, &format!("OppLib{i}"));
    }
    let runner = scenario.build();
    (runner, amber)
}

/// **V-U2e2 — `[COVER]`, RUNTIME.** CR 608.2d (the optional-effect decision) read
/// with CR 608.2c.
///
/// **PASSES AT BASE_SHA — it characterizes an EXISTING engine axis this PR does
/// not change.** With an OPTIONAL parent and `sub_link == ContinuationStep`, a
/// decline skips the sub; with `SequentialSibling` the sub would still resolve.
/// Its job is to make §5.9's `ContinuationStep` ruling for Memory Vessel an
/// empirically grounded choice rather than an asserted one — the axis is real, and
/// Memory Vessel is measured not to be on it (its parent is non-optional, shown
/// behaviourally in `memory_vessel_parsed_chain_grants_and_prohibits`).
/// **The revert-failing content is `memory_vessel_oracle_text_lowers_fully`'s
/// `sub_link` assertion.**
///
/// §G1 rule 3: the ACCEPT branch is written FIRST and is this test's own positive
/// control — it proves the harness reaches the sub at all. Ambergris is one of 15
/// corpus members of the `Discard -> ContinuationStep Draw` shape; accept and
/// decline differ by the entire hand.
#[test]
fn continuation_step_is_skipped_when_optional_parent_declined() {
    // ACCEPT FIRST — the positive control.
    let (mut runner, amber) = ambergris_scenario();
    let (hand_before, lib_before) = {
        let st = runner.state();
        (
            st.objects
                .values()
                .filter(|o| o.owner == P0 && o.zone == Zone::Hand)
                .count(),
            st.objects
                .values()
                .filter(|o| o.owner == P0 && o.zone == Zone::Library)
                .count(),
        )
    };
    assert_eq!(hand_before, 5, "the fixture starts with a five-card hand");
    let (hand_accept, lib_accept) = attack_and_answer_optional(&mut runner, amber, true);
    assert_eq!(
        hand_accept, 3,
        "ACCEPT: the whole hand is discarded and exactly three cards are drawn — \
         this is the positive control that the ContinuationStep sub is REACHED"
    );
    assert_eq!(
        lib_before - lib_accept,
        3,
        "ACCEPT: the three drawn cards came from the library"
    );

    // DECLINE — the same axis, the other branch.
    let (mut runner, amber) = ambergris_scenario();
    let (hand_decline, lib_decline) = attack_and_answer_optional(&mut runner, amber, false);
    assert_eq!(
        hand_decline, hand_before,
        "DECLINE: the hand is untouched — CR 608.2d, the printed `you may` was declined"
    );
    assert_eq!(
        lib_decline, lib_before,
        "DECLINE: the `ContinuationStep` draw is SKIPPED with its parent — this is \
         the axis §5.9's ContinuationStep ruling for Memory Vessel sits on"
    );
    assert_ne!(
        hand_accept, hand_decline,
        "the two branches must differ observably, or this row has no discriminating \
         observable at all"
    );
}

/// **V-U1f — `[COVER]`, SHAPE + RUNTIME.** CR 611.2a.
///
/// **PASSES AT BASE_SHA UNCHANGED, BY DESIGN.** `ForceAttack` is a member of
/// `duration_governs` (so a stated duration reaches `AbilityDefinition.duration`)
/// but deliberately has NO `apply_duration_to_effect` arm, because
/// `Effect::ForceAttack.duration` is NON-`Option` with a serde default of
/// `UntilEndOfTurn` — a printed window and the default are indistinguishable
/// there, so writing it would clobber a printed one (Silver Surfer's "each combat
/// if able"). This row's job is to prove that exclusion COSTS NOTHING, and
/// "nothing changed" IS the assertion.
///
/// **The revert-failing content for `ForceAttack` lives in
/// `oracle_ir::ast::duration_distribution_tests_7923::narrower_printed_window_survives_a_wider_outer_duration`**,
/// which turns red the moment an arm is added.
///
/// Nothing covers `ForceBlock`, and by design: `force_block::resolve` reads the
/// duration ONLY out of `ability.effect` and never consults `ability.duration`, so
/// its `duration_governs` membership is a KNOWINGLY INERT STAMP pending a named
/// follow-up (blast radius measured at 5 cards, all UntilEndOfTurn-under-UntilEndOfTurn).
#[test]
fn gideon_jura_force_attack_uses_ability_duration() {
    const GIDEON: &str = "[+2]: During target opponent's next turn, creatures that player controls attack Gideon Jura if able.\n[−2]: Destroy target tapped creature.\n[0]: Until end of turn, Gideon Jura becomes a 6/6 Human Soldier creature that's still a planeswalker. Prevent all damage that would be dealt to him this turn.";

    // SHAPE half: the printed window lands on `AbilityDefinition.duration` and the
    // EMBEDDED field keeps its serde default. An added `ForceAttack` arm would
    // overwrite the embedded field at EVERY `with_clause_duration` call site, and
    // this assertion is what catches that.
    let parsed = parse_oracle_text(
        GIDEON,
        "Gideon Jura",
        &[],
        &["Legendary".to_string(), "Planeswalker".to_string()],
        &["Gideon".to_string()],
    );
    let plus_two = parsed
        .abilities
        .iter()
        .find(|a| matches!(&*a.effect, Effect::ForceAttack { .. }))
        .expect("the +2 ForceAttack ability");
    assert_eq!(
        plus_two.duration,
        Some(Duration::UntilEndOfNextTurnOf {
            player: PlayerScope::Target
        }),
        "the printed `During target opponent's next turn` reaches ability.duration — \
         the carrier force_attack::resolve consults FIRST"
    );
    match &*plus_two.effect {
        Effect::ForceAttack { duration, .. } => assert_eq!(
            *duration,
            Duration::UntilEndOfTurn,
            "the EMBEDDED field must keep its `default_duration_until_end_of_turn` \
             serde default — unchanged from BASE_SHA. An added apply_duration_to_effect \
             arm would clobber it (and would clobber Silver Surfer's printed \
             `each combat` the same way)"
        ),
        other => panic!("expected ForceAttack, got {other:?}"),
    }

    // RUNTIME half: the requirement actually BINDS, and it binds with the window
    // taken from `ability.duration`, not from the embedded default. Without this
    // the shape assertions above would prove only that a field holds a value.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gideon = scenario
        .add_planeswalker_from_oracle(P0, "Gideon Jura", "Gideon", 6, GIDEON)
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    runner
        .activate(gideon, 0)
        .target_player(P1)
        .decline_optional()
        .resolve();

    // POSITIVE REACH GUARD: the requirement is installed and it reaches P1's
    // creature — without this the duration assertion would pass on a no-op.
    let installed: Vec<&TransientContinuousEffect> = runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|e| {
            e.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustAttackDefender { .. }
                    }
                )
            })
        })
        .collect();
    assert_eq!(
        installed.len(),
        1,
        "the +2 must install exactly one attack requirement: {:?}",
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .collect::<Vec<_>>()
    );
    // THE `[COVER]` ASSERTION: the installed window came from `ability.duration`
    // (the printed next-turn window, with `PlayerScope::Target` already lowered to
    // the concrete targeted player), NOT from the embedded `UntilEndOfTurn`
    // default. This holds identically at BASE_SHA — that is the point.
    assert_eq!(
        installed[0].duration,
        Duration::UntilEndOfNextTurnOf {
            player: PlayerScope::SpecificPlayer { id: P1 }
        },
        "force_attack::resolve applies `ability.duration.unwrap_or_else(|| duration)`, \
         so the printed next-turn window must win over the embedded default"
    );

    // POSITIVE REACH GUARD, part 2: the requirement really BINDS during the
    // targeted opponent's next turn (CR 508.1c) — so "installed" is not
    // "installed against nobody". It is correctly INERT on the activation turn,
    // which is why the check is made after the boundary.
    assert!(
        !engine::game::combat::creature_must_attack(runner.state(), victim),
        "the requirement must NOT bind on the activation turn — it is printed \
         `During target opponent's NEXT turn`"
    );
    cross_turn_boundary(&mut runner);
    assert_eq!(
        runner.state().active_player,
        P1,
        "the boundary crossed into the TARGETED opponent's turn"
    );
    assert!(
        engine::game::combat::creature_must_attack(runner.state(), victim),
        "the attack requirement must bind P1's creature during P1's next turn"
    );
}
