//! Regression for GitHub issue #8721 — a resolving instant/sorcery that prints
//! "Exile <this card>" must exile itself, not go to the graveyard.
//!
//! Three rules meet here, and the assertions below name the one that actually
//! does the moving.
//!
//! CR 608.2c: the controller follows the spell's instructions in the order
//! written — "Exile ~." is such an instruction, and IT is what puts the card in
//! exile. CR 608.2n: "As the final part of an instant or sorcery spell's
//! resolution, the spell is put into its owner's graveyard" — the default this
//! instruction pre-empts, not the authority for exiling. CR 608.2m: a spell that
//! "leaves the stack once it starts to resolve … will continue to resolve fully"
//! — what lets the rest of the resolution happen after the self-move.
//! `stack::spell_still_on_stack` is where the engine reconciles them: it gates
//! the CR 608.2n default on Stack residency, so a self-exile that RUNS is
//! honoured.
//!
//! The defect is upstream of that gate. When a free-cast head carries the
//! parser's graveyard-destination rider ("If that spell would be put into your
//! graveyard, exile it instead" — a `ChangeZone { destination: Exile, target:
//! ParentTarget }` sub-ability), `effects::mod` consumes that rider as
//! permission metadata and returns, discarding EVERY further link in the chain.
//! The trailing self-exile is such a link, so it never runs.
//!
//! Scope, measured over the full-corpus parse dump rather than inferred from
//! wording: exactly one instant or sorcery hangs its self-exile behind a rider on
//! an `Effect::CastFromZone` head — Sins of the Past. The self-exiling instants
//! and sorceries this branch does NOT see route through the generic chain drain,
//! which the control below exercises; the Invoke Calamity test alongside shows a
//! third route again (`Effect::FreeCastFromZones`). (Deliberately not phrased as "N cards print a self-exile":
//! that count moves with the wording predicate chosen, and nothing here rests
//! on it.)
//!
//! The control below is the same trailing self-exile under a head that carries
//! no rider (Treasured Find, issue #323). It must stay green: it is what proves
//! the failure is the rider tail-drop and not the self-exile machinery.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Treasured Find. A trailing self-exile with no rider on the head.
const TREASURED_FIND_ORACLE: &str =
    "Return target card from your graveyard to your hand. Exile Treasured Find.";

/// Sins of the Past. `CastFromZone` head + graveyard-redirect rider + self-exile.
const SINS_OF_THE_PAST_ORACLE: &str =
    "Until end of turn, you may cast target instant or sorcery card from your graveyard \
without paying its mana cost. If that spell would be put into your graveyard, exile it \
instead. Exile Sins of the Past.";

/// Invoke Calamity. `FreeCastFromZones` head + the same rider + self-exile.
const INVOKE_CALAMITY_ORACLE: &str =
    "You may cast up to two instant and/or sorcery spells with total mana value 6 or less \
from your graveyard and/or hand without paying their mana costs. If those spells would be \
put into your graveyard, exile them instead. Exile Invoke Calamity.";

/// CR 608.2c + issue #323: the control. A trailing self-exile under a head with
/// no rider moves the resolved sorcery to exile, so the CR 608.2n default never
/// applies.
#[test]
fn a_self_exile_after_a_riderless_head_exiles_the_resolved_spell() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Treasured Find", false, TREASURED_FIND_ORACLE)
        .id();
    let fodder = scenario
        .add_spell_to_graveyard(P0, "Graveyard Fodder", true)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[fodder]).resolve();

    assert_eq!(
        outcome.state().objects[&fodder].zone,
        Zone::Hand,
        "control: the targeted card must reach hand, proving the chain ran at all"
    );
    assert_eq!(
        outcome.state().objects[&spell].zone,
        Zone::Exile,
        "\"Exile Treasured Find.\" must move the resolved sorcery to exile (CR 608.2c)"
    );
}

/// CR 608.2c + issue #8721: the `CastFromZone` member. The graveyard-redirect
/// rider is consumed as permission metadata, and the self-exile link AFTER it
/// must still run.
#[test]
fn a_self_exile_after_a_cast_from_zone_rider_exiles_the_resolved_spell() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sins of the Past", false, SINS_OF_THE_PAST_ORACLE)
        .id();
    let fodder = scenario
        .add_spell_to_graveyard(P0, "Graveyard Fodder", true)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[fodder]).resolve();

    assert_eq!(
        outcome.state().objects[&fodder].zone,
        Zone::Graveyard,
        "the rider must stay permission metadata: the granted card stays in the graveyard \
         until the player casts it"
    );
    assert_eq!(
        outcome.state().objects[&spell].zone,
        Zone::Exile,
        "\"Exile Sins of the Past.\" must move the resolved sorcery to exile (CR 608.2c), \
         not leave it in the graveyard"
    );
}

/// CR 608.2c + issue #8721: the `FreeCastFromZones` neighbour. Same rider and
/// same trailing self-exile in the AST, but a different head effect — and it was
/// ALREADY green before this fix, because the rider branch this fix changes is
/// gated on `Effect::CastFromZone` and never sees it. So this test is not a
/// proof of the fix; it is the boundary marker that pins the neighbouring head
/// against a future widening of that gate. Counter-probe: neutralizing the fix
/// leaves this test green.
#[test]
fn a_self_exile_after_a_free_cast_from_zones_rider_exiles_the_resolved_spell() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Invoke Calamity", false, INVOKE_CALAMITY_ORACLE)
        .id();
    scenario.add_spell_to_graveyard(P0, "Graveyard Fodder", true);

    let mut runner = scenario.build();
    runner.cast(spell).resolve();

    // The head opens its free-cast window first (CR 608.2g). Decline it, so the
    // only thing left to run is the trailing "Exile Invoke Calamity."
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { .. },
                ..
            }
        ),
        "reach guard: the head must have opened its free-cast window, otherwise this test \
         never reaches the trailing self-exile"
    );
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the window must finish it");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Exile,
        "\"Exile Invoke Calamity.\" must move the resolved sorcery to exile (CR 608.2c)"
    );
}

/// CR 608.2c + issue #8721: the guard for a repair that was MEASURED AND
/// REJECTED.
///
/// Invasion of Alara prints "Put one of them into your hand." — a real
/// instruction, not a graveyard replacement — and `graveyard_destination_rider`'s
/// HAND arm swallows it. It is the one member of that arm without the "if you
/// don't cast it" gate the other four carry (Rashmi, Eternities Crafter;
/// Discover the Impossible; Solstice Revelations; Ziatora's Envoy), so gating
/// the arm on that condition looks like the repair. It is not: with no chosen
/// target on the head, the instruction's `ParentTarget` binds to the SOURCE, and
/// the permanent returns ITSELF to its owner's hand. Measured, then reverted.
///
/// What this test therefore asserts is only that: the source stays put. It is a
/// discriminating guard for the rejected repair — with that repair in place the
/// zone assertion goes red (the reach guard still passes) — and for NOTHING
/// else.
///
/// Deliberately understated, because two limits are real. The card is a Battle
/// (Siege) and is built here as an artifact stand-in, so only the chain is under
/// test, not the type. And the OBSERVED fact — stated without a cause, because
/// the cause was not measured — is that this scenario's outcome is identical
/// with the tail handling on and off. So this test says nothing about the tail,
/// in either direction.
#[test]
fn invasion_of_alara_does_not_return_itself_to_hand() {
    use engine::types::actions::GameAction;

    const ALARA: &str = "When this Siege enters, exile cards from the top of your library until \
you exile two nonland cards with mana value 4 or less. You may cast one of those two cards \
without paying its mana cost. Put one of them into your hand. Then put the other cards exiled \
this way on the bottom of your library in a random order.";

    for accept in [false, true] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        for i in 0..6 {
            scenario.add_spell_to_library_top(P0, &format!("Cheap Spell {i}"), true);
        }
        for _ in 0..6 {
            scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
        }
        // Built as an artifact so the enters-the-battlefield trigger actually
        // fires; see the limits named in this test's doc comment.
        let siege = scenario
            .add_artifact_to_hand_from_oracle(P0, "Invasion of Alara", ALARA)
            .id();
        let mut runner = scenario.build();
        let _ = runner.cast(siege).try_resolve();
        for _ in 0..40 {
            match runner.state().waiting_for.clone() {
                WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                    if runner.choose_first_legal_target().is_err() {
                        break;
                    }
                }
                WaitingFor::OptionalEffectChoice { .. } => {
                    if runner
                        .act(GameAction::DecideOptionalEffect { accept })
                        .is_err()
                    {
                        break;
                    }
                }
                WaitingFor::OrderTriggers { triggers, .. } => {
                    let order = (0..triggers.len()).collect();
                    if runner.act(GameAction::OrderTriggers { order }).is_err() {
                        break;
                    }
                }
                WaitingFor::Priority { .. } => {
                    if runner.state().stack.is_empty()
                        || runner.act(GameAction::PassPriority).is_err()
                    {
                        break;
                    }
                }
                WaitingFor::EffectZoneChoice { cards, count, .. } => {
                    let pick: Vec<_> = cards.into_iter().take(count.max(1)).collect();
                    if runner.act(GameAction::SelectCards { cards: pick }).is_err() {
                        break;
                    }
                }
                _ => {
                    break;
                }
            }
        }
        runner.advance_until_stack_empty();
        let mut zones: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        let mut with_perm: Vec<(String, String)> = Vec::new();
        for (id, o) in runner.state().objects.iter() {
            if *id == siege {
                continue;
            }
            zones
                .entry(format!("{:?}", o.zone))
                .or_default()
                .push(o.name.clone());
            if !o.casting_permissions.is_empty() {
                with_perm.push((o.name.clone(), format!("{:?}", o.zone)));
            }
        }
        // Reach guard: without it this test also passes when the ETB trigger
        // never resolved — which is how an earlier draft of it measured nothing.
        assert!(
            zones.contains_key("Exile"),
            "reach guard (accept={accept}): the ETB trigger must have exiled at least one \
             card, got {zones:?} / {with_perm:?}"
        );
        assert_eq!(
            runner.state().objects[&siege].zone,
            Zone::Battlefield,
            "accept={accept}: the source must not move itself — \"Put one of them into your \
             hand.\" names an exiled card, never the permanent"
        );
    }
}

/// CR 400.7 + issue #8721: the tail handling stops at a tail that chains
/// further instructions, and this pins that boundary.
///
/// The Great Work's chapter III ends "Exile this Saga, then return it to the
/// battlefield" — two `SelfRef` moves in sequence. Running only the first leaves
/// the source in exile, where dropping the whole tail left it in the graveyard;
/// the second move fails because CR 400.7 makes the exiled card a new object.
/// That is worse than the defect this PR repairs, so multi-link tails are out of
/// scope and the second move is left to its own unit of work.
///
/// Measured on this stand-in rather than on the real card: The Great Work is a
/// Saga whose chapter III could not be driven to fire in a scenario, so the
/// printed chapter text is carried by a sorcery here. Only the zone outcome is
/// under test, not the Saga machinery.
///
/// Counter-probe: dropping the `tail.sub_ability.is_none()` filter turns this
/// red — the source lands in `Exile` and stays there.
#[test]
fn a_tail_that_chains_further_instructions_is_out_of_scope() {
    use engine::types::actions::GameAction;

    // Verbatim chapter III of The Great Work (`client/public/card-data.json`).
    const GREAT_WORK_CHAPTER_THREE: &str =
        "Until end of turn, you may cast instant and sorcery spells from any graveyard. If a \
spell cast this way would be put into a graveyard, exile it instead. Exile this Saga, then \
return it to the battlefield (front face up).";

    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_spell_to_hand_from_oracle(P0, "The Great Work", false, GREAT_WORK_CHAPTER_THREE)
        .id();
    let fodder = scenario
        .add_spell_to_graveyard(P0, "Graveyard Fodder", true)
        .id();

    let mut runner = scenario.build();
    let _ = runner.cast(source).try_resolve();
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                if runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();

    // Reach guard: the head must have resolved at all.
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "reach guard: the granted card stays in the graveyard, so the head ran and its rider \
         was consumed as metadata"
    );
    assert_eq!(
        runner.state().objects[&source].zone,
        Zone::Graveyard,
        "a multi-link tail is not run at all: half of \"Exile this Saga, then return it\" \
         would strand the source in exile (CR 400.7 — the moved card is a new object)"
    );
}
