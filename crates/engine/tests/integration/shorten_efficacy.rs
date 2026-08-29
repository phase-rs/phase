// engine-citation-gate: symbol anchors only
//! CR 732.2b/c stage 2 — EFFICACY of a polled seat's loop-shortcut response.
//!
//! CITATION FORM: rule NUMBER only. The number is itself the greppable heading —
//! `grep '^732.2c' docs/MagicCompRules.txt` resolves any citation below. Line
//! anchors are forbidden here (this file is enrolled in
//! `subsystem_citations_are_symbol_anchored`) because `docs/MagicCompRules.txt`
//! is gitignored and re-fetched per checkout, so a line anchor is pinned to
//! whichever rules revision the author happened to hold — the anchors this file
//! originally shipped already resolved to the wrong lines against the revision
//! fetched into the neighbouring checkout.
//!
//! `ai_support::smart_shortcut_response` shipped with a POSSIBILITY predicate
//! only: any meaningful priority action bought a `Shorten`, i.e. a real priority
//! window. That is right for a seat holding a Bolt and wrong for a seat holding
//! a fetchland — activating Terramorphic Expanse satisfies CR 732.2c's "must
//! make a different game choice" while changing nothing about the loop, so the
//! window is spent achieving nothing.
//!
//! Stage 2 is AI POLICY, not a rule: CR 732.2b grants an
//! unconditioned accept-or-shorten option and states no efficacy criterion. The
//! rows below pin the policy's two arms and, more importantly, pin the ONE
//! thing an over-broad version would destroy — that a seat holding real
//! interaction still gets its window.
//!
//! # Mutant discipline
//!
//! Two mutants are named per row, and every row states which one flips it:
//! * **DROP** — delete stage 2 from `smart_shortcut_response` (both arms), i.e.
//!   restore the shipped one-stage predicate.
//! * **TRIVIALIZE** — make the stage-2 predicate constant. For arm (B) that is
//!   `shortcut_efficacy::filter_is_actor_owned ≡ true` (everything looks
//!   confined); for arm (A) it is deleting the `crowned_winner` guard.
//!
//! A row whose expected value equals the SHIPPED value cannot be flipped by
//! DROP — its discriminating power is entirely in TRIVIALIZE, and that is
//! stated on the row rather than papered over.

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::{GameAction, PrecastCopyShortcutResponse};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::game_state::{GameState, LayersDirty, LoopDetectionMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

// --- Oracle-text constants, and what each one's provenance ACTUALLY is ---
//
// Two provenances ship here and they are deliberately not conflated, because
// "verbatim Oracle text" is a claim about a printing and only some of these
// make it.
//
// (1) SIBLING-FIXTURE PROVENANCE — the four loop-shape constants below are
//     byte-identical copies of the shipped constants in
//     `tests/integration/loop_shortcut.rs`, which does not present them as any
//     card's Oracle text either. They exist to reproduce that file's mutual-drain
//     loop, and copying them verbatim is what keeps the two files' loop shape
//     identical. MEASURED against MTGJSON `AtomicCards.json` (`.data[*][0].text`):
//       * `DRAIN_CLERIC` IS one printing's complete Oracle text (Epicure of Blood,
//         Marauding Blight-Priest — 2 exact matches);
//       * `BLOOD_SIPPER` matches NO card, not even as a substring;
//       * `KICKOFF` / `TARGETED_KICKOFF` match no card's complete text; they are
//         single-clause fragments (substrings of 53 and 3 cards respectively).
//     So do NOT cite this block as card-derived: only `DRAIN_CLERIC` would
//     survive that claim, and it is not why any of the four is here.
//
// (2) CARD PROVENANCE — `TERRAMORPHIC` and `DEATHRITE_SHAMAN` (below) ARE their
//     named printing's complete Oracle text, verified byte-for-byte against
//     MTGJSON. That matters for those two specifically: they are the rows'
//     subject matter, and a paraphrase can take a different parser branch and go
//     green while the real card stays broken.

const DRAIN_CLERIC: &str = "Whenever you gain life, each opponent loses 1 life.";
const BLOOD_SIPPER: &str = "Whenever an opponent loses life, you gain 1 life.";
const KICKOFF: &str = "You gain 1 life.";
const TARGETED_KICKOFF: &str = "Target player gains 1 life.";

/// Terramorphic Expanse, verbatim. Acceptance (a): a fetchland is the canonical
/// action that is legal, meaningful to stage 1, and totally confined.
const TERRAMORPHIC: &str = "{T}, Sacrifice this land: Search your library for a basic land card, \
                            put it onto the battlefield tapped, then shuffle.";

/// Deathrite Shaman, verbatim, all three abilities. Ability `[0]`'s cost is
/// `{T}` ALONE — no mana component — which is what lets the V1c fixture deny
/// `{B}`/`{G}` and still leave `[0]` legal. Its target is a land card in *a*
/// graveyard: the AST names no player, so ownership is UNPROVEN (CR 400.1 —
/// "Each player has their own library, hand, and graveyard"), which is exactly
/// why an `origin`-keyed confinement rule would wrongly call it self-contained.
const DEATHRITE_SHAMAN: &str = "{T}: Exile target land card from a graveyard. Add one mana of any \
                                color.\n{B}, {T}: Exile target instant or sorcery card from a \
                                graveyard. Each opponent loses 2 life.\n{G}, {T}: Exile target \
                                creature card from a graveyard. You gain 2 life.";

// ---------------------------------------------------------------------------
// Shared drive helpers. Deliberately local: `loop_shortcut.rs`'s equivalents
// are private to that module and it is not in this change's scope.
// ---------------------------------------------------------------------------

/// Pass/answer beats until the state leaves `Priority`/`OrderTriggers`.
fn drive_collect(runner: &mut GameRunner, cap: usize) -> WaitingFor {
    for _ in 0..cap {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                if runner
                    .act(GameAction::OrderTriggers { order })
                    .or_else(|_| runner.act(GameAction::OrderTriggers { order: vec![] }))
                    .is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
    runner.state().waiting_for.clone()
}

/// The exact action list `smart_shortcut_response` folds over — obtained by
/// CALLING production's recipe (`ai_support::shortcut_probe`), not by copying it.
/// A local copy would drift the moment production's recipe changed, and every
/// reach-guard in this file reads this list, so the guards would then be
/// measuring a different action set than the code under test.
fn probe_actions(state: &GameState, player: PlayerId) -> Vec<GameAction> {
    engine::ai_support::shortcut_probe(state, player).1
}

/// Stage 1's verdict, evaluated on the PROBE state — which is the state
/// production evaluates it on. Evaluating it on the caller's
/// `RespondToShortcut` state instead silently drops
/// `has_meaningful_priority_action`'s sacrifice-for-mana rung, which is gated on
/// `waiting_for` being `Priority`.
fn stage_one_meaningful(state: &GameState, player: PlayerId) -> bool {
    let (probe, actions) = engine::ai_support::shortcut_probe(state, player);
    engine::ai_support::has_meaningful_priority_action(probe.state(), &actions)
}

/// Which ability indices of `source` are actually enumerated at this window.
/// V1c's two reach-guards read this.
fn legal_ability_indices(state: &GameState, player: PlayerId, source: ObjectId) -> Vec<usize> {
    let mut indices: Vec<usize> = probe_actions(state, player)
        .iter()
        .filter_map(|a| match a {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if *source_id == source => Some(*ability_index),
            _ => None,
        })
        .collect();
    indices.sort_unstable();
    indices
}

/// The shipped `setup_3p_optional_cascade` shape (`loop_shortcut.rs`): P0 runs
/// a self-refilling mutual drain, P1's Mountain + Bolt make the loop OPTIONAL
/// so an offer is raised at all. `decorate` stages the seat under test.
fn optional_cascade(decorate: impl FnOnce(&mut GameScenario)) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    decorate(&mut scenario);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, kickoff)
}

/// Cast the kick-off, drive to the offer, have P0 declare, then walk the APNAP
/// queue to `seat` by submitting manual Accepts for everyone ahead of it (never
/// the AI's answer — that would stop the queue).
fn respond_window_at(runner: &mut GameRunner, kickoff: ObjectId, seat: PlayerId) {
    let _ = runner.cast(kickoff).resolve();
    let wf = drive_collect(runner, 500);
    assert!(
        matches!(wf, WaitingFor::LoopShortcut { .. }),
        "reach-guard: the optional cascade must OFFER a shortcut, got {wf:?}"
    );
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("the proposer declares");
    for _ in 0..8 {
        match runner.state().waiting_for {
            WaitingFor::RespondToShortcut { player, .. } if player == seat => return,
            WaitingFor::RespondToShortcut { .. } => {
                runner
                    .act(GameAction::RespondToShortcut {
                        response: ShortcutResponse::Accept,
                    })
                    .expect("manual Accept advances the APNAP queue");
            }
            _ => break,
        }
    }
    panic!(
        "reach-guard: {seat:?} must be polled; stopped at {:?}",
        runner.state().waiting_for
    );
}

// ===========================================================================
// V1b — ACCEPTANCE (a), Path A class: a fetchland no longer buys a window.
// ===========================================================================

/// The shipped optional-cascade fixture plus exactly ONE card: a Terramorphic
/// Expanse on the polled seat's battlefield. Stage 1 still says "you have a
/// meaningful action" — a non-mana activated ability always does — and stage 2
/// answers the question stage 1 cannot: the action reaches nothing but its own
/// controller's library, so the window would change nothing.
///
/// MUTANTS — both flip the `Accept` assertion:
/// * DROP ⇒ `Shorten { at_iteration: 0 }` (this IS the shipped behaviour, which
///   is the defect).
/// * TRIVIALIZE arm (B) (`any_action_may_interfere ≡ true`) ⇒ `Shorten`.
///
/// REACH-GUARDS (both are assertions): the fetchland really is enumerated, and
/// stage 1 really does return `true`. Without them an `Accept` here would be
/// indistinguishable from the empty-board stage-1 path — the vacuity that a
/// naive version of this row would ship.
#[test]
fn v1b_a_confined_fetchland_accepts_instead_of_buying_a_vacuous_window() {
    let mut terramorphic = ObjectId(0);
    let (mut runner, kickoff) = optional_cascade(|s| {
        terramorphic = s
            .add_land_from_oracle(P2, "Terramorphic Expanse", TERRAMORPHIC)
            .id();
    });
    respond_window_at(&mut runner, kickoff, P2);

    let actions = probe_actions(runner.state(), P2);
    assert!(
        actions.contains(&GameAction::ActivateAbility {
            source_id: terramorphic,
            ability_index: 0,
        }),
        "REACH-GUARD 1: the fetchland's ability must be enumerated, otherwise this row \
         degenerates to the empty-board stage-1 path; got {actions:?}"
    );
    assert!(
        stage_one_meaningful(runner.state(), P2),
        "REACH-GUARD 2: stage 1 (POSSIBILITY, untouched by this change) must still return \
         true — an Accept produced by stage 1 would prove nothing about stage 2"
    );

    assert_eq!(
        engine::ai_support::smart_shortcut_response(runner.state(), P2),
        ShortcutResponse::Accept,
        "a seat whose ONLY action is a self-contained fetch has no efficacious response; \
         spending a real priority window on it changes nothing (CR 732.2c is satisfied by \
         any different choice, which is precisely why it grants no efficacy)"
    );
}

// ===========================================================================
// V1c — B1 REGRESSION LOCK. The graveyard-hate seat still Shortens.
// ===========================================================================

/// The one row whose sole job is pinning the owner axis. A graveyard is a
/// PER-PLAYER zone (CR 400.1) and `Zone` carries no
/// player field, so a rule keyed on `ChangeZone.origin` cannot tell "exile a
/// land card from MY graveyard" from "…from YOURS". Deathrite Shaman `[0]`
/// exiles a land card from P0's graveyard — a real interaction with another
/// player's resources — and must keep its window.
///
/// This row's expected value (`Shorten`) IS the shipped value, so **DROP cannot
/// flip it**. Its whole discriminating power is the TRIVIALIZE arm:
/// `filter_is_actor_owned ≡ true` makes DRS `[0]` fold to `OwnResourcesOnly`
/// and the response becomes `Accept` — the row FLIPS.
///
/// That flip only exists if the fixture leaves ability `[0]` and ONLY `[0]`
/// legal, so both constraints ship as assertions:
/// * REACH-GUARD 1 — without a land card in a graveyard, `[0]` is not
///   enumerated at all and the action list collapses to `["PassPriority"]`;
///   stage 1 returns false and the row would pass through the wrong path.
/// * REACH-GUARD 2 — with `{B}`/`{G}` available and a matching graveyard card,
///   `[1]`/`[2]` become legal. Their `LoseLife`/`GainLife` sub-effects classify
///   `MayInterfere` even under the mutant, so `any_action_may_interfere`'s
///   `.any()` absorbs the mutation and the row passes VACUOUSLY.
///
/// The fixture denies `{B}`/`{G}` by construction (P2 controls no lands) and
/// stages no instant/sorcery/creature card in any graveyard, so `[1]` and `[2]`
/// are each blocked on two independent axes.
#[test]
fn v1c_graveyard_hate_across_a_per_player_zone_keeps_its_window() {
    let mut shaman = ObjectId(0);
    let (mut runner, kickoff) = optional_cascade(|s| {
        // CR 302.6 (the summoning-sickness rule): `add_creature_from_oracle`
        // stages a pre-existing battlefield creature, so the `{T}` cost is
        // payable.
        shaman = s
            .add_creature_from_oracle(P2, "Deathrite Shaman", 1, 2, DEATHRITE_SHAMAN)
            .id();
        // The land card sits in P0's graveyard — the ability reaches ACROSS a
        // per-player zone that the AST does not player-qualify. That crossing
        // is the whole of the defect this row locks.
        s.add_land_to_graveyard(P0, "Test Graveyard Land");
    });
    respond_window_at(&mut runner, kickoff, P2);

    let indices = legal_ability_indices(runner.state(), P2, shaman);
    assert!(
        indices.contains(&0),
        "REACH-GUARD 1: without a land card in a graveyard the Shaman's [0] is not enumerated \
         and this row degenerates to the stage-1 empty-action path; got {indices:?}"
    );
    assert_eq!(
        indices,
        vec![0],
        "REACH-GUARD 2: [1]/[2] must stay illegal. They classify MayInterfere even under the \
         TRIVIALIZE mutant, so leaving one legal lets .any() absorb the mutation and this row \
         passes vacuously; got {indices:?}"
    );

    assert_eq!(
        engine::ai_support::smart_shortcut_response(runner.state(), P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "exiling a land card out of ANOTHER player's graveyard is real interaction — the \
         confinement predicate must require PROVEN actor ownership, not merely a zone name"
    );
}

// ===========================================================================
// V2 / V3 — ACCEPTANCE (b) and the matched pair.
// ===========================================================================

/// V3, both arms in one row, because neither arm alone is the discriminator.
/// The two boards are identical but for P2's holdings:
///   * `{}` ⇒ Accept, reached through stage 1 (nothing to do);
///   * `{Mountain, Lightning Bolt}` ⇒ Shorten, reached through stage 2.
///
/// The pass ⇒ grant / respond ⇒ no-grant pair is what proves stage 2 did not
/// over-generalize into "always Accept". Sibling coverage for Wrath of God,
/// Naturalize, Divination and Path to Exile is at classifier granularity in
/// `ai_support::shortcut_efficacy`'s unit table (they are sorceries/instants
/// with no legal target on this board, so a runtime row would assert on
/// castability rather than on efficacy).
///
/// MUTANTS: TRIVIALIZE arm (B) (`any_action_may_interfere ≡ false`, or
/// `filter_is_actor_owned ≡ true` — Bolt's `DealDamage` reaches neither, so it
/// is the whole-predicate constant that bites) flips the Bolt arm to `Accept`.
/// DROP leaves both arms at their shipped values and flips neither; that is
/// stated rather than claimed otherwise.
#[test]
fn v3_matched_pair_empty_seat_accepts_and_bolt_seat_still_shortens() {
    // Arm 1 — nothing at all.
    let (mut bare, bare_kickoff) = optional_cascade(|_| {});
    respond_window_at(&mut bare, bare_kickoff, P2);
    let bare_actions = probe_actions(bare.state(), P2);
    assert!(
        !stage_one_meaningful(bare.state(), P2),
        "reach-guard: this arm must resolve at STAGE 1, so it stays a control for the stage-2 \
         arm below; got {bare_actions:?}"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(bare.state(), P2),
        ShortcutResponse::Accept,
        "no meaningful action ⇒ Accept, unchanged from the shipped predicate"
    );

    // Arm 2 — the SAME board plus a Mountain and a Bolt.
    let (mut armed, armed_kickoff) = optional_cascade(|s| {
        s.add_basic_land(P2, ManaColor::Red);
        s.add_bolt_to_hand(P2);
    });
    respond_window_at(&mut armed, armed_kickoff, P2);
    let armed_actions = probe_actions(armed.state(), P2);
    assert!(
        armed_actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { .. })),
        "reach-guard: the Bolt must actually be castable, otherwise this arm tests the empty \
         board twice; got {armed_actions:?}"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(armed.state(), P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "ACCEPTANCE (b): a seat holding real interaction must still get its priority window — \
         this is the assertion any over-broad confinement rule destroys first"
    );
}

// ===========================================================================
// V4 / V6 / V7 — arm (A): the crowned seat, keyed on `predicted_winner`.
// ===========================================================================

/// CR 732.2a lets the player with priority propose a
/// shortcut whose predictable result crowns SOMEONE ELSE. This fixture is the
/// shipped `interactive_offer_separates_priority_proposer_from_predicted_winner`
/// shape: P1 proposes, P0 is the measured winner, and P1 (the proposer) is
/// excluded from the response queue, so P0 is polled.
///
/// P0 also holds a Mountain and a Bolt — the reach-guard the measurement proved
/// load-bearing. WITHOUT them P0 has no meaningful action and Accepts via
/// stage 1, making the row vacuous; WITH them the shipped predicate returns
/// `Shorten`, i.e. the crowned player shortens its own guaranteed win.
///
/// Three claims ride this one board:
/// * **V4** — arm (A) fires: the crowned seat Accepts.
/// * **V6** — it is keyed on `predicted_winner`, never `proposer`. The row
///   asserts `proposer != predicted_winner` and `polled == predicted_winner`,
///   so a `proposer`-keyed implementation (which passes every other row) fails
///   exactly here.
/// * **V7** — read order. Arm (A) reads the proposal off the ORIGINAL state;
///   `smart_shortcut_response` overwrites its probe clone's `waiting_for` with
///   `Priority` before enumerating. Moving that read after the clone makes
///   `crowned_winner` unconditionally `None` and this row FAILS — it is the
///   only row that can detect the mis-ordering.
///
/// MUTANTS — both flip the `Accept` assertion: DROP ⇒ `Shorten`; TRIVIALIZE
/// arm (A) (delete the `crowned_winner` guard) ⇒ `Shorten` via arm (B), because
/// the Bolt is genuine interference.
#[test]
fn v4_the_crowned_seat_accepts_its_own_predicted_win() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    // The reach-guard: P0 must hold a meaningful action or stage 1 answers first.
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_bolt_to_hand(P0);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P1, "P0 Lifegain Kickoff", false, TARGETED_KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let _ = runner.cast(kickoff).target_player(P0).resolve();
    let wf = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        ..
    } = wf
    else {
        panic!("reach-guard: P1's priority window must receive an offer, got {wf:?}");
    };
    assert_eq!(
        proposer, P1,
        "CR 732.2a routes the offer to the priority holder"
    );
    assert_eq!(
        predicted_winner,
        Some(P0),
        "reach-guard: the two authorities must actually DIFFER, or the winner-keyed and \
         proposer-keyed implementations are indistinguishable here"
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::UntilLethal,
            template: None,
        })
        .expect("P1 declares");
    let WaitingFor::RespondToShortcut {
        player,
        ref proposal,
        ..
    } = runner.state().waiting_for
    else {
        panic!(
            "reach-guard: a response window must open, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P0,
        "the proposer is excluded from its own response queue, so the crowned seat is polled"
    );
    assert_ne!(
        proposal.proposer,
        proposal
            .predicted_winner
            .expect("this offer names a winner"),
        "V6: the multi-authority premise — a proposer-keyed rule would read P1 here"
    );

    let actions = probe_actions(runner.state(), P0);
    assert!(
        stage_one_meaningful(runner.state(), P0),
        "REACH-GUARD: without a meaningful action P0 would Accept via stage 1 and this row \
         would be vacuous; got {actions:?}"
    );

    assert_eq!(
        engine::ai_support::smart_shortcut_response(runner.state(), P0),
        ShortcutResponse::Accept,
        "arm (A): the offer's predicted result already crowns this seat. CR 732.2c grants a \
         shortening player nothing but the obligation to choose differently, so shortening \
         here moves the game away from a win it already holds"
    );
}

// ===========================================================================
// V1 / V5 — REAL 4-player board, loaded through the production restore
// chokepoint and driven through the public `apply()` boundary.
// ===========================================================================

/// Inflate a committed dump fixture.
fn gunzip_dump(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// Decode AS `PersistedGameState` — the production chokepoint the server's
/// `from_persisted` and WASM's `decode_restored_game_state` both funnel
/// through — rather than decoding a bare `GameState`.
fn restore_dump(json: &str) -> GameState {
    let envelope: serde_json::Value =
        serde_json::from_str(json).expect("dump envelope parses as JSON");
    serde_json::from_value::<engine::types::game_state::PersistedGameState>(
        envelope["gameState"].clone(),
    )
    .expect("gameState deserializes through the production decoder")
    .into_game_state()
    .expect("persisted test snapshot satisfies the checked restore contract")
}

/// The LIVE-PATH board: the real 4-player Dina / Bloodthirsty Conqueror drain
/// on which the defect actually occurs, because seat P2 controls a Terramorphic
/// Expanse.
///
/// Derived from the read-only pristine archive, and the derivation is the
/// artifact's provenance rather than a claim about it:
/// `unzip -p combofb-dumps-pristine/dina-conqueror-offers-no-ff.zip |
///  jq -c '{gameState}' | gzip -9 -n`
/// → 841475 bytes, sha256
/// `12f91f38616bd90386fdb7a137371a9f9ec5c6adcb12dd1bbad04bde167ea2b2`.
///
/// That pipeline ALONE is no longer sufficient: this capture predates U5, so
/// its bare `"deck_size": 100` must first become the adjacently-tagged
/// `DeckSizeRule` form `{"type":"Exactly","data":100}` — the variant taken from
/// the sibling `format` field, `Commander` here. Piping the raw member straight
/// through does not merely miss the digest; it yields a fixture
/// `PersistedGameState` cannot deserialize, so `restore_dump`'s
/// `.expect("gameState deserializes through the production decoder")` aborts —
/// a red test on a green engine. `dina_noff_turn5_loader.rs` carries the full
/// provenance table for this same artifact.
///
/// `gzip -n` is a no-op from a pipe but load-bearing from a file (it strips the
/// stored name and mtime), so KEEP it: a re-derivation that stages the 21 MB
/// dump through an intermediate file — the natural thing to do at that size —
/// otherwise misses the digest and presents as a corrupt artifact rather than
/// as convention drift.
fn live_path_board() -> GameState {
    restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_noff_turn5_4p.json.gz"
    )))
}

/// The QUIET board: the same matchup captured at a beat where NO seat holds any
/// meaningful priority action. Retained only as a negative control — see
/// `v1_control_*` for why it has no discriminating power for acceptance (a).
fn quiet_board() -> GameState {
    restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )))
}

fn dump_driver_forbids(a: &GameAction) -> bool {
    matches!(a, GameAction::Concede { .. } | GameAction::Debug(_))
}

fn dump_beat_actor(state: &GameState) -> Option<(PlayerId, Vec<GameAction>)> {
    if let Some(p) = state.waiting_for.acting_player() {
        let (actions, _costs, _grouped) = engine::ai_support::legal_actions_for_viewer(state, p);
        if !actions.is_empty() {
            return Some((p, actions));
        }
    }
    for p in state.players.iter().map(|p| p.id) {
        let (actions, _costs, _grouped) = engine::ai_support::legal_actions_for_viewer(state, p);
        if !actions.is_empty() {
            return Some((p, actions));
        }
    }
    None
}

/// One beat of the drain-drive policy: at `Priority` ALWAYS pass (the mandatory
/// triggers re-trigger — that IS the loop), answer every other prompt.
fn dump_drive_one_beat(state: &mut GameState) -> Result<(), String> {
    let Some((who, actions)) = dump_beat_actor(state) else {
        return Err(format!("no legal actor at {:?}", state.waiting_for));
    };
    let chosen = if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        actions
            .iter()
            .find(|a| matches!(a, GameAction::PassPriority))
            .cloned()
    } else {
        actions
            .iter()
            .find(|a| !matches!(a, GameAction::PassPriority) && !dump_driver_forbids(a))
            .or_else(|| actions.iter().find(|a| !dump_driver_forbids(a)))
            .cloned()
    };
    let Some(action) = chosen else {
        return Err(format!("empty action list at {:?}", state.waiting_for));
    };
    apply(state, who, action.clone())
        .map(|_| ())
        .map_err(|e| format!("apply err ({action:?}): {e:?}"))
}

/// Drive real beats until the board mints a bounded offer.
fn drive_to_offer(state: &mut GameState, cap: usize) -> Option<usize> {
    for beat in 0..cap {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            return Some(beat);
        }
        if dump_drive_one_beat(state).is_err() {
            return None;
        }
    }
    None
}

// NOTE: no `give_fetchland` staging helper. The live-path test drives the seat's
// OWN Terramorphic Expanse out of the restored dump (`ObjectId(203)`), so there is
// nothing to inject; a staging helper here would have made the live test synthetic
// again. `give_bolt` below survives because the positive control needs an
// interactive card the recorded board does not contain.

/// Stage a castable Lightning Bolt in `player`'s hand, mirroring
/// `GameScenario::add_bolt_to_hand` (same `Effect::DealDamage` ability, same
/// absence of a printed mana cost) so the positive control below is the same
/// interaction the shipped fixtures use.
fn give_bolt(state: &mut GameState, player: PlayerId) -> ObjectId {
    give_bolt_with_cost(state, player, ManaCost::zero())
}

/// `give_bolt` with a PRINTED cost, so a row can stage an interaction the seat
/// cannot yet afford. `GameObject::mana_cost` is the field the castability probe
/// reads, and `ManaCost`'s `Default` is `zero()` (`GameObject::new` seeds both
/// cost fields from it), so the free-Bolt caller above is byte-unchanged.
///
/// BOTH fields are assigned, but the LIVE one is what carries this helper —
/// `base_mana_cost` is NOT load-bearing for the objects staged here, and saying
/// otherwise would be a justification the next reader trusts.
///
/// READ FROM SOURCE (three call sites, not a runtime probe — the evidence grade
/// is stated because overclaiming it is the very habit this comment replaces).
/// `game::layers`' base→live reseed does run
/// `mana_cost = base_mana_cost.clone()` (`seed_live_characteristics_from_base`),
/// and every consumer here does reach the object through
/// `ai_support::shortcut_probe`, which flushes layers — but the full pass
/// applies that reseed (via `reset_recipient_to_base`) only over
/// `battlefield_phased_in_ids()`, and the hand branch of the same pass resets
/// `keywords` alone. `layers::layer_pass_materializes_keywords`' doc is the
/// in-repo authority for that split ("Battlefield — resets the full
/// characteristic set" vs "Hand — keywords-only reset"); the incremental arm
/// resets only battlefield entrants and their hosts, so it cannot reach a hand
/// object either. This object is staged to `Zone::Hand`, so no pass reseeds its
/// `mana_cost`. `GameObject`'s
/// `sync_missing_base_characteristics` — which the hand branch DOES call —
/// would in fact back-fill `base_mana_cost` from the live field, the opposite
/// direction.
///
/// `base_mana_cost` is set for symmetry: it keeps the two fields from
/// disagreeing on a freshly minted object, and it keeps the helper correct if
/// the hardcoded `Zone::Hand` below ever becomes the battlefield, where the
/// reseed WOULD restore the default (free) cost over the printed one and
/// silently leave an "otherwise-unaffordable" premise measuring nothing.
fn give_bolt_with_cost(state: &mut GameState, player: PlayerId, cost: ManaCost) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = engine::game::zones::create_object(
        state,
        card_id,
        player,
        "Lightning Bolt".to_string(),
        Zone::Hand,
    );
    let ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    );
    let obj = state.objects.get_mut(&id).expect("just created");
    obj.card_types
        .core_types
        .push(engine::types::card_type::CoreType::Instant);
    obj.base_card_types = obj.card_types.clone();
    obj.base_mana_cost = cost.clone();
    obj.mana_cost = cost;
    obj.abilities = std::sync::Arc::new(vec![ability.clone()]);
    obj.base_abilities = std::sync::Arc::new(vec![ability]);
    state.layers_dirty = LayersDirty::full();
    id
}

/// Declare the offer this board minted, then poll `seat` by walking the APNAP
/// queue with MANUAL Accepts (never the AI's answer, which would stop the
/// queue). Returns the state parked at `seat`'s response window.
fn declare_and_poll(state: &GameState, seat: PlayerId) -> GameState {
    let WaitingFor::LoopShortcut {
        proposer,
        ref schema,
        ..
    } = state.waiting_for
    else {
        panic!(
            "declare_and_poll expects a LoopShortcut window, got {:?}",
            state.waiting_for
        );
    };
    let mut s = state.clone();
    apply(
        &mut s,
        proposer,
        GameAction::DeclareShortcut {
            count: schema.iteration_count.clone(),
            template: None,
        },
    )
    .expect("the proposer declares its own offer");
    for _ in 0..8 {
        match s.waiting_for {
            WaitingFor::RespondToShortcut { player, .. } if player == seat => return s,
            WaitingFor::RespondToShortcut { player, .. } => {
                apply(
                    &mut s,
                    player,
                    GameAction::RespondToShortcut {
                        response: ShortcutResponse::Accept,
                    },
                )
                .expect("manual Accept advances the APNAP queue");
            }
            _ => break,
        }
    }
    panic!(
        "reach-guard: {seat:?} must be polled; stopped at {:?}",
        s.waiting_for
    );
}

/// The non-`PassPriority` actions available to `seat`, rendered with the source
/// object's name / zone / controller so a reach-guard failure names the board
/// rather than an opaque id.
fn non_pass_actions(state: &GameState, seat: PlayerId) -> Vec<String> {
    probe_actions(state, seat)
        .iter()
        .filter(|a| !matches!(a, GameAction::PassPriority))
        .map(|a| match a {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => {
                let o = state.objects.get(source_id);
                format!(
                    "ActivateAbility({source_id:?} {:?} #{ability_index} zone={:?} controller={:?})",
                    o.map(|o| o.name.clone()),
                    o.map(|o| o.zone),
                    o.map(|o| o.controller)
                )
            }
            other => format!("{other:?}"),
        })
        .collect()
}

// ===========================================================================
// V1 / V5 — ACCEPTANCE (a) on the REAL 4-player board, LIVE PATH.
// ===========================================================================

/// **The acceptance row.** A real 4-player Dina / Bloodthirsty Conqueror drain,
/// restored through the production chokepoint
/// (`PersistedGameState::into_game_state()`, the same path the server's
/// `from_persisted` and WASM's `decode_restored_game_state` funnel through) and
/// driven beat by beat through the public `apply()` boundary. No `GameScenario`
/// anywhere on this row: a synthetic board going green while the live 4p case
/// failed is a documented failure mode in this lane.
///
/// The defect, on this exact board: seat P2 controls **`ObjectId(203)`,
/// "Terramorphic Expanse"**, on the battlefield. A non-mana activated ability is
/// unconditionally "meaningful" to stage 1, so the shipped one-stage predicate
/// answered `Shorten { at_iteration: 0 }` — handing P2 a real priority window
/// whose only content is cracking its own fetchland, which cannot touch the
/// drain. Stage 2 answers `Accept`.
///
/// **V5 — bounded offers are NOT exempt.** MEASURED on this board: the offer
/// mints at beat 21 carrying `predicted_winner: None` and
/// `IterationCount::Fixed(25)`. It is the BOUNDED class, not the `UntilLethal`
/// class the synthetic rows use, and `predicted_winner: None` additionally
/// proves arm (A) cannot be what produces the `Accept` below — only arm (B)
/// can. Re-introducing an `UntilLethal`-only gate makes this row return
/// `Shorten` and fail.
///
/// **Why the flip set is exactly {P2}, asserted rather than asserted-about.**
/// P1 and P3 are polled on the same board and hold nothing, so they answer at
/// stage 1 and are unaffected. That is the sibling control: the change is
/// surgical, not a blanket flip to `Accept`.
///
/// MUTANTS — the `Accept` assertion flips under both:
/// * **DROP** (delete stage 2) ⇒ `Shorten { at_iteration: 0 }`, which is the
///   shipped behaviour and therefore the defect itself;
/// * **TRIVIALIZE** (`any_action_may_interfere ≡ true`) ⇒ `Shorten`.
///
/// The opposite direction is `v1_positive_control_*` below, on this same board.
#[test]
fn v1_live_path_fetchland_seat_accepts_on_the_real_4p_board() {
    let mut board = live_path_board();
    assert!(
        !matches!(board.waiting_for, WaitingFor::LoopShortcut { .. }),
        "reach-guard: the dump must not ship AT an offer — the offer is this drive's product, \
         not its input; got {:?}",
        board.waiting_for
    );
    let beat = drive_to_offer(&mut board, 400).expect(
        "CR 732.2a: the offer must FIRE on this real 4p drain. A failure here is the offer \
         never being raised, not a fixture accident",
    );
    let WaitingFor::LoopShortcut {
        predicted_winner,
        ref schema,
        ..
    } = board.waiting_for
    else {
        unreachable!("drive_to_offer only returns at a LoopShortcut window");
    };

    // ── V5's premise, read off the live offer rather than assumed ──
    assert_eq!(
        predicted_winner, None,
        "V5: the BOUNDED class mints no crown — so arm (A) is structurally unable to produce \
         the Accept below, and only arm (B) can (offer beat {beat})"
    );
    assert_eq!(
        schema.iteration_count,
        IterationCount::Fixed(25),
        "V5: a FINITE count is the point — stage 2 takes the identical rule for it and for \
         the UntilLethal class"
    );

    // ── the row: P2, whose only action is its own fetchland ──
    let polled = declare_and_poll(&board, P2);
    let non_pass = non_pass_actions(&polled, P2);

    assert_eq!(
        non_pass.len(),
        1,
        "REACH-GUARD: P2 must hold EXACTLY ONE non-pass action. Two would let the fold's \
         .any() reach Shorten through the other one and this row would pass for the wrong \
         reason; zero would make it the stage-1 path; got {non_pass:?}"
    );
    assert!(
        non_pass[0].contains("Terramorphic Expanse")
            && non_pass[0].contains("zone=Some(Battlefield)")
            && non_pass[0].contains("controller=Some(PlayerId(2))"),
        "REACH-GUARD: that one action must be P2's OWN battlefield fetchland — the object the \
         diagnosis pinned; got {non_pass:?}"
    );

    // NON-VACUITY PIN. The guards above read the FLAT list, which cannot contain
    // a BATTLEFIELD mana activation: `candidates.rs` excludes it at generation
    // (`!is_mana_ability(&ability_def)`), and a land's `TapLandForMana` is
    // additionally dropped by `flat_priority_actions_with_probe`'s
    // `GameAction::is_mana_ability` filter. (It CAN contain a hand- or
    // graveyard-zone mana activation, which has its own candidate loop and is a
    // `GameAction::ActivateAbility` — so the filter never sees it. That class is
    // not on this board, and what would catch it is the `non_pass.len() == 1`
    // REACH-GUARD above, NOT the assertion below: such an activation is already
    // IN the flat list, so it shows up as a second non-pass action, while
    // `stage_two_action_set` only APPENDS `meaningful_sacrifice_mana_actions` —
    // a non-sacrifice one adds nothing and `stage_two == flat` still holds.)
    // The set stage 2 actually folds over is WIDER still — `stage_two_action_set`
    // re-admits sacrifice-for-mana activations — so without this the flagship is
    // blind to exactly the class that would vacuate the feature: a seat that
    // acquired a Lotus-Petal-shaped permanent during the drive would silently
    // start Shortening and this row would flip.
    let (probe, flat) = engine::ai_support::shortcut_probe(&polled, P2);
    let stage_two = engine::ai_support::stage_two_action_set(probe.state(), &flat);
    assert_eq!(
        stage_two, flat,
        "NON-VACUITY: P2 must own NO mana-producing action on this board, so its Accept is \
         produced by the fetchland's confinement and NOT by the absence of a widening. If this \
         fails, the flagship Accept is no longer measuring what it claims — re-derive the row, \
         do NOT relax the assertion"
    );

    assert!(
        stage_one_meaningful(&polled, P2),
        "REACH-GUARD: stage 1 (POSSIBILITY, untouched here) must still return true. An Accept \
         produced by stage 1 would prove nothing about stage 2 — this is the assertion that \
         makes the row non-vacuous"
    );

    assert_eq!(
        engine::ai_support::smart_shortcut_response(&polled, P2),
        ShortcutResponse::Accept,
        "ACCEPTANCE (a), LIVE PATH: cracking its own fetchland cannot touch the drain, so P2 \
         must not buy a priority window with it. CR 732.2c is satisfied by ANY different \
         choice, which is exactly why satisfying it carries no efficacy"
    );

    // ── sibling control: the flip set is exactly {P2} ──
    for seat in [P1, P3] {
        let other = declare_and_poll(&board, seat);
        assert!(
            !stage_one_meaningful(&other, seat),
            "sibling control: {seat:?} holds nothing on this board, so it answers at stage 1 \
             and stage 2 never runs for it; got {:?}",
            non_pass_actions(&other, seat)
        );
        assert_eq!(
            engine::ai_support::smart_shortcut_response(&other, seat),
            ShortcutResponse::Accept,
            "sibling control: {seat:?} is unchanged by this fix — the flip set is exactly {{P2}}"
        );
    }
}

/// The positive control for the row above, on the SAME real board: give P2 a
/// castable Lightning Bolt and it must Shorten.
///
/// This is what makes `v1_live_path_*`'s `Accept` attributable. The same
/// instrument, on the same restored 4p board, at the same offer, returns BOTH
/// values — so the `Accept` is caused by the fetchland's confinement and not by
/// anything about the board, the beat, or the offer class. Without this row a
/// classifier that answered `Accept` unconditionally would pass.
///
/// MUTANT: `any_action_may_interfere ≡ false` ⇒ `Accept` — this row flips.
///
/// This row's expected value (`Shorten`) IS the shipped value, so **DROP cannot
/// flip it**. Its whole discriminating power is the TRIVIALIZE arm named above.
#[test]
fn v1_positive_control_interactive_seat_still_shortens_on_the_real_4p_board() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("the offer must fire");
    let bolt = give_bolt(&mut board, P2);

    let polled = declare_and_poll(&board, P2);
    let actions = probe_actions(&polled, P2);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == bolt)),
        "REACH-GUARD: the Bolt must actually be castable here, or this control cannot fire; \
         got {:?}",
        non_pass_actions(&polled, P2)
    );

    assert_eq!(
        engine::ai_support::smart_shortcut_response(&polled, P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "ACCEPTANCE (b), LIVE PATH: a seat holding real interaction must still get its window. \
         This is the assertion any over-broad confinement rule destroys first"
    );
}

/// NEGATIVE CONTROL — and its limits are the point.
///
/// The previously-tracked `dina_conqueror_4p` board is the same matchup at a
/// beat where NO seat holds any meaningful priority action. MEASURED: the offer
/// mints at beat 19 with `predicted_winner: None`, `IterationCount::Fixed(30)`,
/// and all three polled seats (P1, P2, P3) enumerate exactly
/// `["PassPriority"]` — length one, NOT empty; it is
/// `has_meaningful_priority_action` returning false that produces the Accept,
/// not an empty action vector.
///
/// **This board therefore has ZERO discriminating power for acceptance (a), and
/// it is retained only for what it CAN show.** The defect needs a seat holding
/// a meaningful-but-vacuous action; a board with no such seat cannot exhibit it,
/// so this row passes identically with and without stage 2. Do not promote it
/// to an acceptance row, and do not read its green as evidence about the fix:
/// what it pins is the one-way property that stage 2 must not make a quiet
/// board start Shortening.
#[test]
fn v1_control_quiet_board_is_unchanged_and_cannot_discriminate() {
    let mut board = quiet_board();
    let beat = drive_to_offer(&mut board, 400).expect("the quiet board still mints an offer");
    let WaitingFor::LoopShortcut {
        predicted_winner,
        ref schema,
        ..
    } = board.waiting_for
    else {
        unreachable!()
    };
    assert_eq!(predicted_winner, None, "bounded class (offer beat {beat})");
    assert_eq!(schema.iteration_count, IterationCount::Fixed(30));

    for seat in [P1, P2, P3] {
        let polled = declare_and_poll(&board, seat);
        let actions = probe_actions(&polled, seat);
        assert_eq!(
            actions,
            vec![GameAction::PassPriority],
            "the premise of this control: {seat:?} enumerates exactly one action, and it is a \
             pass. If this ever fails the board is no longer quiet and the row's `cannot \
             discriminate` claim needs re-deriving"
        );
        assert!(
            !stage_one_meaningful(&polled, seat),
            "and it is stage 1, not an empty action list, that answers"
        );
        assert_eq!(
            engine::ai_support::smart_shortcut_response(&polled, seat),
            ShortcutResponse::Accept,
            "no-regress: stage 2 must not make a quiet seat start Shortening"
        );
    }
}

// ===========================================================================
// V8 — the SECOND window this authority answers: the pre-cast copy route.
// ===========================================================================

const PRECAST_EPOCH: u64 = 7;
const PRECAST_BREAKPOINT: u64 = 99;

/// Re-park an already-polled state at the PRE-CAST responder window, board
/// untouched.
///
/// Hand-built from the engine's own constructor shape
/// (`game::precast_copy_shortcut::responder_wait`), exactly as the shipped
/// `precast_copy_shortcut.rs` fixture `precast_shortcut_response_state` does.
/// Sound HERE specifically: `smart_shortcut_response` reads `waiting_for` for
/// one thing only (the crown — and this variant carries no crown to read) and
/// then re-parks its own probe clone at `Priority`, so the efficacy answer is a
/// function of the BOARD. Driving a genuine pre-cast copy route would supply a
/// different board, which is the one variable this row must hold fixed against
/// `v1b`/`v3` above.
fn as_precast_window(state: &GameState, seat: PlayerId) -> GameState {
    let mut s = state.clone();
    s.waiting_for = WaitingFor::RespondToPrecastCopyShortcut {
        player: seat,
        epoch: PRECAST_EPOCH,
        breakpoint_ids: vec![PRECAST_BREAKPOINT],
        remaining_players: Vec::new(),
    };
    s
}

/// The pre-cast reply the PRODUCTION candidate builder emits for this state.
/// Goes through `ai_support::candidate_actions`, i.e. the real consumer at
/// `candidates::candidate_actions_broad_with_probe`, so the
/// `ShortcutResponse` → `PrecastCopyShortcutResponse` mapping is measured too.
fn precast_candidate_response(state: &GameState) -> PrecastCopyShortcutResponse {
    let replies: Vec<PrecastCopyShortcutResponse> = engine::ai_support::candidate_actions(state)
        .iter()
        .filter_map(|candidate| match &candidate.action {
            GameAction::PrecastCopyShortcut { epoch, response } if *epoch == PRECAST_EPOCH => {
                Some(response.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        replies.len(),
        1,
        "reach-guard: the pre-cast responder window must offer exactly one reply candidate, \
         otherwise this row is reading something else; got {replies:?}"
    );
    replies[0].clone()
}

/// `smart_shortcut_response` is the single authority for TWO accept-or-shorten
/// windows, not one: `candidates::candidate_actions_broad_with_probe` routes
/// `WaitingFor::RespondToPrecastCopyShortcut` through it as well and maps the
/// answer onto `PrecastCopyShortcutResponse`. Stage 2 therefore changed behaviour
/// at that window too, and this row measures it instead of assuming it.
///
/// Uniform treatment is the deliberate choice: both windows ask the identical
/// question — is a real priority window worth taking here — so a seat whose only
/// action cannot touch the loop should decline both. Arm (A) is separately
/// INAPPLICABLE here rather than merely skipped: `RespondToPrecastCopyShortcut`
/// carries no proposal summary and hence no `predicted_winner` field, so there is
/// no crown to read. Stage 1 and arm (B) both apply and both run.
///
/// NON-VACUITY, and it is arm 2 that supplies it: `candidates.rs` maps a
/// `Shorten` with an EMPTY `breakpoint_ids` back to `Accept`, so on a
/// breakpoint-less prompt both answers would collapse to `Accept` and arm 1
/// would pass for free. Arm 2 returns `Shorten { breakpoint_id }` off the same
/// staged breakpoint list, which proves the mapping is live and arm 1's `Accept`
/// is the efficacy verdict rather than the collapse.
///
/// MUTANTS — both RUN, not reasoned about:
/// * `any_action_may_interfere ≡ true` ⇒ arm 1's production-path assertion fails
///   with `left: Shorten { breakpoint_id: 99 }, right: Accept`. This is also the
///   direct measurement of the non-vacuity claim above: the mapping's `Shorten`
///   branch really is reachable on this prompt.
/// * `any_action_may_interfere ≡ false` ⇒ arm 2 fails with
///   `left: Accept, right: Shorten { breakpoint_id: 99 }`.
///
/// DROP (delete stage 2 entirely) flips arm 1 the same way and leaves arm 2 at
/// its shipped value; the first mutant covers that direction.
#[test]
fn v8_precast_window_takes_the_same_efficacy_answer() {
    // Arm 1 — the confined fetchland seat.
    let (mut runner, kickoff) = optional_cascade(|s| {
        s.add_land_from_oracle(P2, "Terramorphic Expanse", TERRAMORPHIC);
    });
    respond_window_at(&mut runner, kickoff, P2);
    let fetch_precast = as_precast_window(runner.state(), P2);

    assert!(
        stage_one_meaningful(&fetch_precast, P2),
        "REACH-GUARD: stage 1 must still say `meaningful` at the PRE-CAST window, or this arm \
         measures the stage-1 path and says nothing about stage 2; got {:?}",
        non_pass_actions(&fetch_precast, P2)
    );
    // The PRODUCTION-PATH assertion comes first deliberately: it is the one that
    // has to discriminate, and an authority-level assertion ahead of it would
    // absorb every mutant before the candidate builder was ever exercised.
    assert_eq!(
        precast_candidate_response(&fetch_precast),
        PrecastCopyShortcutResponse::Accept,
        "the pre-cast candidate builder must carry stage 2's answer through: a window whose only \
         content is cracking one's own fetchland is worth no more on the pre-cast route than on \
         the generic one"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&fetch_precast, P2),
        ShortcutResponse::Accept,
        "and the authority itself answers identically at both windows"
    );

    // Arm 2 — the same board plus real interaction. The window is still granted.
    let (mut armed, armed_kickoff) = optional_cascade(|s| {
        s.add_land_from_oracle(P2, "Terramorphic Expanse", TERRAMORPHIC);
        s.add_basic_land(P2, ManaColor::Red);
        s.add_bolt_to_hand(P2);
    });
    respond_window_at(&mut armed, armed_kickoff, P2);
    let armed_precast = as_precast_window(armed.state(), P2);

    assert!(
        probe_actions(&armed_precast, P2)
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { .. })),
        "reach-guard: the Bolt must be castable at the pre-cast window too, or this arm repeats \
         arm 1; got {:?}",
        non_pass_actions(&armed_precast, P2)
    );
    assert_eq!(
        precast_candidate_response(&armed_precast),
        PrecastCopyShortcutResponse::Shorten {
            breakpoint_id: PRECAST_BREAKPOINT
        },
        "ACCEPTANCE (b) on the pre-cast route: a seat holding real interaction keeps its window, \
         named at the breakpoint the engine issued to it. This is also arm 1's non-vacuity proof \
         — the Shorten branch of the mapping is reachable on this exact prompt"
    );
}

// ===========================================================================
// V9 — COVERAGE INVARIANT: stage 2 classifies everything stage 1 counted.
// ===========================================================================

/// Krark-Clan Ironworks, verbatim, verified byte-for-byte against MTGJSON
/// `AtomicCards.json` (`.data["Krark-Clan Ironworks"][0].text`; `.types` is
/// `["Artifact"]`). CARD PROVENANCE, in the sense the header block above defines
/// — it is the shape under test, so a paraphrase could take a different parser
/// branch and go green while the real card stayed broken.
///
/// Why THIS card: its activation is the issue #544 shape — a sacrifice-for-mana
/// ability that `legal_actions` structurally omits while
/// `has_meaningful_priority_action`'s second rung still counts it off `state`.
/// That gap between the two stages' inputs is the whole subject of this section.
const IRONWORKS: &str = "Sacrifice an artifact: Add {C}{C}.";

/// Stage the Ironworks on `player`'s battlefield, ability taken from the REAL
/// parser (see `give_parsed_card`, which this is now one call into).
///
/// It was an inlined copy of that helper until the two were diffed field by
/// field and found byte-equivalent — same parse call, same assertion text once
/// `name` is substituted, same `create_object` arguments, same core-type push,
/// same `base_card_types`/`abilities`/`base_abilities` assignment, same
/// `layers_dirty`. Delegating is behaviour-identical BY CONSTRUCTION, and the
/// divergence it prevents already fired once inside this same change:
/// `base_mana_cost` reached one construction path and not the other.
fn give_ironworks(state: &mut GameState, player: PlayerId) -> ObjectId {
    give_parsed_card(
        state,
        player,
        "Krark-Clan Ironworks",
        IRONWORKS,
        CoreType::Artifact,
        Zone::Battlefield,
    )
}

/// A vanilla artifact for the Ironworks to eat. No abilities and no card claim:
/// it exists so the sacrifice cost is payable, and it must contribute no action
/// of its own or it would give the fold a second way to reach `Shorten`.
fn give_artifact_fodder(state: &mut GameState, player: PlayerId) -> ObjectId {
    let id = engine::game::zones::create_object(
        state,
        CardId(state.next_object_id),
        player,
        "Test Artifact Fodder".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).expect("just created");
    obj.card_types
        .core_types
        .push(engine::types::card_type::CoreType::Artifact);
    obj.base_card_types = obj.card_types.clone();
    obj.summoning_sick = false;
    state.layers_dirty = LayersDirty::full();
    id
}

/// The optional-cascade board with an Ironworks + one artifact staged on P2
/// AFTER the response window opens, so the addition cannot perturb the drive to
/// the offer.
fn ironworks_seat_polled() -> (GameRunner, ObjectId) {
    let (mut runner, kickoff) = optional_cascade(|_| {});
    respond_window_at(&mut runner, kickoff, P2);
    let ironworks = give_ironworks(runner.state_mut(), P2);
    let _fodder = give_artifact_fodder(runner.state_mut(), P2);
    (runner, ironworks)
}

/// The invariant, asserted directly on the set rather than inferred from a
/// verdict: **stage 2 folds over every action stage 1 counted as meaningful.**
///
/// This is the row that survives a future reclassification. `v9b` below reads
/// the Ironworks' *verdict*, which a later, more precise `filter_is_actor_owned`
/// could legitimately flip (CR 701.21a: "A player can't sacrifice something that
/// isn't a permanent, or something that's a permanent they don't control" — so
/// "Sacrifice an artifact" IS actor-owned in fact, merely not PROVEN so by this
/// AST). This row does not depend on the verdict at all — it
/// pins that the action is *handed to the classifier*, which is what keeps a
/// newly added stage-1 rung from silently reintroducing Accept-by-omission.
///
/// The three assertions are the defect's three premises, in order:
///  1. the activation is ABSENT from the flat list (issue #544 grouping), so
///  2. stage 1 nonetheless counts it — via the `state`-reading second rung — and
///  3. `stage_two_action_set` therefore has to put it back, or the two stages
///     read different inputs.
///
/// Revert-probe (EXECUTED, see the report): defining `stage_two_action_set` as
/// `flat_actions.to_vec()` fails assertion 3.
#[test]
fn v9a_stage_two_folds_over_every_action_stage_one_counted() {
    let (runner, ironworks) = ironworks_seat_polled();
    let activation = GameAction::ActivateAbility {
        source_id: ironworks,
        ability_index: 0,
    };

    let (probe, flat) = engine::ai_support::shortcut_probe(runner.state(), P2);
    assert!(
        !flat.contains(&activation),
        "PREMISE 1: sacrifice-for-mana stays out of the flat priority list (issue #544) — if it \
         were present, the two stages would already agree and this row would be vacuous; got \
         {flat:?}"
    );
    assert!(
        stage_one_meaningful(runner.state(), P2),
        "PREMISE 2: stage 1 counts it anyway, off `state` rather than off that list — this is the \
         asymmetry the invariant exists to close; got {flat:?}"
    );

    let stage_two = engine::ai_support::stage_two_action_set(probe.state(), &flat);
    assert!(
        stage_two.contains(&activation),
        "THE INVARIANT: every action stage 1 counted as meaningful must be handed to stage 2. An \
         action the classifier never sees reaches no arm, so the fail-closed default cannot save \
         it and the seat Accepts BY OMISSION; got {stage_two:?}"
    );
}

/// The response-level discriminator: with the invariant restored, this seat
/// Shortens; without it, it Accepts.
///
/// It discriminates because the Ironworks activation classifies `MayInterfere`,
/// and since V10 it does so through TWO independent legs. Its `Sacrifice` cost
/// filter (`Typed{Artifact}`) names no controller, so `filter_is_actor_owned`
/// cannot PROVE actor ownership and `cost_window_reach` takes the fail-closed
/// direction; and its `Effect::Mana` head is no longer allowlisted either, so
/// the head alone would carry the verdict.
///
/// The counterfactual this doc used to carry — "a sacrifice ability whose filter
/// *were* proven actor-owned would classify `OwnResourcesOnly` … i.e. would not
/// discriminate" — is FALSE since `Effect::Mana` left the allowlist, and
/// `v10b` is the row that refutes it on exactly that shape (Lotus Petal's
/// `SelfRef` sacrifice IS proven actor-owned, and the seat still Shortens). What
/// survives is the sentence's purpose: the unproven filter is still what makes
/// THIS row's own MUTANT discriminate, because that mutant deletes the widening
/// rather than touching the classifier.
///
/// REACH-GUARD: the flat list is asserted to be EXACTLY `[PassPriority]`. That
/// is what makes the verdict attributable: `PassPriority` is the classifier's
/// one `false` arm, so the flat half cannot reach `Shorten` on its own and the
/// only action that can is the one the widening added.
///
/// MUTANT (EXECUTED, see the report): `stage_two_action_set ≡ flat_actions
/// .to_vec()` — i.e. delete the widening — flips this row to `Accept`.
///
/// This row's verdict is now OVER-DETERMINED, which changes what a future
/// refinement does to it. The doc used to predict that a `filter_is_actor_owned`
/// which learns to prove "Sacrifice an artifact" actor-owned (CR 701.21a bounds
/// the actor to permanents they CONTROL, which is the fact such a refinement
/// would be reading) would red this row; it will not, because the unallowlisted
/// `Effect::Mana` head carries `MayInterfere` unconditionally. The guidance the
/// prediction carried still stands and is the part to keep: if this row ever
/// does red, re-derive the fixture on an ability whose reach is genuinely
/// unproven — NOT delete the row, and NOT weaken the classifier. Its
/// discriminating power comes from its MUTANT rather than from the cost filter's
/// imprecision, and `v9a` above holds the invariant meanwhile.
#[test]
fn v9b_a_sacrifice_for_mana_seat_still_gets_its_window() {
    let (runner, ironworks) = ironworks_seat_polled();
    let activation = GameAction::ActivateAbility {
        source_id: ironworks,
        ability_index: 0,
    };

    let flat = probe_actions(runner.state(), P2);
    assert_eq!(
        flat,
        vec![GameAction::PassPriority],
        "REACH-GUARD: the flat half must be exactly the one action the classifier answers `false` \
         on, or a `Shorten` here is not attributable to the widening; got {flat:?}"
    );
    assert!(
        stage_one_meaningful(runner.state(), P2),
        "reach-guard: stage 1 must return true, or the seat resolves at stage 1 and never reaches \
         the fold under test"
    );

    assert_eq!(
        engine::ai_support::smart_shortcut_response(runner.state(), P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "a seat whose only meaningful action is {activation:?} must keep its window: the AST does \
         not prove the sacrificed artifact is this seat's own, and stage 2 must not Accept a \
         reach it cannot rule out"
    );
}

// ===========================================================================
// V10 — MANA IS FUNGIBLE REACH, not a confined own resource.
//
// CR 106.4's first sentence ("that mana goes into a player's mana pool") is the
// half an earlier `Effect::Mana` arm quoted; the rest of the same rule says the
// mana "can be used to pay costs immediately", CR 106.1 says paying costs is
// mana's whole function, and CR 601.2g runs mana abilities during the very cast
// they fund. So producing mana widens what the polled seat can do inside the
// window `game::engine`'s `RespondToShortcut(Shorten)` arm hands back, and the
// classifier — which reads ONE ability's AST and no other object — cannot prove
// otherwise. Both rows below ride the REAL 4p board through the same production
// chokepoint the flagship uses.
// ===========================================================================

/// Dark Ritual, verbatim. CARD PROVENANCE in the sense the header block defines.
/// The `Effect::Mana` head with `cost: None` is the point — nothing but the head
/// can carry this object's verdict.
const DARK_RITUAL: &str = "Add {B}{B}{B}.";

/// Lotus Petal, verbatim. CARD PROVENANCE. This is the maintainer's named
/// "actor-owned sacrifice-for-mana" class: the parser emits "Sacrifice this
/// artifact" as a `TargetFilter::SelfRef`, which `filter_is_actor_owned`
/// returns true for, so the cost leg reads confined and the mana head carries
/// the verdict ALONE. CR 701.21a bounds the actor to permanents they CONTROL,
/// which is the most that predicate can be grounded in — its first sentence
/// sends the sacrificed card to its OWNER's graveyard, so control is not
/// ownership and "confined" is narrower than the predicate's name suggests.
/// `shortcut_efficacy`'s `mana_production_is_reach_not_a_confined_own_resource`
/// quotes the rule in full and names the limit; nothing here rests on it,
/// because the mana head decides this verdict either way. That is also what
/// makes this row not a second `v9b`,
/// whose Ironworks reaches the same verdict through an UNPROVEN cost filter.
const LOTUS_PETAL: &str = "{T}, Sacrifice this artifact: Add one mana of any color.";

/// Sol Ring, verbatim. CARD PROVENANCE. The ORDINARY mana source: no sacrifice
/// leg, so `mana_ability_penalty` is `None` rather than `Sacrifices` and the
/// stage-2 widening must not re-admit it.
const SOL_RING: &str = "{T}: Add {C}{C}.";

/// Crop Rotation, verbatim, BOTH lines. CARD PROVENANCE (MTGJSON
/// `.data["Crop Rotation"][0].text`). The UNTAPPED fetch — the residual the
/// `enter_tapped` gate closes, and the class `Terramorphic Expanse` is NOT.
///
/// MEASURED on the live parser: this text yields exactly ONE ability with
/// `cost: None` — the additional-cost line is not carried — heading
/// `SearchLibrary { filter: Typed[Land], target_player: None }` over a
/// `ChangeZone { Library -> Battlefield, target: Any, enter_tapped: Unspecified }`
/// over `Shuffle { Controller }`. So every leg but the tap state reads confined,
/// which is precisely why this card was allowlisted before the gate. Its search
/// filter is ANY land card, which is what lets the funding lemma below find a
/// real basic Swamp in P2's own recorded library instead of staging one.
const CROP_ROTATION: &str = "As an additional cost to cast this spell, sacrifice a land.\n\
                             Search your library for a land card, put that card onto the \
                             battlefield, then shuffle.";

/// Rampant Growth, verbatim. CARD PROVENANCE. The TAPPED sibling and the whole
/// point of the matched pair: one printed word apart from Crop Rotation on every
/// axis this classifier reads, and the fetched land arrives unable to pay for
/// anything.
const RAMPANT_GROWTH: &str = "Search your library for a basic land card, put that card onto \
                              the battlefield tapped, then shuffle.";

/// Reshape the Earth, verbatim. CARD PROVENANCE — Oracle text verified on
/// Scryfall (`/cards/named?exact=Reshape+the+Earth`), a Sorcery whose whole
/// printed text is this one sentence.
///
/// Rampant Growth's UNRESTRICTED sibling, and the second one-word-apart pair in
/// this file: identical AST on every axis this classifier reads — no cost, one
/// ability, `SearchLibrary` over `ChangeZone { Library -> Battlefield, target:
/// Any, enter_tapped: Tapped }` over `Shuffle { Controller }`, no triggers, no
/// statics, no replacements, no keywords — and the search filter is
/// `Typed[Land]` where Rampant Growth's is `Typed[Land] + HasSupertype(Basic)`.
/// So the only thing that can separate their verdicts is WHICH CARDS THE ACTOR
/// COULD SELECT.
///
/// Elvish Reclaimer is the card the review named for this row and it CANNOT
/// serve, which is worth stating rather than leaving as an unexplained
/// substitution. Its ability costs "Sacrifice a land" — parsed
/// `AbilityCost::Sacrifice(Typed[Land])` with `controller: null` — and
/// `filter_is_actor_owned` proves nothing about an unqualified `Typed`, so
/// `cost_window_reach` already answers `MayInterfere` for it. It reads
/// `Shorten` with the selection gate and `Shorten` without it: an
/// over-determined row that would go green while measuring nothing.
const RESHAPE_THE_EARTH: &str = "Search your library for up to ten land cards, put them onto \
                                 the battlefield tapped, then shuffle.";

/// Stage a real card with its abilities taken from the REAL parser, not
/// hand-built: every verdict below is a function of the AST, so a hand-written
/// `AbilityDefinition` would let this section pass against a shape the pipeline
/// never produces. Parameterized over the three axes the V10 rows vary, and the
/// SINGLE staging path for parsed cards in this file — `give_ironworks` above
/// delegates here rather than keeping the byte-equivalent copy it used to be.
fn give_parsed_card(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    oracle: &str,
    core_type: CoreType,
    zone: Zone,
) -> ObjectId {
    let parsed = engine::parser::oracle::parse_oracle_text(oracle, name, &[], &[], &[]);
    assert_eq!(
        parsed.abilities.len(),
        1,
        "PREMISE: {name} parses to exactly one ability; got {:?}",
        parsed.abilities
    );
    let id = engine::game::zones::create_object(
        state,
        CardId(state.next_object_id),
        player,
        name.to_string(),
        zone,
    );
    let obj = state.objects.get_mut(&id).expect("just created");
    obj.card_types.core_types.push(core_type);
    obj.base_card_types = obj.card_types.clone();
    // No `summoning_sick = false` here: it would be a no-op that reads as
    // load-bearing. `zones::create_object` documents that it deliberately does
    // NOT set the flag (only the real ETB pipeline's
    // `reset_for_battlefield_entry` does), `add_to_zone` never touches it, and
    // `GameObject::new` already defaults it to `false`.
    obj.abilities = std::sync::Arc::new(parsed.abilities.clone());
    obj.base_abilities = std::sync::Arc::new(parsed.abilities);
    state.layers_dirty = LayersDirty::full();
    id
}

/// The Ritual is staged as an INSTANT and with no printed mana cost, and both
/// are deliberate. Without a core type the sorcery-timing gate refuses the cast
/// at this window and the funder never enters the action set — the row would go
/// vacuous silently. And Dark Ritual's real printed cost is `{B}` while P2
/// controls no mana source, so a printed-cost Ritual would itself be uncastable
/// and the row would measure an empty board twice. What the row measures is the
/// CLASSIFIER, which reads the AST and never the mana cost.
fn give_dark_ritual(state: &mut GameState, player: PlayerId) -> ObjectId {
    give_parsed_card(
        state,
        player,
        "Dark Ritual",
        DARK_RITUAL,
        CoreType::Instant,
        Zone::Hand,
    )
}

/// THE SIZING SITE. This source contributes **exactly 1** to
/// `game::mana_sources`' `feasible_mana_capacity`: its `AnyOneColor` production
/// carries `count: Fixed { value: 1 }`, and that arm of
/// `game::effects::mana`'s `resolve_mana_types_for_ability` returns
/// `vec![mana_type; amount]` — length `amount`, NOT `color_options.len()`.
///
/// MEASURED on this fixture, the only mana-gated action P2 owns at this window
/// is Angel of the Ruins' hand-zone plainscycling (object 210), whose cost is
/// `Composite[Mana{generic 2}, Discard{self_ref}]` — `{2}` generic. **The margin
/// is exactly 1 mana.** ANY staged P2 source contributing 2 or more unlocks that
/// cycling, puts an `ActivateAbility` in P2's flat list, and destroys `v10b`'s
/// attribution — its `non_pass` assertion is what fails, loudly, if that
/// happens. A future edit that raises this source's capacity silently destroys
/// the discrimination, so do NOT "strengthen" it.
fn give_lotus_petal(state: &mut GameState, player: PlayerId) -> ObjectId {
    give_parsed_card(
        state,
        player,
        "Lotus Petal",
        LOTUS_PETAL,
        CoreType::Artifact,
        Zone::Battlefield,
    )
}

/// Both fetches are staged as INSTANTS with no printed mana cost, for
/// `give_dark_ritual`'s reasons: without a core type the sorcery-timing gate
/// refuses the cast at this window and the funder never enters the action set,
/// and P2 controls no mana source so a printed cost would make the funder itself
/// uncastable and the row would measure an empty board twice.
///
/// The type is card-faithful for Crop Rotation (a real instant) and is NOT for
/// Rampant Growth (a real sorcery). That deviation is deliberate and stated: the
/// control's subject is the TAP STATE of the fetched land, and putting the two
/// fetches on different timing rails would confound exactly that axis.
fn give_crop_rotation(state: &mut GameState, player: PlayerId) -> ObjectId {
    give_parsed_card(
        state,
        player,
        "Crop Rotation",
        CROP_ROTATION,
        CoreType::Instant,
        Zone::Hand,
    )
}

/// The tapped half of `v10c`'s pair. See `give_crop_rotation` for the staging.
fn give_rampant_growth(state: &mut GameState, player: PlayerId) -> ObjectId {
    give_parsed_card(
        state,
        player,
        "Rampant Growth",
        RAMPANT_GROWTH,
        CoreType::Instant,
        Zone::Hand,
    )
}

/// Cast `fetch` at P2's OWN probe priority and drive real `apply()` beats until
/// it resolves, selecting a basic **Swamp** at the search prompt. Returns the
/// resulting state and the fetched land.
///
/// The Swamp is chosen by NAME rather than taken as the prompt's first offer:
/// several of the lands in P2's recorded library carry their own "enters tapped"
/// clause (Path of Ancestry, Goldmire Bridge, Temple of Silence, ...), and one of
/// those would make the funding lemma measure the FETCHED CARD's printed text
/// instead of the fetch effect's `enter_tapped` rider — the very axis under test.
fn resolve_fetch_choosing_a_swamp(arm: &GameState, fetch: ObjectId) -> (GameState, ObjectId) {
    let (probe, list) = engine::ai_support::shortcut_probe(arm, P2);
    let cast = list
        .iter()
        .find(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == fetch))
        .cloned()
        .expect("reach-guard: the fetch must be castable at P2's own probe priority");
    let mut state = probe.state().clone();
    // This drive is the first thing in this file to reach a library SHUFFLE, and
    // a shuffle is the one operation that reads the live ChaCha20 stream.
    // `restore_dump` above decodes through `into_game_state()`, which does NOT
    // reseed the `#[serde(skip)]` `rng` — production's restore does that in a
    // SECOND step (`engine-wasm`'s `restore_game_state` calls
    // `GameState::rehydrate_rng` right after decoding, issue #5466). Without it
    // the live stream sits at offset 0 while this dump's `rng_word_pos` is 313,
    // and `game::library`'s `capture_rng_word_pos` fails the entropy-high-water
    // invariant. Doing it here rather than in `restore_dump` keeps every other
    // row in this file byte-identical: the reseed lands only on this quarantined
    // funding clone, which no verdict is ever taken on.
    state.rehydrate_rng();
    apply(&mut state, P2, cast).expect("the fetch casts at P2's own probe priority");

    let mut chosen: Option<ObjectId> = None;
    for _ in 0..60 {
        let prompt = match &state.waiting_for {
            WaitingFor::SearchChoice { player, cards, .. } => Some((*player, cards.clone())),
            _ => None,
        };
        if let Some((player, cards)) = prompt {
            let swamp = cards
                .iter()
                .copied()
                .find(|id| state.objects.get(id).is_some_and(|o| o.name == "Swamp"))
                .expect("P2's recorded library must offer a basic Swamp to this search");
            apply(
                &mut state,
                player,
                GameAction::SelectCards { cards: vec![swamp] },
            )
            .expect("the searcher selects the Swamp");
            chosen = Some(swamp);
            continue;
        }
        if chosen.is_some() && !state.stack.iter().any(|entry| entry.id == fetch) {
            break;
        }
        dump_drive_one_beat(&mut state).expect("passing priority resolves the top of the stack");
    }
    (
        state,
        chosen.expect("reach-guard: the fetch must have PROMPTED a library search"),
    )
}

/// Capacity **2** (`Colorless { count: Fixed 2 }`), which is why the control it
/// serves asserts re-admission ONLY and never a verdict — see `v10b`.
fn give_sol_ring(state: &mut GameState, player: PlayerId) -> ObjectId {
    give_parsed_card(
        state,
        player,
        "Sol Ring",
        SOL_RING,
        CoreType::Artifact,
        Zone::Battlefield,
    )
}

/// V10a — a CAST mana spell funds an otherwise-unaffordable answer, so the seat
/// must keep its window.
///
/// The pair varies exactly one object: a Dark Ritual in P2's hand. Both arms
/// hold the same `{B}{B}{B}` Bolt, and assertion 1 is the operational definition
/// of "otherwise-unaffordable" — `feasible_mana_capacity` is battlefield-scoped,
/// so a Ritual sitting in HAND contributes 0 and the castability gate
/// structurally cannot see "cast a ritual first, then the Bolt". That two-step
/// is exactly what the priority window buys, and CR 601.2g / CR 117.1d are the
/// rules that make the mana available to pay a cost the moment it is produced.
///
/// BOTH halves of "otherwise-unaffordable" are measured, as in `v10b`. The
/// negative half is assertion 1 above, in both arms. The positive half is the
/// QUARANTINED funding lemma at the end of the row: the Ritual is driven through
/// the stack on the `apply()` boundary and the SAME Bolt is re-probed, so the
/// arithmetic (`{B}{B}{B}` added against `{B}{B}{B}` printed) is read off the
/// engine's castability gate rather than off the verbatim texts.
///
/// MUTANT: restoring `Effect::Mana {..} => WindowReach::OwnResourcesOnly` as
/// `effect_window_reach`'s first arm flips the SHORTEN arm to `Accept`. Under it
/// the Ritual's single ability folds `OwnResourcesOnly` (head `Effect::Mana`,
/// `cost: None`), and P2's remaining actions are `PassPriority` — the
/// classifier's one `false` arm — and the fetchland, whose verdict rides
/// untouched arms. The ACCEPT arm is unaffected by the mutation by construction:
/// its action set carries no `Effect::Mana` node at all.
///
/// Every `GameAction::CastSpell` matcher below binds `{ object_id, .. }` and
/// must NOT name `payment_mode`: a Petal- or Ritual-funded cast can be offered
/// as `CastPaymentMode::AutoExceptSacrificialMana`, and a mode-specific matcher
/// would fail with a message claiming the funding does not work.
#[test]
fn v10a_a_cast_mana_spell_that_funds_an_unaffordable_answer_keeps_its_window() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("CR 732.2a: the offer must fire on this real 4p drain");
    // ONE drive, ONE poll, shared by both arms — so staging cannot perturb the
    // drive, the offer schema, or the APNAP walk.
    let polled = declare_and_poll(&board, P2);

    let mut base = polled.clone();
    let bolt = give_bolt_with_cost(
        &mut base,
        P2,
        // `{B}{B}{B}` — sized to exactly what one Dark Ritual adds.
        ManaCost::Cost {
            shards: vec![ManaCostShard::Black; 3],
            generic: 0,
        },
    );

    // ── arm ACCEPT: the interaction alone ──
    let accept_arm = base.clone();
    // ── arm SHORTEN: same board, same Bolt, PLUS the funding piece ──
    let mut shorten_arm = base.clone();
    let ritual = give_dark_ritual(&mut shorten_arm, P2);

    for (label, arm) in [("ACCEPT", &accept_arm), ("SHORTEN", &shorten_arm)] {
        assert!(
            !probe_actions(arm, P2).iter().any(
                |a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == bolt)
            ),
            "PREMISE ({label} arm): the Bolt must be OTHERWISE-UNAFFORDABLE at poll time — a \
             hand-zone Ritual is invisible to the battlefield-scoped capacity scan, so the \
             castability gate cannot see the two-step the window buys; got {:?}",
            non_pass_actions(arm, P2)
        );
        assert!(
            stage_one_meaningful(arm, P2),
            "reach-guard ({label} arm): stage 1 must return true, or the seat answers at stage 1 \
             and the fold under test never runs"
        );
    }

    assert!(
        probe_actions(&shorten_arm, P2)
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == ritual)),
        "reach-guard: the FUNDER must really be castable, or the SHORTEN arm is the ACCEPT arm \
         with extra steps; got {:?}",
        non_pass_actions(&shorten_arm, P2)
    );

    let shorten_non_pass = non_pass_actions(&shorten_arm, P2);
    assert_eq!(
        shorten_non_pass.len(),
        2,
        "ATTRIBUTION + THRESHOLD SENTINEL, mirroring `v10b`'s: the SHORTEN arm must be the \
         ACCEPT arm's single fetchland PLUS the Ritual cast, and nothing else. The Ritual sits \
         in `Zone::Hand`, which the battlefield-scoped `feasible_mana_capacity` cannot see, so \
         it unlocks no third action — but that is DERIVED, and a fixture or capacity change \
         that added one `MayInterfere` action here would over-determine this row silently \
         instead of reddening. MEASURED at 2 on the committed fixture; got {shorten_non_pass:?}"
    );

    let accept_non_pass = non_pass_actions(&accept_arm, P2);
    assert_eq!(
        accept_non_pass.len(),
        1,
        "ATTRIBUTION: the ACCEPT arm's action set must be the flagship's exactly, so its Accept \
         is the already-shipped verdict and the pair's ONLY variable is the Ritual; got \
         {accept_non_pass:?}"
    );
    assert!(
        accept_non_pass[0].contains("Terramorphic Expanse")
            && accept_non_pass[0].contains("zone=Some(Battlefield)")
            && accept_non_pass[0].contains("controller=Some(PlayerId(2))"),
        "ATTRIBUTION: that one action must be P2's OWN battlefield fetchland; got \
         {accept_non_pass:?}"
    );

    // IDENTITY, not just cardinality. The sentinel above bounds the SHORTEN
    // arm's COUNT; this bounds its MEMBERSHIP, which is what that sentinel's
    // prose actually claims ("the ACCEPT arm's single fetchland PLUS the Ritual
    // cast, and nothing else"). Without this pair, a fixture or capacity change
    // that DROPPED the fetchland and added some unrelated `MayInterfere` action
    // still satisfies `len() == 2` AND the ritual reach-guard, and the row
    // silently measures the wrong pair.
    //
    // The fetchland is the same fixture object in both arms — both are clones
    // of one `base`, and only the Ritual is staged on top — so
    // `accept_non_pass[0]` is reusable verbatim instead of re-typing the three
    // `contains` substrings. MEASURED: the two formatted strings are byte-equal
    // (`ObjectId(203)` in both arms).
    //
    // The Ritual leg is matched on `object_id` ONLY. It must NOT name
    // `payment_mode`, for the reason this test's doc comment gives.
    let (_ritual_leg, other_legs): (Vec<&String>, Vec<&String>) = shorten_non_pass
        .iter()
        .partition(|a| a.starts_with(&format!("CastSpell {{ object_id: {ritual:?},")));
    assert_eq!(
        other_legs,
        vec![&accept_non_pass[0]],
        "ATTRIBUTION: the SHORTEN arm's set MINUS the Ritual must be the ACCEPT arm's set \
         EXACTLY — same fetchland object, same zone, same controller. Anything else means the \
         pair varies more than the single object it claims to vary, and the Shorten verdict below \
         is no longer attributable to the Ritual. This one equality also pins the Ritual leg: if \
         the partition matched nothing, `other_legs` carries both members and reddens; if it \
         matched both, `other_legs` is empty and reddens; got {shorten_non_pass:?}"
    );

    assert_eq!(
        engine::ai_support::smart_shortcut_response(&accept_arm, P2),
        ShortcutResponse::Accept,
        "the pair's negative arm: an unaffordable answer and a confined fetchland buy nothing"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&shorten_arm, P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "the pair's positive arm: producing mana is REACH (CR 106.1 / CR 106.4 / CR 601.2g). \
         The ritual is the one object that differs, and accepting here surrenders a live out"
    );

    // ── FUNDING LEMMA (CR 117.1d + CR 601.2g), deliberately QUARANTINED ──
    //
    // The negative half is already asserted above: the `{B}{B}{B}` Bolt is NOT
    // castable in EITHER arm at poll time. This is the positive half — the half
    // this row's title claims ("funds an unaffordable answer") — measured on the
    // production instrument rather than left to the verbatim texts' arithmetic.
    //
    // QUARANTINE, mirroring `v10b`'s lemma: this clone NEVER reaches
    // `smart_shortcut_response`. Both verdicts above are already taken; resolving
    // the Ritual inside an arm would change the very action set they were taken
    // on, and the funded board additionally unlocks the Angel's `{2}` cycling
    // (`give_lotus_petal`'s capacity note), which carries `MayInterfere` on a
    // route this pair does not model.
    let (probe, probe_list) = engine::ai_support::shortcut_probe(&shorten_arm, P2);
    let cast_ritual = probe_list
        .iter()
        .find(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == ritual))
        .cloned()
        .expect("the funder is castable — the reach-guard above asserts exactly this");
    let mut funded = probe.state().clone();
    apply(&mut funded, P2, cast_ritual).expect("the funder casts at P2's own probe priority");
    // Pass beats until the Ritual leaves the stack: it is on top, so the first
    // full pass round resolves it (MEASURED: 4 beats on this 4p board).
    for _ in 0..40 {
        if !funded.stack.iter().any(|entry| entry.id == ritual) {
            break;
        }
        dump_drive_one_beat(&mut funded).expect("passing priority resolves the top of the stack");
    }
    assert_eq!(
        funded.objects.get(&ritual).map(|o| o.zone),
        Some(Zone::Graveyard),
        "reach-guard: the Ritual must have RESOLVED, not merely been cast — CR 608.2n puts a \
         resolved instant into its owner's graveyard, so this is the observable that separates \
         'it resolved' from 'the drive capped out' and would otherwise red the funding \
         assertion below for the wrong reason"
    );
    assert!(
        probe_actions(&funded, P2)
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == bolt)),
        "FUNDING: with the Ritual resolved, the engine's own castability gate says the SAME \
         `{{B}}{{B}}{{B}}` Bolt that is unaffordable in BOTH arms above IS payable. That is the \
         two-step the priority window buys (CR 117.1d / CR 601.2g), measured rather than \
         inferred from the printed texts; got {:?}",
        non_pass_actions(&funded, P2)
    );
}

/// V10b — an ACTOR-OWNED sacrifice-for-mana seat keeps its window. This is the
/// maintainer's named class, and it is the row `v9b` structurally cannot be.
///
/// The pair varies exactly ONE object: a Lotus Petal on P2's battlefield. No
/// Bolt, no Swamp, no second permanent.
///
/// **CAPACITY THRESHOLD — the sizing this row lives or dies on.** The staged
/// source contributes exactly **1** to `feasible_mana_capacity` (see
/// `give_lotus_petal`), and the only mana-gated action P2 owns at this window is
/// Angel of the Ruins' hand-zone plainscycling at `{2}` generic. **The margin is
/// exactly 1 mana.** ANY staged P2 source contributing 2 or more unlocks that
/// cycling, puts an `ActivateAbility` in P2's flat list, and destroys this row's
/// attribution — the `non_pass` assertion below is what fails, loudly, if that
/// happens. A row that reds that way is a fixture-threshold artifact, not a
/// defect in the classifier: check whether `non_pass_actions` names object 210
/// first, shrink the staged source, and do NOT respond by relaxing an
/// `Effect::Mana` classification.
///
/// **P2's full reachable surface at this window, enumerated so the threshold is
/// not a claim about the hand alone.** MEASURED from the committed fixture:
/// battlefield 1 (Terramorphic Expanse, capacity 0); hand 7, of which six are
/// sorcery-speed (`Victimize`, `Plains`, `Arcane Signet`, `Commander's Sphere`,
/// `Compleated Huntmaster`, `Night's Whisper`) and only the Angel offers a
/// mana-gated instant-speed action; command zone 1 — `Brimaz, Blight of
/// Oreskos`, `{2}{W}{B}`, a CREATURE, and `format_config.command_zone` is true,
/// so `casting::spell_objects_available_to_cast`'s `Zone::Command` clause does
/// put it inside the candidate loop. The fixture is `active_player` 0 in
/// `CombatDamage` with priority at P2, so it is neither P2's turn nor a main
/// phase and sorcery-speed timing blocks Brimaz — and no amount of mana lifts a
/// timing gate. Library-zone activations are gated `is_active && stack_empty`
/// and P2 is not active. So `{2}` really is the whole threshold, over the whole
/// surface rather than over the hand.
///
/// MUTANT: restoring `Effect::Mana {..} => WindowReach::OwnResourcesOnly` flips
/// the SHORTEN arm to `Accept`. The Petal's cost leg is ALREADY
/// `OwnResourcesOnly` — `Composite[Tap, Sacrifice{SelfRef}]`, and
/// `filter_is_actor_owned` proves `SelfRef` at its first match arm — so with the
/// mana arm restored the whole ability folds `OwnResourcesOnly`. That is exactly
/// why this row is not a second `v9b`, whose verdict rides its UNPROVEN cost
/// filter and is untouched by the mutation.
#[test]
fn v10b_an_actor_owned_sacrifice_for_mana_seat_keeps_its_window() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("CR 732.2a: the offer must fire on this real 4p drain");
    let polled = declare_and_poll(&board, P2);

    // ── arm ACCEPT: the polled board, untouched ──
    let accept_arm = polled.clone();
    // ── arm SHORTEN: the polled board plus ONE object ──
    let mut shorten_arm = polled.clone();
    let petal = give_lotus_petal(&mut shorten_arm, P2);
    let activation = GameAction::ActivateAbility {
        source_id: petal,
        ability_index: 0,
    };

    assert!(
        !probe_actions(&shorten_arm, P2).contains(&activation),
        "PREMISE: the Petal's ability IS a mana ability (CR 605.1a), so candidate generation \
         excludes it from the flat list outright. This is the issue-#544 asymmetry the whole \
         V9/V10 section exists for — if it were present, stage 2 would already see it and the \
         widening below would be vacuous"
    );

    let non_pass = non_pass_actions(&shorten_arm, P2);
    assert_eq!(
        non_pass.len(),
        1,
        "ATTRIBUTION + THRESHOLD SENTINEL: if the Petal's mana made ANY P2 action affordable — a \
         hand card, or the Angel's {{2}} cycling — it would appear here and could carry \
         MayInterfere independently of the arm under test. See this row's capacity-threshold \
         note before touching the staged source; got {non_pass:?}"
    );
    assert!(
        non_pass[0].contains("Terramorphic Expanse")
            && non_pass[0].contains("zone=Some(Battlefield)")
            && non_pass[0].contains("controller=Some(PlayerId(2))"),
        "ATTRIBUTION: that one action must still be P2's OWN battlefield fetchland; got \
         {non_pass:?}"
    );

    let (probe, flat) = engine::ai_support::shortcut_probe(&shorten_arm, P2);
    let mut expected = flat.clone();
    expected.push(activation);
    assert_eq!(
        engine::ai_support::stage_two_action_set(probe.state(), &flat),
        expected,
        "the widening added EXACTLY the Petal. Order is determinate — `stage_two_action_set` is \
         the flat list chained with the meaningful sacrifice-mana actions — and the penalty is \
         `Sacrifices` because `mana_ability_penalty`'s FIRST clause is `cost_includes_sacrifice`, \
         which inspects `Composite` legs"
    );

    let (accept_probe, accept_flat) = engine::ai_support::shortcut_probe(&accept_arm, P2);
    assert_eq!(
        engine::ai_support::stage_two_action_set(accept_probe.state(), &accept_flat),
        accept_flat,
        "the negative half of the widening, on the same instrument: without the Petal there is \
         nothing to re-admit"
    );

    for (label, arm) in [("ACCEPT", &accept_arm), ("SHORTEN", &shorten_arm)] {
        assert!(
            stage_one_meaningful(arm, P2),
            "reach-guard ({label} arm): stage 1 must return true, or the seat answers at stage 1 \
             and the fold under test never runs"
        );
    }

    assert_eq!(
        engine::ai_support::smart_shortcut_response(&accept_arm, P2),
        ShortcutResponse::Accept,
        "the pair's negative arm — and it is the flagship's own verdict on the flagship's own \
         action set"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&shorten_arm, P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "the pair's positive arm: an actor-owned sacrifice-for-mana activation is two board \
         events, not a confined own resource. Stage 1 re-admits it PRECISELY because the \
         sacrifice is board-changing, so classifying it as confined here contradicted the stage \
         that handed it over"
    );

    // ── FUNDING LEMMA (CR 117.1d + CR 601.2g), deliberately QUARANTINED ──
    //
    // These two clones NEVER reach `smart_shortcut_response`. Staging the probe
    // spell into an arm above would put a `CastSpell` in the flat list whose own
    // `Effect::DealDamage` reaches the fail-closed arm — the pair would then
    // Shorten with the fix reverted and would stop measuring anything. The
    // separation IS the design; do not merge them.
    //
    // `{1}` generic on purpose: a generic residual is decided by comparing the
    // summed capacity against it, so both halves are decided on one read path
    // with zero slack. Without the Petal, P2's battlefield is one Terramorphic
    // Expanse, whose ability heads `SearchLibrary` and contributes 0, so the sum
    // is 0 < 1; with the Petal it is exactly 1 >= 1.
    let mut funded = shorten_arm.clone();
    let probe_spell = give_bolt_with_cost(&mut funded, P2, ManaCost::generic(1));
    assert!(
        probe_actions(&funded, P2).iter().any(
            |a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == probe_spell)
        ),
        "FUNDING: with the Petal on the battlefield the engine's own castability gate says a \
         {{1}} interaction IS payable — by activating a mana ability during cost payment, which \
         is exactly what happens inside the window this row buys (CR 117.1d / CR 601.2g); got \
         {:?}",
        non_pass_actions(&funded, P2)
    );

    let mut unfunded = accept_arm.clone();
    let unfunded_spell = give_bolt_with_cost(&mut unfunded, P2, ManaCost::generic(1));
    assert!(
        !probe_actions(&unfunded, P2).iter().any(
            |a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == unfunded_spell)
        ),
        "FUNDING, negative half: the SAME interaction on the SAME board minus the Petal is NOT \
         payable. Together with the assertion above, the Petal's mana is what makes it reachable \
         — which is the whole content of 'otherwise-unaffordable'; got {:?}",
        non_pass_actions(&unfunded, P2)
    );

    // ── the ORDINARY-mana-source control: plain mana never leaks into stage 2 ──
    let mut ordinary = polled.clone();
    let sol_ring = give_sol_ring(&mut ordinary, P2);
    let (ordinary_probe, ordinary_flat) = engine::ai_support::shortcut_probe(&ordinary, P2);
    // REACH-GUARD — the assertion the two negatives below rest on. `stage_two_action_set`
    // filters `activatable_object_mana_actions`, and on a probe state parked at
    // `Priority { player }` that IS
    // `mana_sources::activatable_mana_actions_for_player(state, player)` — the same call,
    // routed through `mana_action_player`'s `Priority` arm. Asserting the Sol Ring is IN
    // that sweep is what makes "absent from the stage-2 set" attributable to the penalty
    // filter; without it, the two negatives below cannot tell `ManaSourcePenalty::None`
    // apart from "this object was never swept as a mana source at all".
    assert!(
        engine::game::mana_sources::activatable_mana_actions_for_player(ordinary_probe.state(), P2)
            .contains(&GameAction::ActivateAbility {
                source_id: sol_ring,
                ability_index: 0,
            }),
        "REACH-GUARD: the Sol Ring must reach the sweep stage 2 filters, or the negatives \
         below pass because nothing was ever there to re-admit — which is not the \
         `None`-vs-`Sacrifices` penalty distinction this control claims to measure"
    );
    assert!(
        !ordinary_flat.iter().any(
            |a| matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == sol_ring)
        ),
        "candidate generation excludes a mana ability outright (CR 605.1a); got {ordinary_flat:?}"
    );
    assert_eq!(
        engine::ai_support::stage_two_action_set(ordinary_probe.state(), &ordinary_flat),
        ordinary_flat,
        "NON-VACUITY: an ordinary mana source has penalty `None`, not `Sacrifices`, so the \
         stage-2 widening does NOT re-admit it. The Lotus Petal DOES enter this same set in this \
         same test, so this emptiness is a measurement and not a stuck instrument"
    );
    // NO verdict assertion here, deliberately. Sol Ring contributes 2 to
    // `feasible_mana_capacity`, which is enough to pay Angel of the Ruins'
    // hand-zone plainscycling already sitting in P2's hand on this fixture. That
    // activation enters the FLAT list — MEASURED, because the withheld verdict
    // rests on it and a stale fixture could silently make it false:
    let ordinary_non_pass = non_pass_actions(&ordinary, P2);
    assert!(
        ordinary_non_pass.iter().any(|a| {
            a.contains("Angel of the Ruins")
                && a.contains("#0 zone=Some(Hand)")
                && a.contains("controller=Some(PlayerId(2))")
        }),
        "the Sol Ring's 2 mana must unlock the Angel's {{2}} plainscycling — this is the fact \
         that makes withholding a verdict assertion correct rather than evasive. Ability index 0 \
         is MEASURED off this fixture, never assumed: the Angel (object 210) carries EXACTLY ONE \
         parsed ability and it is the plainscycling one — `ability_tag: Cycling`, \
         `activation_zone: Hand`, cost `Composite[Mana{{generic 2}}, Discard{{self_ref}}]`, \
         effect `SearchLibrary` filtered to `Subtype(Plains)`. A bare name match would also be \
         satisfied by some OTHER Angel activated ability, or by an Angel in a zone whose \
         activation this Sol Ring does not pay for, neither of which supports the withheld \
         verdict. If this reddens, first check whether the INDEX shifted (a newly parsed second \
         ability) before concluding the semantics changed; got {ordinary_non_pass:?}"
    );
    // It is `MayInterfere` on two independent legs (`AbilityCost::Discard` is not
    // allowlisted; its `sub_ability` moves a card to a HAND, which the anaphoric
    // fetch disjunct does not cover), so this seat answers Shorten — through
    // candidate affordability, a route this control does not model and claims
    // nothing about. Asserting Accept here would assert a false fact about the
    // board.
}

/// V10c — an UNTAPPED fetch is the `Effect::Mana` case with one extra step, so
/// the seat must keep its window; the TAPPED sibling still Accepts.
///
/// `v10a`/`v10b` closed mana production. This row closes the residual one arm
/// over: `effect_window_reach`'s `ChangeZone` arm allowlisted ANY
/// `Library -> Battlefield` move, and a land that arrives untapped (CR 110.5b —
/// permanents enter untapped "unless a spell or ability says otherwise") taps for
/// mana inside the window the Shorten hands back. CR 302.6's summoning-sickness
/// bar is a CREATURE rule and never reaches a land, and CR 601.2g runs the mana
/// ability during the cast it funds.
///
/// **The pair varies exactly one object** on the flagship board: a Crop Rotation
/// in P2's hand. Its four-legged AST is confined on every axis but the tap state
/// (MEASURED — see `CROP_ROTATION`), so the Shorten below is attributable to the
/// gate and to nothing else.
///
/// **Otherwise-unaffordable, both halves, on the production instrument.** The
/// negative half is asserted in BOTH arms at poll time: the `{1}` answer is not
/// castable, because `feasible_mana_capacity` is battlefield-scoped and P2's
/// battlefield is one Terramorphic Expanse (capacity 0). The positive half is the
/// QUARANTINED funding lemma: the Rotation is driven through the stack on the
/// `apply()` boundary, a real basic Swamp from P2's own recorded library enters
/// UNTAPPED, and the SAME answer is re-probed against the engine's own
/// castability gate.
///
/// **The tapped control is the load-bearing half of the pair.** The same drive
/// with Rampant Growth puts a land on the battlefield TAPPED, the answer stays
/// uncastable, and the seat still Accepts. That is what makes this gate a
/// distinction rather than a blanket flip — and it is the measurement that keeps
/// `v1`/`v1b`'s Terramorphic Accept correct rather than merely surviving.
///
/// MUTANT: in `effect_window_reach`'s `ChangeZone` arm, replace
/// `object_is_confined && entry_is_confined` with `object_is_confined` alone (the
/// pre-fix expression) — the SHORTEN arm flips to `Accept` (the shipped defect
/// this row exists to catch). The ACCEPT arm and
/// the tapped control are unaffected by that mutation by construction — neither
/// carries an untapped battlefield entry at all — so the row cannot pass by
/// trivializing in either direction.
///
/// Every `GameAction::CastSpell` matcher binds `{ object_id, .. }` and must NOT
/// name `payment_mode`, for `v10a`'s reason.
#[test]
fn v10c_an_untapped_fetch_that_funds_an_unaffordable_answer_keeps_its_window() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("CR 732.2a: the offer must fire on this real 4p drain");
    let polled = declare_and_poll(&board, P2);

    let mut base = polled.clone();
    // `{1}` generic, sized as in `v10b`: a generic residual is decided by
    // comparing summed battlefield capacity against it, so ONE untapped Swamp is
    // exactly enough and one tapped Swamp is exactly not — zero slack on both
    // halves, decided on one read path.
    let answer = give_bolt_with_cost(&mut base, P2, ManaCost::generic(1));

    // ── arm ACCEPT: the answer alone ──
    let accept_arm = base.clone();
    // ── arm SHORTEN: same board, same answer, PLUS the untapped fetch ──
    let mut shorten_arm = base.clone();
    let rotation = give_crop_rotation(&mut shorten_arm, P2);

    for (label, arm) in [("ACCEPT", &accept_arm), ("SHORTEN", &shorten_arm)] {
        assert!(
            !probe_actions(arm, P2).iter().any(
                |a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == answer)
            ),
            "PREMISE ({label} arm): the answer must be OTHERWISE-UNAFFORDABLE at poll time — P2's \
             battlefield is one Terramorphic Expanse, which contributes 0 to the \
             battlefield-scoped capacity scan, and the scan cannot see the fetch-then-tap two-step \
             the window buys; got {:?}",
            non_pass_actions(arm, P2)
        );
        assert!(
            stage_one_meaningful(arm, P2),
            "reach-guard ({label} arm): stage 1 must return true, or the seat answers at stage 1 \
             and the fold under test never runs"
        );
    }

    assert!(
        probe_actions(&shorten_arm, P2).iter().any(
            |a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == rotation)
        ),
        "reach-guard: the FUNDER must really be castable, or the SHORTEN arm is the ACCEPT arm \
         with extra steps; got {:?}",
        non_pass_actions(&shorten_arm, P2)
    );

    let shorten_non_pass = non_pass_actions(&shorten_arm, P2);
    assert_eq!(
        shorten_non_pass.len(),
        2,
        "ATTRIBUTION + THRESHOLD SENTINEL, mirroring `v10a`'s: the SHORTEN arm must be the ACCEPT \
         arm's single fetchland PLUS the Rotation cast, and nothing else. A fixture or capacity \
         change that added a third `MayInterfere` action would over-determine this row silently \
         instead of reddening; got {shorten_non_pass:?}"
    );

    let accept_non_pass = non_pass_actions(&accept_arm, P2);
    assert_eq!(
        accept_non_pass.len(),
        1,
        "ATTRIBUTION: the ACCEPT arm's action set must be the flagship's exactly, so its Accept is \
         the already-shipped verdict and the pair's ONLY variable is the Rotation; got \
         {accept_non_pass:?}"
    );
    assert!(
        accept_non_pass[0].contains("Terramorphic Expanse")
            && accept_non_pass[0].contains("zone=Some(Battlefield)")
            && accept_non_pass[0].contains("controller=Some(PlayerId(2))"),
        "ATTRIBUTION: that one action must be P2's OWN battlefield fetchland; got \
         {accept_non_pass:?}"
    );

    // MEMBERSHIP, not just cardinality — `v10a`'s partition, for its reasons.
    let (_rotation_leg, other_legs): (Vec<&String>, Vec<&String>) = shorten_non_pass
        .iter()
        .partition(|a| a.starts_with(&format!("CastSpell {{ object_id: {rotation:?},")));
    assert_eq!(
        other_legs,
        vec![&accept_non_pass[0]],
        "ATTRIBUTION: the SHORTEN arm's set MINUS the Rotation must be the ACCEPT arm's set \
         EXACTLY — same fetchland object, same zone, same controller; got {shorten_non_pass:?}"
    );

    assert_eq!(
        engine::ai_support::smart_shortcut_response(&accept_arm, P2),
        ShortcutResponse::Accept,
        "the pair's negative arm: an unaffordable answer and a confined TAPPED fetchland buy \
         nothing"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&shorten_arm, P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "the pair's positive arm: a fetch that puts a land onto the battlefield UNTAPPED \
         (CR 110.5b) hands the seat mana inside its own window, so accepting here surrenders a \
         live out. The Rotation is the one object that differs"
    );

    // ── FUNDING LEMMA (CR 110.5b + CR 117.1d + CR 601.2g), QUARANTINED ──
    //
    // Both verdicts above are already taken; resolving the fetch inside an arm
    // would change the very action set they were taken on, and the funded board
    // additionally unlocks the Angel's `{2}` cycling (`give_lotus_petal`'s
    // capacity note), which carries `MayInterfere` on a route this pair does not
    // model. Same quarantine as `v10a`/`v10b`.
    let (funded, fetched) = resolve_fetch_choosing_a_swamp(&shorten_arm, rotation);
    let land = funded
        .objects
        .get(&fetched)
        .expect("the fetched Swamp is a real object on this board");
    assert_eq!(
        land.zone,
        Zone::Battlefield,
        "reach-guard: the Rotation must have RESOLVED and MOVED the land, not merely been cast — \
         otherwise the funding assertion below would red for the wrong reason"
    );
    assert!(
        !land.tapped,
        "CR 110.5b: the fetched land enters UNTAPPED because this fetch says nothing otherwise. \
         This is the game-level fact the AST's `enter_tapped` stands for, measured on the object \
         rather than inferred from the printed text"
    );
    assert!(
        probe_actions(&funded, P2)
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == answer)),
        "FUNDING: with the fetched Swamp untapped, the engine's own castability gate says the SAME \
         `{{1}}` answer that is unaffordable in BOTH arms above IS payable. That is the two-step \
         the priority window buys (CR 117.1d / CR 601.2g), measured rather than inferred; got {:?}",
        non_pass_actions(&funded, P2)
    );

    // ── THE TAPPED CONTROL — the half that makes this a distinction ──
    //
    // Same board, same answer, same drive, same chosen Swamp: only the fetch's
    // printed tap rider differs. The land arrives tapped, funds nothing, and the
    // seat still Accepts — which is `v1`/`v1b`'s Terramorphic verdict, measured
    // here on the funding mechanism itself instead of assumed to survive.
    let mut tapped_arm = base.clone();
    let growth = give_rampant_growth(&mut tapped_arm, P2);
    assert_eq!(
        non_pass_actions(&tapped_arm, P2).len(),
        2,
        "reach-guard: the tapped fetch must be castable too, or its Accept below is produced by an \
         absent action rather than by a confined one; got {:?}",
        non_pass_actions(&tapped_arm, P2)
    );

    let (tapped_funded, tapped_fetched) = resolve_fetch_choosing_a_swamp(&tapped_arm, growth);
    let tapped_land = tapped_funded
        .objects
        .get(&tapped_fetched)
        .expect("the fetched Swamp is a real object on this board");
    assert_eq!(
        tapped_land.zone,
        Zone::Battlefield,
        "reach-guard: the tapped fetch must have resolved and moved the land as well"
    );
    assert!(
        tapped_land.tapped,
        "CR 110.5b: 'unless a spell or ability says otherwise' — this one says otherwise, and this \
         assertion is what separates the pair"
    );
    assert!(
        !probe_actions(&tapped_funded, P2)
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == answer)),
        "FUNDING, negative half: a TAPPED land pays for nothing, so the identical answer stays \
         uncastable after the identical drive. Together with the assertion above, the tap state — \
         not the fetch shape — is what funds the out; got {:?}",
        non_pass_actions(&tapped_funded, P2)
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&tapped_arm, P2),
        ShortcutResponse::Accept,
        "…and the classifier agrees with the board: a tapped fetch buys the seat nothing, so the \
         gate is a DISTINCTION on the tap axis and not a blanket flip of the fetch class. This is \
         the assertion an over-broad version of this fix destroys first"
    );
}

// ===========================================================================
// PR #7101 — the two maintainer findings, both on the REAL 4p board.
//
// SHARED PREMISE, MEASURED once here rather than re-asserted per row. At the
// offer beat the flagship board carries 171 active trigger definitions; the
// CR 113.6 zone-of-function gate removes 168, leaving exactly three:
//
//   Dina, Soul Steeper       LifeGained  valid_card=None      owner=P0
//   Bloodthirsty Conqueror   LifeLost    valid_card=None      owner=P0
//   Abundant Growth          ChangesZone valid_card=SelfRef   owner=P1
//
// Dina and the Conqueror are relieved by the MODE gate (CR 119.3: no confined
// action adjusts a life total). Abundant Growth is relieved for P2 by the
// SELF-REFERENCE carve-out and by nothing else — `obj.owner (P1) != actor (P2)`
// is the whole of it. That is why `v1_live_path_*` still Accepts, and it is why
// polling P1 on this same board would find a live observer: for P1 the carve-out
// conjunct is false. P1 is nevertheless safe, and NOT because "no board fires" —
// it is because `smart_shortcut_response` returns `Accept` from STAGE 1, before
// `any_action_may_interfere` ever runs, which `v1_live_path_*`'s own sibling
// control pins via `!stage_one_meaningful(&other, seat)`.
// ===========================================================================

const HEDRON_CRAB: &str = "Landfall — Whenever a land you control enters, target player mills \
                           three cards. (They put the top three cards of their library into their \
                           graveyard.)";
const BLOODGHAST_LANDFALL: &str = "Landfall — Whenever a land you control enters, you may return \
                                   this card from your graveyard to the battlefield.";

/// Stage a real parsed OBSERVER — a card whose rules content is a printed
/// TRIGGER rather than an activated/spell ability — owned and controlled by
/// `player`.
///
/// Distinct from `give_parsed_card` above on purpose: that helper stages the
/// ACTING object and asserts exactly one parsed *ability*, which is the premise
/// the V10 rows need and the exact opposite of what an observer carries. Verbatim
/// Oracle text under the card's REAL name — a paraphrase can take a different
/// parser branch and green a row while the printed card stays broken.
fn give_parsed_observer(
    state: &mut GameState,
    player: PlayerId,
    zone: Zone,
    name: &str,
    oracle: &str,
) -> ObjectId {
    let parsed = engine::parser::oracle::parse_oracle_text(oracle, name, &[], &[], &[]);
    assert_eq!(
        parsed.triggers.len(),
        1,
        "PREMISE: {name} must parse to exactly one printed trigger, or the row below cannot \
         attribute its verdict to that trigger; got {:?}",
        parsed.triggers
    );
    let id = engine::game::zones::create_object(
        state,
        CardId(state.next_object_id),
        player,
        name.to_string(),
        zone,
    );
    let obj = state.objects.get_mut(&id).expect("just created");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.install_trigger_base_definitions(std::sync::Arc::new(parsed.triggers))
        .expect("staging one printed trigger set on a fresh object");
    state.layers_dirty = LayersDirty::full();
    id
}

/// Every active trigger definition on `object`, rendered for assertions.
fn trigger_shapes(state: &GameState, object: ObjectId) -> Vec<String> {
    let obj = state.objects.get(&object).expect("staged object exists");
    engine::game::functioning_abilities::active_trigger_definitions(state, obj)
        .map(|a| {
            format!(
                "mode={:?} valid_card={:?} trigger_zones={:?} zcc={}",
                a.definition.mode,
                a.definition.valid_card,
                a.definition.trigger_zones,
                a.definition.zone_change_clauses.len()
            )
        })
        .collect()
}

/// **T1.2 — the [MED] finding.** `SelfRef` / `Controller` prove CONTROL, never
/// OWNERSHIP.
///
/// CR 110.2 makes owner and controller independent. CR 701.21a: "To sacrifice a
/// permanent, its controller moves it from the battlefield directly to its
/// OWNER'S graveyard." So when P2 cracks a Terramorphic Expanse it controls but
/// does NOT own, the land lands in somebody else's graveyard — a per-player zone
/// (CR 400.1) — and the action was never confined at all. The fold's
/// `filter_is_actor_owned(sacrifice.target)` says `true` regardless, because
/// `TargetFilter::SelfRef` resolves through control (CR 109.5).
///
/// ONE VARIABLE against `v1_live_path_fetchland_seat_accepts_on_the_real_4p_board`:
/// the same board, the same beat, the same offer, the same single action — only
/// `ObjectId(203).owner` differs. MEASURED: all four seats own everything they
/// control on this dump naturally, so the violating shape has to be constructed;
/// it is constructed on the flagship's OWN fetchland rather than on a spare
/// object so that the mis-owned permanent IS the one being sacrificed.
///
/// REVERT-PROBE (executed): trivialize `actor_owns_everything_they_control` to
/// `true` ⇒ this row returns `Accept` and fails.
#[test]
fn t1_2_a_controlled_but_unowned_fetchland_is_not_confined() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("the offer must fire");
    let mut polled = declare_and_poll(&board, P2);

    // REACH-GUARD, before the mutation: an `Accept` produced by an empty seat
    // would be indistinguishable from an `Accept` produced by the predicate.
    let non_pass = non_pass_actions(&polled, P2);
    assert!(
        !non_pass.is_empty(),
        "REACH-GUARD: P2 must hold a non-pass action, or stage 1 answers and stage 2 never runs"
    );
    assert!(
        non_pass
            .iter()
            .any(|a| a.contains("Terramorphic Expanse") && a.contains("zone=Some(Battlefield)")),
        "REACH-GUARD: the action under test must be the fetchland activation the finding names; \
         got {non_pass:?}"
    );
    assert!(
        stage_one_meaningful(&polled, P2),
        "REACH-GUARD: stage 1 must still pass P2 through to stage 2"
    );

    let fetch = *polled
        .objects
        .values()
        .find(|o| o.name == "Terramorphic Expanse" && o.controller == P2)
        .map(|o| &o.id)
        .expect("P2's fetchland is on this board");

    // CONTROL half — untouched board, unchanged answer.
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&polled, P2),
        ShortcutResponse::Accept,
        "matched control (T1.3): with ownership intact the seat still Accepts, so the flip below \
         is attributable to the owner field and to nothing else about this board"
    );

    // WITNESS half — one field.
    polled.objects.get_mut(&fetch).expect("just read").owner = P1;
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&polled, P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "CR 701.21a: sacrificing a permanent P2 controls but P1 OWNS puts a card into P1's \
         graveyard. `filter_is_actor_owned` cannot see that — it proves control (CR 109.5) — so \
         the ownership fact has to be proven at board level or this Accept is unsound"
    );
}

/// **T2.1 — the [HIGH] finding.** A confined action is still OBSERVED.
///
/// CR 603.2: "Whenever a game event or game state matches a triggered ability's
/// trigger event, that ability automatically triggers." Reading the ACTING
/// object's AST proves what that object does; it proves nothing about what the
/// rest of the board is watching for. Hedron Crab watches for exactly the event
/// the flagship's confined fetch produces, and its effect targets a PLAYER.
///
/// The crab is OPPONENT-owned on purpose. Actor-owned, both the shipped
/// carve-out and the `filter_is_actor_owned` shape rejected in T2.3 would relieve
/// it (the `obj.owner != actor` conjunct rescues it), so an actor-owned crab
/// cannot tell the two apart. Only the opponent-owned one discriminates.
///
/// REVERT-PROBE (executed): delete the `board_observer_may_react` conjunct from
/// `any_action_may_interfere` ⇒ this row returns `Accept` and fails.
#[test]
fn t2_1_an_opponent_owned_landfall_observer_defeats_a_confined_fetch() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("the offer must fire");

    // MATCHED CONTROL (T2.2) — same board, same beat, no crab.
    let clean = declare_and_poll(&board, P2);
    assert!(
        stage_one_meaningful(&clean, P2),
        "REACH-GUARD: stage 1 passes P2 through, so both halves below measure stage 2"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&clean, P2),
        ShortcutResponse::Accept,
        "T2.2 matched control: without an observer the confined fetch still Accepts"
    );

    // WITNESS — one object added, owned and controlled by an opponent.
    let mut with_crab = board.clone();
    let crab = give_parsed_observer(
        &mut with_crab,
        P1,
        Zone::Battlefield,
        "Hedron Crab",
        HEDRON_CRAB,
    );

    // PARSE PIN: if the parser ever stops producing this shape the row must go
    // RED rather than silently green on a different (or absent) trigger.
    let shapes = trigger_shapes(&with_crab, crab);
    assert_eq!(
        shapes.len(),
        1,
        "PREMISE: the crab must carry exactly one active trigger; got {shapes:?}"
    );
    assert!(
        shapes[0].contains("mode=ChangesZone"),
        "PREMISE: landfall is a CR 603.6a zone-change trigger; got {shapes:?}"
    );
    assert!(
        shapes[0].contains("Land") && shapes[0].contains("You"),
        "PREMISE: `valid_card` must be the TYPED land-you-control filter, NOT `SelfRef`. This is \
         the exact shape T2.3 proves `filter_is_actor_owned` accepts — reusing that helper as the \
         carve-out would relieve this trigger and the [HIGH] finding would survive; got {shapes:?}"
    );
    assert!(
        shapes[0].contains("trigger_zones=[Battlefield]"),
        "PREMISE: the crab must FUNCTION where it is standing, or the zone gate — not the \
         carve-out — would be what produces the verdict below; got {shapes:?}"
    );

    let polled = declare_and_poll(&with_crab, P2);
    assert!(
        stage_one_meaningful(&polled, P2),
        "REACH-GUARD: the crab must not have changed which stage answers"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&polled, P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "CR 603.2: P2's 'confined' fetch makes a land enter, the opponent's Crab triggers on it, \
         and its effect mills a TARGET PLAYER. The action reached outside P2's own resources \
         without P2's own AST containing anything that says so"
    );
}

/// **T2.4 — the zone-of-function gate, on one axis.** Same card, same trigger,
/// two zones.
///
/// CR 113.6 / CR 603.6: a Bloodghast-shaped landfall trigger functions from the
/// GRAVEYARD (that is where the ability returns the card from), so a copy sitting
/// in HAND observes nothing. The pair moves the zone and nothing else, so the
/// `Accept` cannot be a property of the card and the `Shorten` cannot be a
/// property of the board.
///
/// REVERT-PROBE (executed): delete the `trigger_definition_functions_in_zone`
/// conjunct from `board_observer_may_react` ⇒ the hand half returns `Shorten`
/// and this row fails.
///
/// SCOPE, measured rather than assumed. The OTHER way this gate can go wrong —
/// reading `def.trigger_zones.contains(&obj.zone)` directly, which answers
/// "functions nowhere" for the empty list that means battlefield-only — is NOT
/// discriminated by this row, and it cannot be discriminated on this board:
/// MEASURED, every active trigger definition on the flagship carries an explicit
/// `trigger_zones: [Battlefield]`, so the direct read and the authority agree on
/// the whole corpus. That fail-open direction is a LATENT hole here, closed for
/// the same reason as this module's `Zone::Stack` arm, and it is pinned by
/// `t2_4b_an_empty_trigger_zones_list_means_battlefield_not_nowhere` at unit
/// level where the shape can actually be built.
#[test]
fn t2_4_a_landfall_observer_in_hand_does_not_veto_but_in_its_own_zone_it_does() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("the offer must fire");

    for (zone, expected) in [
        (Zone::Hand, ShortcutResponse::Accept),
        (
            Zone::Graveyard,
            ShortcutResponse::Shorten { at_iteration: 0 },
        ),
    ] {
        let mut staged = board.clone();
        let ghast = give_parsed_observer(&mut staged, P1, zone, "Bloodghast", BLOODGHAST_LANDFALL);
        let shapes = trigger_shapes(&staged, ghast);
        assert!(
            shapes.iter().any(|s| s.contains("mode=ChangesZone")),
            "PREMISE at {zone:?}: the landfall trigger must survive parsing; got {shapes:?}"
        );

        let polled = declare_and_poll(&staged, P2);
        assert!(
            stage_one_meaningful(&polled, P2),
            "REACH-GUARD at {zone:?}: stage 2 must be the stage that answers"
        );
        assert_eq!(
            engine::ai_support::smart_shortcut_response(&polled, P2),
            expected,
            "CR 113.6: the SAME trigger on the SAME card must veto from the zone it functions in \
             and not from the zone it does not; failed at {zone:?}"
        );
    }
}

/// **T2.8 — the scan is not battlefield-only.** A command-zone emblem is scanned.
///
/// CR 114.1 puts emblems in the command zone and CR 114.4 is exact — "Abilities
/// of emblems function in the command zone" — so an observer that never touches
/// the battlefield
/// still observes. `active_trigger_definitions` applies the CR 114.4 emblem gate
/// itself, which is why the scan delegates to it rather than filtering zones.
///
/// REVERT-PROBE (executed): restrict `board_observer_may_react` to
/// `obj.zone == Zone::Battlefield` ⇒ this row returns `Accept` and fails.
#[test]
fn t2_8_a_command_zone_emblem_observer_is_scanned() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("the offer must fire");

    let emblem = give_parsed_observer(&mut board, P1, Zone::Command, "Hedron Crab", HEDRON_CRAB);
    {
        let obj = board.objects.get_mut(&emblem).expect("just staged");
        obj.is_emblem = true;
        // CR 114.1: an emblem has no characteristics other than its abilities.
        obj.card_types.core_types.clear();
        obj.base_card_types = obj.card_types.clone();
        // CR 114.4: "Abilities of emblems function in the command zone." The
        // printed crab trigger parses as battlefield-only, so the zone list is
        // retargeted — that retarget is the whole staging, and the assertion
        // below proves it survived into the live set.
        let mut defs = (*obj.base_trigger_definitions).clone();
        for d in &mut defs {
            d.trigger_zones = vec![Zone::Command];
        }
        obj.install_trigger_base_definitions(std::sync::Arc::new(defs))
            .expect("re-staging the emblem's trigger set");
    }
    let shapes = trigger_shapes(&board, emblem);
    assert_eq!(
        shapes.len(),
        1,
        "PREMISE: the emblem must expose exactly one active trigger from the command zone — \
         `active_trigger_definitions` drops non-emblem command-zone triggers, so an empty list \
         here would make the row vacuous; got {shapes:?}"
    );

    let polled = declare_and_poll(&board, P2);
    assert!(
        stage_one_meaningful(&polled, P2),
        "REACH-GUARD: stage 2 must be the stage that answers"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&polled, P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "CR 114.1 + CR 603.2: an observer in the command zone observes. A battlefield-only scan \
         would miss every emblem and every command-zone trigger"
    );
}

/// **T2.9 — the actor's OWN self-referential observer is NOT relieved**, on the
/// real board's own survivor.
///
/// Abundant Growth (`ChangesZone`, `valid_card: SelfRef`) is one of the three
/// definitions that survive the zone gate on the flagship board, and for P2 it is
/// relieved by the carve-out's `obj.owner != actor` conjunct alone. Retag it to
/// P2 — owner AND controller together, so the T1.2 ownership conjunct stays
/// satisfied and cannot be what moves the verdict — and the same trigger must
/// stop being relieved.
///
/// One variable: the object's seat. This is the row that pins the carve-out to
/// "somebody ELSE'S self-reference", which is the only version of it that is
/// sound: an actor's own self-referential trigger is reachable by the actor's own
/// action.
///
/// REVERT-PROBE (executed): drop the `obj.owner != actor` conjunct ⇒ this row
/// returns `Accept` and fails.
#[test]
fn t2_9_the_actors_own_self_referential_observer_keeps_the_veto() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("the offer must fire");
    let mut polled = declare_and_poll(&board, P2);

    let growth = *polled
        .objects
        .values()
        .find(|o| o.name == "Abundant Growth")
        .map(|o| &o.id)
        .expect("MEASURED: Abundant Growth is on this board and survives the zone gate");
    let shapes = trigger_shapes(&polled, growth);
    assert!(
        shapes
            .iter()
            .any(|s| s.contains("valid_card=Some(SelfRef)")),
        "PREMISE: this row needs the SelfRef shape the carve-out keys on; got {shapes:?}"
    );
    assert_ne!(
        polled.objects[&growth].owner, P2,
        "PREMISE: it starts on another seat, which is why the flagship Accepts"
    );

    // CONTROL — somebody else's self-reference, relieved.
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&polled, P2),
        ShortcutResponse::Accept,
        "control: an opponent's SelfRef observer cannot be reached by an action confined to P2's \
         own resources"
    );

    // WITNESS — the same trigger, now the actor's own.
    {
        let obj = polled.objects.get_mut(&growth).expect("just read");
        obj.owner = P2;
        obj.base_controller = Some(P2);
        obj.controller = P2;
    }
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&polled, P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "CR 109.5: 'you' on the observer means ITS controller. Once that is the actor, the actor's \
         own confined action can reach it, so the carve-out must not apply"
    );
}

/// **T3.5 — the new `GameObject::parse_warnings` field changes no committed byte.**
///
/// `skip_serializing_if = "Vec::is_empty"` is the whole mechanism, and this is
/// the assertion that it is actually wired: re-serialize every object of a
/// committed fixture through the production decoder and compare against the
/// bytes the fixture shipped with.
///
/// REVERT-PROBE (executed): drop `skip_serializing_if` from the field ⇒ every
/// object gains `"parse_warnings":[]` and this row fails.
#[test]
fn t3_5_the_new_parse_warnings_field_keeps_dumps_byte_identical() {
    let json = gunzip_dump(include_bytes!("../fixtures/dina_noff_turn5_4p.json.gz"));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let before = envelope["gameState"]["objects"].clone();
    assert!(
        before.as_object().is_some_and(|m| !m.is_empty()),
        "PREMISE: the fixture must carry objects, or byte-equality below is vacuous"
    );
    assert!(
        !before.to_string().contains("parse_warnings"),
        "PREMISE: the committed OBJECTS predate the field, so any occurrence below is new. \
         (Scoped to `objects` on purpose: the envelope's card-database subtree carries the \
         face-level `parse_warnings` this field is copied FROM, and has since before this change.)"
    );

    let state = restore_dump(&json);
    let after =
        serde_json::to_value(state.objects.values().collect::<Vec<_>>()).expect("objects reencode");
    assert!(
        !after.to_string().contains("parse_warnings"),
        "an EMPTY diagnostics list must not serialize. Without `skip_serializing_if` every object \
         in every committed dump gains a `\"parse_warnings\":[]` key and every stored game grows"
    );
}

/// Helping Hand, verbatim (Oracle text verified on Scryfall,
/// `api.scryfall.com/cards/named?exact=Helping+Hand`). Confined on every axis
/// `effect_window_reach`'s `ChangeZone` arm reads EXCEPT the origin: the card it
/// returns is a real object in the actor's graveyard whose rules content the fold
/// never classified.
const HELPING_HAND: &str = "Return target creature card with mana value 3 or less from your \
                            graveyard to the battlefield tapped.";

/// Stage a vanilla creature card in `player`'s graveyard. VANILLA on purpose:
/// with no abilities of its own it cannot itself move any verdict — not through
/// `board_observer_may_react` (nothing to observe with), not through the fold
/// (it is not the subject of any action). It exists only so the Helping Hand
/// below has a legal target, and it is staged in BOTH arms so it is held
/// constant rather than varied.
fn give_graveyard_creature(state: &mut GameState, player: PlayerId) -> ObjectId {
    let id = engine::game::zones::create_object(
        state,
        CardId(state.next_object_id),
        player,
        "Grizzly Bears".to_string(),
        Zone::Graveyard,
    );
    let obj = state.objects.get_mut(&id).expect("just created");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    state.layers_dirty = LayersDirty::full();
    id
}

/// V10d — a TAPPED battlefield entry from a GRAVEYARD keeps the seat's window;
/// the tapped LIBRARY sibling still Accepts.
///
/// `v10c` closed the tap axis on a library fetch. This row closes the ORIGIN
/// axis: `effect_window_reach`'s `ChangeZone` arm allowlisted a tapped
/// battlefield entry from ANY origin, so a graveyard recursion read as confined
/// while the card it returns IS in `state.objects` with readable
/// trigger/replacement/static definitions that nothing reads.
/// `object_window_reach`'s `carries_unreadable_rules_content` gate runs on the
/// SOURCE (the Helping Hand), never on the card it returns, and
/// `board_observer_may_react` correctly answers "does not function" for a
/// graveyard ETB (CR 113.6). So a Fleshbag-Marauder-class creature in the actor's
/// own graveyard — "When this creature enters, each player sacrifices a creature
/// of their choice" — arrives inside the very window the seat declined to keep.
///
/// The LIBRARY sibling below is not confined because its origin excuses it from
/// being read — v10e is the row that shows a library fetch reading `Shorten` on
/// this same board — but because `library_arrivals_are_inert` reads the cards
/// Rampant Growth could actually select and finds them vanilla.
///
/// **The pair varies exactly one object** on the flagship board, and both halves
/// of that object are the SAME SHAPE on every axis but origin: two spells staged
/// identically (`give_parsed_card`, instant, no printed cost), each heading a
/// `ChangeZone` to the battlefield with `enter_tapped: Tapped`, no
/// `enters_attacking`, no `enters_modified_if`, and a You-controlled target. The
/// graveyard creature is present in BOTH arms. So the Shorten is attributable to
/// the origin field and to nothing else.
///
/// **The control is the load-bearing half.** Rampant Growth's tapped LIBRARY
/// fetch must still Accept — that is what keeps this gate a distinction rather
/// than a blanket flip, and it is the same measurement that keeps `v1`/`v1b`'s
/// Terramorphic Accept correct.
///
/// **Conservative by construction, and the cost is stated.** Grizzly Bears is
/// vanilla, so this seat surrenders its window over a recursion that could not
/// have interfered. That is the gate's shape: it reads the ORIGIN, not the
/// returned card, so it is uniform over what the graveyard happens to hold. Per
/// the module's §2 that is the direction that costs efficacy rather than games,
/// and it is the same trade the `Hand` destination gate already made. MEASURED on
/// `data/card-data.json`, of the 493 document-wide tapped/non-attacking/
/// unconditional/You-controlled battlefield entries, 247 come from a library and
/// keep their classification; 74 graveyard, 25 hand, 6 exile and 140
/// absent-origin nodes lose it.
///
/// MUTANT: in `effect_window_reach`'s `ChangeZone` arm, delete the
/// `*origin == Some(Zone::Library)` conjunct from `entry_is_confined` (the
/// pre-fix expression) — the SHORTEN arm flips to `Accept`. The ACCEPT arm is
/// unaffected by that mutation by construction: its origin already IS
/// `Library`, so the conjunct it deletes was true there anyway.
///
/// Every `GameAction::CastSpell` matcher binds `{ object_id, .. }` and must NOT
/// name `payment_mode`, for `v10a`'s reason.
#[test]
fn v10d_a_tapped_graveyard_return_keeps_its_window_and_the_library_sibling_accepts() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("CR 732.2a: the offer must fire on this real 4p drain");
    let polled = declare_and_poll(&board, P2);

    let mut base = polled.clone();
    give_graveyard_creature(&mut base, P2);

    // ── arm ACCEPT: the tapped LIBRARY fetch ──
    let mut accept_arm = base.clone();
    let growth = give_rampant_growth(&mut accept_arm, P2);
    // ── arm SHORTEN: same board, same graveyard, the tapped GRAVEYARD return ──
    let mut shorten_arm = base.clone();
    let helping_hand = give_parsed_card(
        &mut shorten_arm,
        P2,
        "Helping Hand",
        HELPING_HAND,
        CoreType::Instant,
        Zone::Hand,
    );

    for (label, arm, staged) in [
        ("ACCEPT", &accept_arm, growth),
        ("SHORTEN", &shorten_arm, helping_hand),
    ] {
        assert!(
            stage_one_meaningful(arm, P2),
            "reach-guard ({label} arm): stage 1 must return true, or the seat answers at stage 1 \
             and the fold under test never runs"
        );
        assert!(
            probe_actions(arm, P2).iter().any(
                |a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == staged)
            ),
            "reach-guard ({label} arm): the staged spell must really be castable at this window, \
             or the arm is the base board with extra steps and the pair measures nothing; got {:?}",
            non_pass_actions(arm, P2)
        );
    }

    // ATTRIBUTION: both arms must be the flagship's action set PLUS exactly the
    // one staged cast. A third `MayInterfere` action in either arm would
    // over-determine the row silently instead of reddening.
    let accept_non_pass = non_pass_actions(&accept_arm, P2);
    let shorten_non_pass = non_pass_actions(&shorten_arm, P2);
    assert_eq!(
        accept_non_pass.len(),
        2,
        "ATTRIBUTION: the ACCEPT arm must be the flagship fetchland PLUS the Rampant Growth cast, \
         and nothing else; got {accept_non_pass:?}"
    );
    assert_eq!(
        shorten_non_pass.len(),
        2,
        "ATTRIBUTION: the SHORTEN arm must be the flagship fetchland PLUS the Helping Hand cast, \
         and nothing else; got {shorten_non_pass:?}"
    );

    // MEMBERSHIP, not just cardinality: the two arms MINUS their staged cast must
    // be the same single flagship action, so the pair really does vary one object.
    let accept_rest: Vec<&String> = accept_non_pass
        .iter()
        .filter(|a| !a.starts_with(&format!("CastSpell {{ object_id: {growth:?},")))
        .collect();
    let shorten_rest: Vec<&String> = shorten_non_pass
        .iter()
        .filter(|a| !a.starts_with(&format!("CastSpell {{ object_id: {helping_hand:?},")))
        .collect();
    assert_eq!(
        accept_rest, shorten_rest,
        "ATTRIBUTION: each arm's set MINUS its staged cast must be the SAME flagship action — same \
         object, same zone, same controller; got {accept_non_pass:?} vs {shorten_non_pass:?}"
    );
    assert!(
        accept_rest.len() == 1
            && accept_rest[0].contains("Terramorphic Expanse")
            && accept_rest[0].contains("zone=Some(Battlefield)")
            && accept_rest[0].contains("controller=Some(PlayerId(2))"),
        "ATTRIBUTION: that shared action must be P2's OWN battlefield fetchland; got \
         {accept_rest:?}"
    );

    assert_eq!(
        engine::ai_support::smart_shortcut_response(&accept_arm, P2),
        ShortcutResponse::Accept,
        "the pair's negative arm: a tapped LIBRARY fetch stays confined — Rampant Growth searches \
         for a BASIC land and every basic in P2's recorded library is vanilla, so \
         `library_arrivals_are_inert` really did read the selectable set rather than skip it \
         (v10e is the row that varies that set)"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&shorten_arm, P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "the pair's positive arm: the returned card is a REAL object in the actor's graveyard \
         whose rules content this fold never classified, so `OwnResourcesOnly` would be a proof it \
         cannot discharge. The Helping Hand is the one object that differs"
    );
}

/// V10e — a tapped LIBRARY fetch whose search can SELECT an interfering
/// permanent is not confined, and the two things that could be producing that
/// verdict are separated by two orthogonal one-variable controls.
///
/// The hole this closes was argued, not overlooked. Every earlier revision of
/// `effect_window_reach`'s `ChangeZone` arm discharged the arriving card with a
/// hidden-zone argument: CR 400.2 makes a library hidden, the card is an unchosen
/// member of it, so "the seam is handed a `GameState` and a list of
/// `GameAction`s, and the card is the subject of neither". CR 701.23a is the rule
/// that refutes it — "To search for a card in a zone, look at all cards in that
/// zone (even if it's a hidden zone) and find a card that matches the given
/// description" — and MEASURED on THIS fixture at THIS poll, all 91 cards of P2's
/// library resolve in `state.objects` with full parsed rules content. Hidden
/// means the opponents cannot see it. The searcher picks whichever member they
/// like.
///
/// **`board_observer_may_react` cannot cover this**, and correctly so: CR 113.6,
/// through `trigger_definition_functions_in_zone`, answers "does not function"
/// for a card in a library. That scan asks which triggers function NOW; the
/// hazard is a trigger that functions once the search puts the card onto the
/// battlefield.
///
/// **The witness is on the recorded board, not staged.** P2's own library holds
/// **Bojuka Bog** — "This land enters tapped. / When this land enters, exile
/// target player's graveyard. / {T}: Add {B}." (Oracle text verified on Scryfall)
/// — a LAND whose ETB exiles a graveyard belonging to somebody else. A seat that
/// Accepts while holding an unrestricted land fetch has declined a window in
/// which it could have made a live cross-player choice.
///
/// **Three arms, two orthogonal controls, one variable each.**
/// * `SHORTEN` — Reshape the Earth (`Typed[Land]`) on the recorded library.
/// * `ACCEPT_BY_FILTER` — Rampant Growth (`Typed[Land] + Basic`) on the SAME
///   library. Holds the board fixed and varies the printed search filter, so the
///   gate is shown to read the filter and not merely "some land fetch exists".
/// * `ACCEPT_BY_LIBRARY` — Reshape the Earth again, with the non-inert LANDS
///   removed from P2's library. Holds the card fixed and varies the deck, which
///   is the axis the gate actually claims to read. This is the arm that
///   distinguishes the implemented gate from a filter-text heuristic: a
///   `Basic`-means-safe shortcut passes `ACCEPT_BY_FILTER` and fails here.
///
/// MUTANT (executed): delete the `library_arrivals_are_inert` conjunct from
/// `object_window_reach` ⇒ the `SHORTEN` arm flips to `Accept`. The two ACCEPT
/// arms are unaffected by that mutation by construction — their selectable sets
/// are already inert, so the deleted conjunct was true in both.
#[test]
fn v10e_an_unrestricted_land_fetch_that_can_select_bojuka_bog_keeps_its_window() {
    let mut board = live_path_board();
    drive_to_offer(&mut board, 400).expect("CR 732.2a: the offer must fire on this real 4p drain");
    let polled = declare_and_poll(&board, P2);

    // ── PREMISE, read off the recorded library rather than assumed ──
    let library: Vec<ObjectId> = polled
        .players
        .iter()
        .find(|p| p.id == P2)
        .expect("P2 is seated on this dump")
        .library
        .iter()
        .copied()
        .collect();
    assert!(
        library.len() > 20,
        "PREMISE: P2 must have a real recorded library for the selection set to mean anything; \
         got {}",
        library.len()
    );
    let resolved = library
        .iter()
        .filter(|id| polled.objects.contains_key(id))
        .count();
    assert_eq!(
        resolved,
        library.len(),
        "PREMISE, and the refutation of the hidden-zone argument this row exists to retire: every \
         library card must resolve in `state.objects` at the decision point. If this ever fails, \
         the gate has nothing to read and the row measures nothing"
    );

    let named = |state: &GameState, id: &ObjectId| {
        state
            .objects
            .get(id)
            .map(|o| o.name.clone())
            .unwrap_or_default()
    };
    // A land the gate must reject, and the basics it must accept — both real
    // members of this deck.
    let bog = library
        .iter()
        .copied()
        .find(|id| named(&polled, id) == "Bojuka Bog")
        .expect("PREMISE: the recorded library must contain Bojuka Bog — it IS the hazard");
    let bog_obj = polled.objects.get(&bog).expect("just found");
    assert!(
        !bog_obj.trigger_definitions.is_empty(),
        "PREMISE: Bojuka Bog must carry its ETB trigger, or it is not the card this row names"
    );
    assert!(
        bog_obj.card_types.core_types.contains(&CoreType::Land),
        "PREMISE: it must be a LAND, or an unrestricted land search cannot select it"
    );
    assert!(
        !bog_obj.card_types.supertypes.contains(&Supertype::Basic),
        "PREMISE: it must NOT be basic, or the ACCEPT_BY_FILTER control below cannot be a control"
    );
    let basics: Vec<ObjectId> = library
        .iter()
        .copied()
        .filter(|id| {
            polled.objects[id]
                .card_types
                .supertypes
                .contains(&Supertype::Basic)
        })
        .collect();
    assert!(
        !basics.is_empty(),
        "PREMISE: ACCEPT_BY_FILTER needs a NON-EMPTY basic-land match set — an empty match set is \
         the one case `library_arrivals_are_inert` deliberately fails closed on, so it would \
         produce the wrong verdict for the wrong reason"
    );
    for id in &basics {
        let o = &polled.objects[id];
        assert!(
            o.trigger_definitions.is_empty()
                && o.replacement_definitions.is_empty()
                && o.static_definitions.is_empty()
                && o.keywords.is_empty(),
            "PREMISE: every basic in this deck must be vanilla, or ACCEPT_BY_FILTER would be \
             asserting the wrong direction; {} is not",
            o.name
        );
    }

    // ── the three arms ──
    let mut shorten_arm = polled.clone();
    let reshape_shorten = give_parsed_card(
        &mut shorten_arm,
        P2,
        "Reshape the Earth",
        RESHAPE_THE_EARTH,
        CoreType::Instant,
        Zone::Hand,
    );

    let mut accept_by_filter = polled.clone();
    let growth = give_rampant_growth(&mut accept_by_filter, P2);

    // Vary the DECK, not the card: strip every library land the gate would
    // reject, leaving a pool whose land members are all vanilla.
    let mut accept_by_library = polled.clone();
    let stripped = strip_non_inert_library_lands(&mut accept_by_library, P2);
    assert!(
        stripped.contains(&bog),
        "the ACCEPT_BY_LIBRARY arm must actually remove the hazard, or it is the SHORTEN arm with \
         extra steps; removed {stripped:?}"
    );
    let reshape_accept = give_parsed_card(
        &mut accept_by_library,
        P2,
        "Reshape the Earth",
        RESHAPE_THE_EARTH,
        CoreType::Instant,
        Zone::Hand,
    );

    // ── reach-guards: each arm's staged spell must really be castable here, and
    //    each arm must be the flagship action set PLUS exactly that one cast ──
    for (label, arm, staged) in [
        ("SHORTEN", &shorten_arm, reshape_shorten),
        ("ACCEPT_BY_FILTER", &accept_by_filter, growth),
        ("ACCEPT_BY_LIBRARY", &accept_by_library, reshape_accept),
    ] {
        assert!(
            stage_one_meaningful(arm, P2),
            "reach-guard ({label}): stage 1 must return true, or the seat answers at stage 1 and \
             the fold under test never runs"
        );
        let non_pass = non_pass_actions(arm, P2);
        assert_eq!(
            non_pass.len(),
            2,
            "ATTRIBUTION ({label}): the arm must be the flagship fetchland PLUS the one staged \
             cast, and nothing else; got {non_pass:?}"
        );
        assert!(
            non_pass
                .iter()
                .any(|a| a.starts_with(&format!("CastSpell {{ object_id: {staged:?},"))),
            "reach-guard ({label}): the staged spell must be castable at this window; got \
             {non_pass:?}"
        );
        assert!(
            non_pass.iter().any(|a| a.contains("Terramorphic Expanse")
                && a.contains("zone=Some(Battlefield)")
                && a.contains("controller=Some(PlayerId(2))")),
            "ATTRIBUTION ({label}): the other action must be P2's OWN battlefield fetchland, so \
             all three arms share it; got {non_pass:?}"
        );
    }

    assert_eq!(
        engine::ai_support::smart_shortcut_response(&shorten_arm, P2),
        ShortcutResponse::Shorten { at_iteration: 0 },
        "CR 701.23a: an unrestricted land search LOOKS AT ALL CARDS in the library, and this one \
         contains Bojuka Bog, whose ETB exiles a graveyard P2 does not own. Accepting here \
         declines a window holding a live cross-player choice"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&accept_by_filter, P2),
        ShortcutResponse::Accept,
        "CONTROL 1 (filter varies, deck held): a basic-land search on this same library can select \
         only vanilla Plains and Swamps, so the gate is a distinction and not a blanket flip"
    );
    assert_eq!(
        engine::ai_support::smart_shortcut_response(&accept_by_library, P2),
        ShortcutResponse::Accept,
        "CONTROL 2 (deck varies, card held): the SAME unrestricted search is confined once every \
         land it could select is vanilla. This is what proves the gate reads the library rather \
         than the printed word 'basic'"
    );
}

/// Remove from `player`'s library every LAND that carries rules content
/// `shortcut_efficacy`'s gate cannot classify, and return what was removed.
///
/// Lands only, deliberately: the search under test selects lands, so stripping
/// the rest would vary more than the arm claims to. The predicate mirrors the
/// public half of `carries_unreadable_rules_content` — if the two ever disagree
/// the ACCEPT arm reds rather than passing quietly, because a survivor the gate
/// still rejects keeps the verdict at `Shorten`.
fn strip_non_inert_library_lands(state: &mut GameState, player: PlayerId) -> Vec<ObjectId> {
    let doomed: Vec<ObjectId> = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player is seated")
        .library
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                o.card_types.core_types.contains(&CoreType::Land)
                    && (!o.trigger_definitions.is_empty()
                        || !o.replacement_definitions.is_empty()
                        || !o.static_definitions.is_empty()
                        || !o.keywords.is_empty())
            })
        })
        .collect();
    for id in &doomed {
        state.objects.remove(id);
    }
    let library = &mut state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player is seated")
        .library;
    library.retain(|id| !doomed.contains(id));
    doomed
}
