//! CR 202.3 + CR 608.2k: Celestial Kirin — "Whenever you cast a Spirit or Arcane
//! spell, destroy all permanents with that spell's mana value."
//!
//! The elliptical possessive ("with <referent>'s mana value") is a filter, not a
//! bare type phrase: the trigger destroys only the permanents whose mana value
//! EQUALS the triggering spell's, and it reads that value off the object the
//! trigger condition named (CR 608.2k).
//!
//! Revert-failing, with one important caveat. `shared_card_db` deserializes
//! PRE-PARSED triggers from the committed fixture; `add_real_card` uses that
//! face verbatim and never re-parses Oracle text. So the fixture-backed tests
//! below discriminate on the FILTER (strip the `Cmc` prop from the fixture and
//! they all fail), but they would NOT catch a parser regression that landed
//! without the fixture being regenerated.
//!
//! `kirin_live_parser_destroys_only_matching_mana_value` closes that gap: it
//! builds the Kirin from verbatim Oracle text through the live parser, so it
//! fails if the `parse_mana_value_suffix` production is reverted, fixture or
//! no fixture. The parser seam is additionally pinned directly by
//! `target_phrase_carries_possessive_mana_value_filter` in
//! `parser/oracle_target.rs`.

use engine::database::CardDatabase;
use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

use crate::support::shared_card_db as load_db;

/// The permanents the Kirin shares a battlefield with, spanning four distinct
/// mana values so every test discriminates rather than merely surviving.
struct Board {
    kirin: ObjectId,   // Celestial Kirin — {2}{W}{W}, MV 4
    birds: ObjectId,   // Birds of Paradise — {G}, MV 1
    memnite: ObjectId, // Memnite — {0}, MV 0
    bears: ObjectId,   // Grizzly Bears — {1}{G}, MV 2
    forest: ObjectId,  // Forest — a land, MV 0 (CR 202.3a: no mana cost)
}

impl Board {
    fn stage(scenario: &mut GameScenario, db: &CardDatabase) -> Self {
        let board = Self {
            kirin: scenario.add_real_card(P0, "Celestial Kirin", Zone::Battlefield, db),
            birds: scenario.add_real_card(P0, "Birds of Paradise", Zone::Battlefield, db),
            memnite: scenario.add_real_card(P0, "Memnite", Zone::Battlefield, db),
            bears: scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db),
            forest: scenario.add_real_card(P1, "Forest", Zone::Battlefield, db),
        };
        // CR 104.3c: Reach Through Mists draws a card, and drawing from an empty
        // library loses the game at the next state-based check — which exiles
        // every object the loser owns (CR 800.4a) and would mask the zone
        // assertions this file makes. Seed a library so the draw is a no-op.
        seed_library(scenario, P0, db);
        board
    }
}

/// Two spare cards in `player`'s library, so any incidental draw resolves.
fn seed_library(scenario: &mut GameScenario, player: PlayerId, db: &CardDatabase) {
    for _ in 0..2 {
        scenario.add_real_card(player, "Forest", Zone::Library, db);
    }
}

/// CR 202.3 + CR 608.2k: an MV-1 Arcane spell destroys exactly the MV-1
/// permanents. Reach Through Mists ({U} Instant — Arcane, "Draw a card.") is the
/// minimal Arcane trigger source: no targets, no Splice.
#[test]
fn kirin_destroys_only_permanents_with_the_cast_spells_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let board = Board::stage(&mut scenario, db);
    let reach = scenario.add_real_card(P0, "Reach Through Mists", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, reach, false, vec![])],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(reach).resolve();

    outcome.assert_zone(&[board.birds], Zone::Graveyard);
    outcome.assert_zone(
        &[board.kirin, board.memnite, board.bears, board.forest],
        Zone::Battlefield,
    );
}

/// CR 202.3 + CR 608.2k: the LIVE-PARSER case. Every other runtime test in this
/// file loads the Kirin from the committed fixture, which stores an
/// already-parsed trigger — so those tests pin the FILTER's runtime behavior but
/// would stay green if the `parse_mana_value_suffix` production were reverted
/// and the fixture not regenerated.
///
/// This one builds the Kirin from its verbatim Oracle text through the live
/// parser (`from_oracle_text_with_keywords`), so the production itself is on the
/// path under test. Reverting the parser branch drops the mana-value clause, the
/// filter becomes a bare `permanent`, and the survivors below all die.
///
/// The oracle-built Kirin has no mana cost, so its own mana value is 0; only
/// Birds of Paradise matches the MV-1 Arcane spell.
#[test]
fn kirin_live_parser_destroys_only_matching_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kirin = scenario
        .add_creature(P0, "Celestial Kirin", 3, 3)
        .from_oracle_text_with_keywords(
            &["Flying"],
            "Flying\nWhenever you cast a Spirit or Arcane spell, destroy all permanents \
             with that spell's mana value.",
        )
        .id();
    // The cast spell comes from the fixture so it carries real Arcane typing;
    // the parser production under test lives in the Kirin's trigger, not here.
    let birds = scenario.add_real_card(P0, "Birds of Paradise", Zone::Battlefield, db);
    let memnite = scenario.add_real_card(P0, "Memnite", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let reach = scenario.add_real_card(P0, "Reach Through Mists", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, reach, false, vec![])],
    );
    seed_library(&mut scenario, P0, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(reach).resolve();

    outcome.assert_zone(&[birds], Zone::Graveyard);
    outcome.assert_zone(&[kirin, memnite, bears], Zone::Battlefield);
}

/// CR 107.3a + CR 202.3e: while a spell is on the stack, X equals the announced
/// value, so Ugin's Conjurant ({X} Creature — Spirit Monk) cast for X=0 has mana
/// value 0 — and CR 202.3a gives a land mana value 0 too, so the trigger takes
/// the lands with it.
///
/// The trigger resolves above the Conjurant (CR 603.3 puts the trigger on the
/// stack above the spell that caused it; CR 608.1 resolves the stack LIFO), so
/// the Conjurant is
/// still a spell on the stack and is not itself a destroy candidate; it then
/// resolves as a 0/0 and dies to state-based actions (CR 704.5f). That is
/// incidental to this assertion, so nothing here asserts on it.
#[test]
fn kirin_x_spell_cast_for_zero_destroys_mana_value_zero_including_lands() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let board = Board::stage(&mut scenario, db);
    let conjurant = scenario.add_real_card(P0, "Ugin's Conjurant", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(conjurant).x(0).resolve();

    outcome.assert_zone(&[board.memnite, board.forest], Zone::Graveyard);
    outcome.assert_zone(&[board.kirin, board.birds, board.bears], Zone::Battlefield);
}

/// CR 603.2: the trigger condition is "a Spirit or Arcane spell", so a spell that
/// is neither must not trigger it at all. Birds of Paradise ({G} Creature — Bird)
/// shares the MV-1 victim's mana value, which is what makes this discriminating:
/// a trigger that fired regardless of subtype would destroy the board's Birds.
///
/// Paired reach guard: the identical board driven through an MV-1 *Arcane* cast
/// DOES lose its Birds. Without it this test would pass if the trigger never
/// fired for any reason at all.
#[test]
fn kirin_ignores_non_spirit_non_arcane_spell() {
    let Some(db) = load_db() else {
        return;
    };

    // Negative: casting a Bird does nothing.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let board = Board::stage(&mut scenario, db);
    let cast_birds = scenario.add_real_card(P0, "Birds of Paradise", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, cast_birds, false, vec![])],
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(cast_birds).resolve();
    outcome.assert_zone(
        &[
            board.kirin,
            board.birds,
            board.memnite,
            board.bears,
            board.forest,
        ],
        Zone::Battlefield,
    );

    // Reach guard: the same MV-1 cast, but Arcane, DOES kill the board's Birds.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let board = Board::stage(&mut scenario, db);
    let reach = scenario.add_real_card(P0, "Reach Through Mists", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, reach, false, vec![])],
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(reach).resolve();
    outcome.assert_zone(&[board.birds], Zone::Graveyard);
    // ... and takes ONLY the MV-1 permanents with it, so this half fails too if
    // the mana-value filter is ever dropped again.
    outcome.assert_zone(&[board.bears, board.forest], Zone::Battlefield);
}

/// CR 701.8: the filter carries no "other" clause, so the Kirin is an ordinary
/// destroy candidate against its own trigger. Kami of Old Stone ({3}{W} Creature
/// — Spirit, no rules text) matches the Kirin's own mana value of 4.
#[test]
fn kirin_destroys_itself_when_cast_spell_matches_its_own_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let board = Board::stage(&mut scenario, db);
    let kami = scenario.add_real_card(P0, "Kami of Old Stone", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, kami, false, vec![]),
            ManaUnit::new(ManaType::Colorless, kami, false, vec![]),
            ManaUnit::new(ManaType::Colorless, kami, false, vec![]),
            ManaUnit::new(ManaType::Colorless, kami, false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(kami).resolve();

    outcome.assert_zone(&[board.kirin], Zone::Graveyard);
    outcome.assert_zone(
        &[board.birds, board.memnite, board.bears, board.forest],
        Zone::Battlefield,
    );
}

/// CR 603.2 + CR 608.2k + CR 601.2i: TWO triggers with DIFFERENT authorities on
/// the stack at once. "That spell's" is provenance text, and its runtime read
/// goes through a single global `current_trigger_event` slot restored around
/// each resolution — so a design that leaked one trigger's authority into the
/// other would still pass every single-cast test in this file.
///
/// Cast Kami of Old Stone (MV 4) first, then Reach Through Mists (MV 1) in
/// response. The stack is [Kami, trigger(MV 4), Reach, trigger(MV 1)] and
/// resolves LIFO, so the MV-1 wave lands first and the MV-4 wave second. Each
/// must destroy by ITS OWN spell's mana value:
///
/// * Birds of Paradise (MV 1) dies to the Reach trigger.
/// * Celestial Kirin itself (MV 4) dies to the Kami trigger.
/// * Memnite (MV 0) and Grizzly Bears (MV 2) survive both waves.
///
/// A shared or last-writer-wins authority spares one of the two victims, which
/// no other test in this file would catch.
#[test]
fn two_triggers_each_destroy_by_their_own_spells_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let board = Board::stage(&mut scenario, db);
    let kami = scenario.add_real_card(P0, "Kami of Old Stone", Zone::Hand, db);
    let reach = scenario.add_real_card(P0, "Reach Through Mists", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, kami, false, vec![]),
            ManaUnit::new(ManaType::Colorless, kami, false, vec![]),
            ManaUnit::new(ManaType::Colorless, kami, false, vec![]),
            ManaUnit::new(ManaType::Colorless, kami, false, vec![]),
            ManaUnit::new(ManaType::Blue, reach, false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Kami (sorcery speed, empty stack) goes first; Reach is an Instant and is
    // cast in response, so both triggers are on the stack together.
    runner.cast(kami).commit();
    let outcome = runner.cast(reach).resolve();

    outcome.assert_zone(&[board.birds], Zone::Graveyard);
    outcome.assert_zone(&[board.kirin], Zone::Graveyard);
    outcome.assert_zone(&[board.memnite, board.bears], Zone::Battlefield);
}

/// CR 115.1 + CR 603.3d + CR 608.2k: the same filter on a TARGETED trigger —
/// Skyfire Kirin's "gain control of target creature with that spell's mana
/// value". This is the non-obvious half: the demonstrative must resolve while the
/// engine builds the trigger's target slots, not only at resolution, or the legal
/// set is computed against a mana value of 0.
///
/// Both halves stage exactly ONE opposing creature, so the engine assigns the
/// slot itself when that creature is legal and removes the trigger for want of a
/// legal target (CR 603.3d) when it is not. Control of that creature is then a
/// clean, purely behavioral read of the filter:
///
/// * MV 1 (matches the MV-1 Arcane spell) → control changes.
/// * MV 2 (does not match) → control does NOT change. An unfiltered trigger
///   would find it legal, auto-assign it, and steal it.
#[test]
fn skyfire_kirin_restricts_target_to_the_cast_spells_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    // Reach guard: the MV-1 creature IS taken, proving the trigger fires and
    // reaches its effect at all.
    let (outcome, victim) = skyfire_cast_against(db, "Birds of Paradise");
    assert_eq!(
        outcome.state().objects.get(&victim).map(|o| o.controller),
        Some(P0),
        "an MV-1 Arcane spell must let Skyfire Kirin take the MV-1 creature"
    );

    // Discriminator: the MV-2 creature is NOT a legal target, so the trigger is
    // removed and control never changes.
    let (outcome, survivor) = skyfire_cast_against(db, "Grizzly Bears");
    assert_eq!(
        outcome.state().objects.get(&survivor).map(|o| o.controller),
        Some(P1),
        "an MV-1 Arcane spell must NOT let Skyfire Kirin take an MV-2 creature —          an unfiltered trigger would find it legal and auto-assign it"
    );
}

/// Stage Skyfire Kirin opposite a single creature named `opposing_creature`, cast
/// Reach Through Mists ({U} Arcane, MV 1), and run the stack out. Returns the
/// outcome and the opposing creature's id.
fn skyfire_cast_against(db: &CardDatabase, opposing_creature: &str) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Skyfire Kirin", Zone::Battlefield, db);
    let victim = scenario.add_real_card(P1, opposing_creature, Zone::Battlefield, db);
    let reach = scenario.add_real_card(P0, "Reach Through Mists", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, reach, false, vec![])],
    );
    seed_library(&mut scenario, P0, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // CR 608.2d: the trigger is a "you may", and the driver declines optional
    // effects by default — accept, or the effect never runs and both halves of
    // the assertion would read "control unchanged" for the wrong reason.
    (runner.cast(reach).accept_optional().resolve(), victim)
}
