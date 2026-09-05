//! Regression (issue #8392): "exile the top card of each opponent's library"
//! degenerated into a free library pick.
//!
//! Oracle (verbatim, verified against Scryfall `cards/named?exact=Fire Lord Ozai`
//! and against `client/public/card-data.json`):
//!   "Whenever Fire Lord Ozai attacks, you may sacrifice another creature. If you
//!    do, add an amount of {R} equal to the sacrificed creature's power. Until end
//!    of combat, you don't lose this mana as steps end.
//!    {6}: Exile the top card of each opponent's library. Until end of turn, you
//!    may play one of those cards without paying its mana cost."
//!
//! BUG: `parse_library_player_suffix` — the single authority mapping a
//! top-of-library possessive to its owner filter — had rows for "each player's
//! library" but none for "each opponent's library". The clause therefore fell
//! through to the generic `ChangeZone(Library -> Exile)` path, which offers an
//! `EffectZoneChoice` tutor prompt over EVERY card in EVERY opponent's library.
//! Measured on the unpatched tree in a 3-player game: the prompt offered four
//! cards (both opponents' top AND second cards) with `min_count: 0`, and the
//! exile zone ended empty — so nothing was exiled and the trailing "play one of
//! those cards" had nothing to play.
//!
//! The same missing owner arm in `parse_dig_library_owner` made the
//! look-then-exile idiom (Lobelia, Defender of Bag End) fall through to
//! `TargetFilter::Controller` and SILENTLY exile the controller's own top card.
//!
//! FIX (parser only): both owner recognizers now map "each opponent's library" to
//! `TargetFilter::Opponent` as a parse-only scope sentinel, and the shared
//! `lift_distributive_exile_top_scope` erases that sentinel to `Controller` while
//! stamping `player_scope: Some(PlayerFilter::Opponent)`. The runtime fan-out then
//! rebinds the acting controller to each opponent in turn and `Effect::ExileTop`
//! reads exactly that opponent's library — the production-proven shape already
//! used by the `each player's` sibling (Etali / Extract Power / Lidless Gaze).
//!
//! Measured after the fix, same 3-player scenario: no prompt at all
//! (`WaitingFor::Priority`), each opponent's TOP card in exile, each opponent's
//! SECOND card untouched, and the controller's own library untouched.
//!
//! THREE PLAYERS IS LOAD-BEARING. In a two-player game `PlayerFilter::Opponent`
//! and a plain "the opponent" single-player reading are indistinguishable, so a
//! two-player test would pass against the bug. Every runtime test here uses
//! `new_n_player(3, ..)` so that "each opponent" must contribute two distinct
//! libraries.
//!
//! CR 401.1: when a game begins, each player's deck becomes their library — so
//! "each opponent's library" names one library per opponent.
//! CR 102.2 + CR 102.3: who a player's opponents are (team-aware).
//! CR 608.2c: the controller follows the instructions in the order written.
//! CR 406.3: exiled cards are face up by default; Lobelia exiles face down.

use engine::ai_support::legal_actions;
use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::GameScenario;
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Effect, LibraryPosition, PlayerFilter, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text (Scryfall / MTGJSON `TLE`).
const OZAI: &str = "Whenever Fire Lord Ozai attacks, you may sacrifice another creature. If you do, \
     add an amount of {R} equal to the sacrificed creature's power. Until end of combat, you don't \
     lose this mana as steps end.\n{6}: Exile the top card of each opponent's library. Until end of \
     turn, you may play one of those cards without paying its mana cost.";

/// Verbatim Oracle text (MTGJSON `LTR`). The look-then-exile idiom — a DIFFERENT
/// parser seam (`parse_dig_library_owner`) from Ozai's direct exile.
const LOBELIA: &str = "When Lobelia enters, look at the top card of each opponent's library and \
     exile those cards face down.\n{T}, Sacrifice an artifact: Choose one —\n• Until end of turn, \
     you may play a card exiled with Lobelia without paying its mana cost.\n• Each opponent loses \
     2 life and you gain 2 life.";

/// Verbatim Oracle text (MTGJSON `CLB`). Same clause as Ozai on a triggered ability.
const BRAINSTEALER_DRAGON: &str =
    "Flying\nAt the beginning of your end step, exile the top card of \
     each opponent's library. You may play those cards for as long as they remain exiled. If you \
     cast a spell this way, you may spend mana as though it were mana of any color to cast it.\n\
     Whenever a nonland permanent an opponent owns enters the battlefield under your control, they \
     lose life equal to its mana value.";

/// Verbatim Oracle text (MTGJSON `RIX`). The `each player's` sibling — the
/// INVARIANCE CONTROL for this whole change.
const ETALI: &str =
    "Whenever Etali attacks, exile the top card of each player's library, then you \
     may cast any number of spells from among those cards without paying their mana costs.";

/// Six generic mana, enough to activate Ozai's `{6}` ability.
fn six_colorless(source: ObjectId) -> Vec<ManaUnit> {
    (0..6)
        .map(|_| ManaUnit::new(ManaType::Colorless, source, false, Vec::new()))
        .collect()
}

/// The `ExileTop`-rooted ability in a parsed card's ACTIVATED abilities.
fn activated_exile_top(parsed: &engine::parser::oracle::ParsedAbilities) -> &AbilityDefinition {
    parsed
        .abilities
        .iter()
        .find(|a| matches!(&*a.effect, Effect::ExileTop { .. }))
        .expect("an ExileTop activated ability must be present")
}

/// The `ExileTop`-rooted ability body among a parsed card's TRIGGERS.
///
/// Scoped to the clause under test on purpose: Lobelia and Brainstealer both
/// carry unrelated abilities, so a card-scoped predicate would answer a question
/// about the wrong clause.
fn triggered_exile_top(parsed: &engine::parser::oracle::ParsedAbilities) -> &AbilityDefinition {
    parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find(|a| matches!(&*a.effect, Effect::ExileTop { .. }))
        .expect("an ExileTop trigger body must be present")
}

/// V1 + V2 — the reported bug, at runtime, in a three-player game.
///
/// V1 DISCRIMINATING ASSERTIONS: `zone_of(p1_top) == Exile` and
/// `zone_of(p2_top) == Exile`. With §5.1 reverted the ability parses to
/// `ChangeZone { Typed { controller: Opponent, InZone: Library } }` with no
/// `player_scope`, resolution halts on an `EffectZoneChoice` and NOTHING is
/// exiled — both assertions fail.
///
/// V2 DISCRIMINATING ASSERTION: the pipeline halts in `WaitingFor::Priority`,
/// i.e. no card-selection prompt is offered at all.
///
/// ORDERING IS LOAD-BEARING: `WaitingFor::Priority` is ALSO the state when the
/// ability fizzled, was never activated, or its cost failed — on its own it is an
/// absence-of-failure assertion. The positive exile assertions therefore execute
/// and must pass ABOVE it, proving the ability actually resolved and moved cards.
#[test]
fn ozai_exiles_top_card_of_each_opponent_in_three_player() {
    let mut scenario = GameScenario::new_n_player(3, 8_392);
    scenario.at_phase(Phase::PreCombatMain);

    // Seed each library: "second" first, then "top" (the helper re-seats each
    // card at index 0, so the last one added is the top).
    let p0_second = scenario.add_card_to_library_top(P0, "P0 Second");
    let p0_top = scenario.add_card_to_library_top(P0, "P0 Top");
    let p1_second = scenario.add_card_to_library_top(P1, "P1 Second");
    let p1_top = scenario.add_card_to_library_top(P1, "P1 Top");
    let p2_second = scenario.add_card_to_library_top(P2, "P2 Second");
    let p2_top = scenario.add_card_to_library_top(P2, "P2 Top");

    let ozai = scenario
        .add_creature_from_oracle(P0, "Fire Lord Ozai", 4, 4, OZAI)
        .id();
    scenario.with_mana_pool(P0, six_colorless(ozai));
    let mut runner = scenario.build();

    // Positive reach-guard: the activated ability exists to be activated at all.
    assert_eq!(
        runner.state().objects[&ozai].abilities.len(),
        1,
        "Ozai must carry exactly one activated ability ({{6}}: exile ...) to activate"
    );

    let outcome = runner.activate(ozai, 0).resolve();

    // --- V1: each opponent's TOP card is exiled. ---
    // Positive reach-guard for the whole test: cards actually moved.
    let exiled_count = outcome
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Exile)
        .count();
    assert_eq!(
        exiled_count, 2,
        "exactly two cards (one per opponent) must be exiled; \
         on the bug NOTHING is exiled and the exile zone is empty"
    );
    assert_eq!(
        outcome.zone_of(p1_top),
        Zone::Exile,
        "P1's top card must be exiled — 'each opponent's library' means every opponent"
    );
    assert_eq!(
        outcome.zone_of(p2_top),
        Zone::Exile,
        "P2's top card must be exiled — 'each opponent's library' means every opponent"
    );

    // Sibling/negative: only the TOP card moves. The bug offered a free pick over
    // every card in every opponent's library, second cards included.
    assert_eq!(
        outcome.zone_of(p1_second),
        Zone::Library,
        "only the TOP card is exiled — P1's second card must stay in the library"
    );
    assert_eq!(
        outcome.zone_of(p2_second),
        Zone::Library,
        "only the TOP card is exiled — P2's second card must stay in the library"
    );

    // HOSTILE FIXTURE — the third authority that must NOT contribute. This is the
    // arm `PlayerFilter::Opponent` -> `players::is_opponent` has to exclude; a
    // `PlayerFilter::All` scope would wrongly exile the controller's card here.
    assert_eq!(
        outcome.zone_of(p0_top),
        Zone::Library,
        "the CONTROLLER's own library must be untouched — opponents only, not each player"
    );
    assert_eq!(
        outcome.zone_of(p0_second),
        Zone::Library,
        "the controller's library must be untouched"
    );

    // --- V2: no card-selection prompt is offered at all. ---
    // Asserted LAST, deliberately: see the ordering note in this test's doc.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must complete with no selection prompt; got {:?}",
        outcome.final_waiting_for()
    );
}

/// V3 — the exiled cards from BOTH opponents feed ONE tracked set, so the
/// trailing "you may play one of those cards" can reach either of them.
///
/// This is the multi-authority case: two independent opponent libraries, each its
/// own owner authority, must both contribute to a single `TrackedSetFiltered(0)`
/// published once over the whole fan-out span.
///
/// DISCRIMINATING ASSERTION: a legal `CastSpell` action exists for BOTH opponents'
/// exiled cards. On the bug nothing is exiled, the tracked set is empty, and zero
/// such actions exist.
#[test]
fn ozai_grants_play_permission_over_every_opponents_exiled_card() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new_n_player(3, 8_393);
    scenario.at_phase(Phase::PreCombatMain);

    // One real card per opponent library (added first => top of that library),
    // plus a second card for P1 that must NOT become playable.
    let p1_top = scenario.add_real_card(P1, "Grizzly Bears", Zone::Library, db);
    let p1_second = scenario.add_real_card(P1, "Runeclaw Bear", Zone::Library, db);
    let p2_top = scenario.add_real_card(P2, "Memnite", Zone::Library, db);
    let p0_top = scenario.add_real_card(P0, "Ornithopter", Zone::Library, db);

    let ozai = scenario
        .add_creature_from_oracle(P0, "Fire Lord Ozai", 4, 4, OZAI)
        .id();
    scenario.with_mana_pool(P0, six_colorless(ozai));
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.activate(ozai, 0).resolve();

    // Positive reach-guard: both opponents' top cards really are in exile.
    assert_eq!(
        outcome.zone_of(p1_top),
        Zone::Exile,
        "P1's top card must be exiled before any play permission can reference it"
    );
    assert_eq!(
        outcome.zone_of(p2_top),
        Zone::Exile,
        "P2's top card must be exiled before any play permission can reference it"
    );

    let actions = legal_actions(outcome.state());
    // Positive reach-guard: a bare "contains both" over an EMPTY list is vacuous.
    assert!(
        !actions.is_empty(),
        "the controller must have legal actions at priority"
    );
    let castable = |id: ObjectId| {
        actions.iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id),
        )
    };

    assert!(
        castable(p1_top),
        "P0 must be able to play P1's exiled card through Ozai's tracked permission"
    );
    assert!(
        castable(p2_top),
        "P0 must be able to play P2's exiled card — the tracked set must contain the UNION \
         across every fan-out iteration, not just the last opponent's card"
    );

    // Negative: cards that were never exiled grant no permission.
    assert!(
        !castable(p1_second),
        "P1's second card was never exiled and must not be playable"
    );
    assert!(
        !castable(p0_top),
        "the controller's own library card was never exiled and must not be playable"
    );
}

/// V4 (SHAPE) — the look-then-exile idiom reads the OPPONENTS' libraries.
///
/// Distinct seam from V1: `parse_dig_library_owner` (not
/// `parse_library_player_suffix`), lifted at the `sequence.rs` back-patch site.
///
/// DISCRIMINATING ASSERTION: `player_scope == Some(PlayerFilter::Opponent)`. With
/// §5.2 reverted the recognizer falls through to `TargetFilter::Controller`, the
/// body carries no `player_scope`, and Lobelia silently exiles the CONTROLLER's
/// own top card.
///
/// The reach-guard is scoped to the CLAUSE under test, not the card: Lobelia also
/// carries an unrelated modal activated ability, so a card-scoped "no
/// Unimplemented anywhere" predicate would answer a question about the wrong
/// clause.
#[test]
fn lobelia_look_then_exile_scopes_to_opponents() {
    let parsed = parse_oracle_text(
        LOBELIA,
        "Lobelia, Defender of Bag End",
        &[],
        &["Creature".to_string()],
        &["Halfling".to_string(), "Citizen".to_string()],
    );
    let dbg = format!("{parsed:#?}");

    // Positive reach-guard, clause-scoped: the ETB trigger parsed and its body is
    // an ExileTop reading the top of a library.
    let body = triggered_exile_top(&parsed);
    match &*body.effect {
        Effect::ExileTop {
            player,
            position,
            face_down,
            ..
        } => {
            assert_eq!(
                *position,
                LibraryPosition::Top,
                "Lobelia looks at the TOP card of each opponent's library;\n{dbg}"
            );
            assert_eq!(
                *player,
                TargetFilter::Controller,
                "the lifted ExileTop rebinds to Controller, re-scoped per fan-out iteration;\n{dbg}"
            );
            // The new owner arm must not swallow the trailing "face down".
            assert!(
                *face_down,
                "Lobelia exiles those cards FACE DOWN (CR 406.3);\n{dbg}"
            );
        }
        other => panic!("expected ExileTop, got {other:?};\n{dbg}"),
    }

    assert_eq!(
        body.player_scope,
        Some(PlayerFilter::Opponent),
        "the look-then-exile body must fan out over each OPPONENT, not the controller;\n{dbg}"
    );
}

/// SHAPE — Brainstealer Dragon carries the identical clause on a triggered
/// ability, confirming the fix is keyed on the grammar rather than on one card's
/// ability kind.
#[test]
fn brainstealer_dragon_end_step_exile_scopes_to_opponents() {
    let parsed = parse_oracle_text(
        BRAINSTEALER_DRAGON,
        "Brainstealer Dragon",
        &[],
        &["Creature".to_string()],
        &["Dragon".to_string(), "Horror".to_string()],
    );
    let dbg = format!("{parsed:#?}");

    let body = triggered_exile_top(&parsed);
    assert!(
        matches!(
            &*body.effect,
            Effect::ExileTop {
                player: TargetFilter::Controller,
                position: LibraryPosition::Top,
                ..
            }
        ),
        "Brainstealer's end-step trigger must root on a lifted ExileTop;\n{dbg}"
    );
    assert_eq!(
        body.player_scope,
        Some(PlayerFilter::Opponent),
        "Brainstealer must fan out over each opponent;\n{dbg}"
    );
}

/// V6 (SHAPE + SYNTHETIC) — the PLURAL form scopes identically.
///
/// Honestly labelled: this grammar has ZERO corpus members today. The plural row
/// is added symmetrically with the singular one so a future printing never
/// becomes a second instance of this same bug, not because a card needs it now.
#[test]
fn synthetic_plural_each_opponent_library_top_scopes_to_opponents() {
    let parsed = parse_oracle_text(
        "Exile the top two cards of each opponent's library.",
        "Synthetic Plural Probe",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let dbg = format!("{parsed:#?}");

    // Positive reach-guard: it produced an ExileTop at all.
    let ability = activated_exile_top(&parsed);
    match &*ability.effect {
        Effect::ExileTop { count, player, .. } => {
            assert_eq!(
                *count,
                QuantityExpr::Fixed { value: 2 },
                "the plural form must carry the printed count;\n{dbg}"
            );
            assert_eq!(
                *player,
                TargetFilter::Controller,
                "lifted to Controller;\n{dbg}"
            );
        }
        other => panic!("expected ExileTop, got {other:?};\n{dbg}"),
    }
    assert_eq!(
        ability.player_scope,
        Some(PlayerFilter::Opponent),
        "the plural form must scope to each opponent exactly like the singular;\n{dbg}"
    );
}

/// V5 — INVARIANCE CONTROL, not a fix-detector.
///
/// By design this shows the SAME result whether the fix is present or absent:
/// the `each player's` path must NOT move. It is therefore exempt from "what
/// would this show if the fix were absent?" — its non-vacuity is BORROWED from
/// `ozai_exiles_top_card_of_each_opponent_in_three_player` in this same file,
/// which proves the new `Opponent` arm fires. If that test were ever deleted,
/// this one would become genuinely vacuous.
///
/// What it guards: that the two scopes have not collapsed into one. A
/// `PlayerFilter::Opponent` here would mean Etali stopped exiling the
/// controller's own top card — a regression on 16 cards.
#[test]
fn etali_each_player_scope_is_unchanged() {
    let parsed = parse_oracle_text(
        ETALI,
        "Etali, Primal Storm",
        &[],
        &["Creature".to_string()],
        &["Elder".to_string(), "Dinosaur".to_string()],
    );
    let dbg = format!("{parsed:#?}");

    let body = triggered_exile_top(&parsed);
    assert_eq!(
        body.player_scope,
        Some(PlayerFilter::All),
        "the 'each player's library' sibling must still fan out over ALL players;\n{dbg}"
    );
    assert!(
        matches!(
            &*body.effect,
            Effect::ExileTop {
                player: TargetFilter::Controller,
                ..
            }
        ),
        "Etali's ExileTop must remain player: Controller;\n{dbg}"
    );
}

/// HOSTILE FIXTURE — CR 609.3 (an effect does only as much as possible; its own
/// example covers moving cards out of a library): an opponent with an EMPTY
/// library contributes nothing and does not error.
///
/// NON-DEGENERACY IS DELIBERATE. The surviving opponent is given TWO library
/// cards, not one. With a single card this fixture does not discriminate: the
/// bug's free-pick prompt over a one-card population coincides with the correct
/// answer, so the test passes against the very defect it is supposed to survive.
/// (Measured: with §5.1 reverted, the one-card version of this test stayed
/// green.) With two cards the buggy path halts on an `EffectZoneChoice` over
/// both and exiles nothing, so the assertions below genuinely flip.
#[test]
fn ozai_empty_opponent_library_resolves_without_error() {
    let mut scenario = GameScenario::new_n_player(3, 8_394);
    scenario.at_phase(Phase::PreCombatMain);

    let p1_second = scenario.add_card_to_library_top(P1, "P1 Second");
    let p1_top = scenario.add_card_to_library_top(P1, "P1 Top");
    // P2: library intentionally left empty.

    let ozai = scenario
        .add_creature_from_oracle(P0, "Fire Lord Ozai", 4, 4, OZAI)
        .id();
    scenario.with_mana_pool(P0, six_colorless(ozai));
    let mut runner = scenario.build();

    let outcome = runner.activate(ozai, 0).resolve();

    assert_eq!(
        outcome.zone_of(p1_top),
        Zone::Exile,
        "the non-empty opponent's top card is still exiled"
    );
    // Discriminator: exactly the TOP card, not a free pick over the library.
    assert_eq!(
        outcome.zone_of(p1_second),
        Zone::Library,
        "only the TOP card is exiled — the second card stays in the library"
    );
    assert!(
        outcome
            .state()
            .players
            .iter()
            .find(|p| p.id == P2)
            .expect("P2 present")
            .library
            .is_empty(),
        "P2's empty library is a no-op — nothing to exile, no error"
    );
    // The empty library contributed nothing: exactly one card moved in total.
    assert_eq!(
        outcome
            .state()
            .objects
            .values()
            .filter(|o| o.zone == Zone::Exile)
            .count(),
        1,
        "only the one non-empty opponent contributes; the empty library adds nothing"
    );
}
