//! Cast-pipeline regression for White Sun's Twilight (`{X}{W}{W}` sorcery).
//!
//! Root cause: the Priority-7 static gate matched `"can't block"` INSIDE the
//! created token's quoted inline ability (`"This token can't block."`) and
//! routed the whole sorcery to the static parser, yielding `abilities: []`
//! (the spell did nothing). The fix masks double-quoted spans before
//! spell-line static classification (CR 111.3 + CR 111.4), so the spell's own
//! grammar (gain life / create tokens / conditional destroy) reaches the
//! effect parser. These tests drive the real cast pipeline and would fail if
//! the masking fix were reverted (the spell would resolve to a no-op).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

// Self-reference inside the token's quoted text is normalized to `~` in the
// shipped card data (`client/public/card-data.json`), so the regression uses the
// `~` form verbatim — the exact string the cast pipeline sees in a real game.
const ORACLE: &str = "You gain X life. Create X 1/1 colorless Phyrexian Mite artifact \
creature tokens with toxic 1 and \"~ can't block.\" If X is 5 or more, destroy \
all other creatures.";

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(
        color,
        engine::types::identifiers::ObjectId(0),
        false,
        vec![],
    )
}

/// `{X}{W}{W}`: the colored pips plus `x` colorless absorbing the generic {X}.
fn pool_for_x(x: usize) -> Vec<ManaUnit> {
    let mut pool = vec![mana(ManaType::White), mana(ManaType::White)];
    pool.extend((0..x).map(|_| mana(ManaType::Colorless)));
    pool
}

/// All Phyrexian Mite tokens currently on the battlefield.
fn mite_tokens(runner: &GameRunner) -> Vec<engine::types::identifiers::ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case("Mite"))
            })
        })
        .copied()
        .collect()
}

fn cast_white_suns_twilight(x: usize) -> (GameRunner, engine::game::scenario::CastOutcome) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool_for_x(x));

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "White Sun's Twilight", false, ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::White, ManaCostShard::White],
            generic: 0,
        })
        .id();

    // Seed an opponent creature to witness the X>=5 board wipe.
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).x(x as u32).resolve();
    (runner, outcome)
}

fn opponent_creature(runner: &GameRunner) -> Option<engine::types::identifiers::ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .find(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|obj| obj.name == "Grizzly Bears" && obj.controller == P1)
        })
        .copied()
}

/// X=5: caster gains 5 life; the `If X is 5 or more` clause fires, destroying
/// all OTHER creatures. Per the official ruling (shared with Martial Coup, the
/// same template), "all other creatures" means creatures that are NOT the tokens
/// this spell just created — so the 5 Mites SURVIVE while the opponent's seeded
/// creature is destroyed. The Mites are not "other": they were created earlier in
/// this same resolution.
///
/// CR 111.3 + CR 111.4: the token's quoted "can't block" is the token's text,
/// not a host static — so the spell's gain/create/destroy chain must run.
/// The "other"-excludes-self-created-tokens behavior is the official Martial Coup /
/// White Sun's Twilight Gatherer ruling (the CR has no numbered "other" entry).
/// CR 119.3: life gain. CR 701.7: create tokens. CR 701.8: destroy.
/// CR 702.164: Toxic.
#[test]
fn white_suns_twilight_x5_gains_creates_and_spares_own_mites() {
    let (runner, outcome) = cast_white_suns_twilight(5);

    // Life gain happens before the wipe and is unaffected by it.
    outcome.assert_life_delta(P0, 5);

    // The seeded opponent creature is destroyed by "destroy all other creatures".
    assert!(
        opponent_creature(&runner).is_none(),
        "X>=5 must destroy the opponent's Grizzly Bears"
    );
    // The 5 Mites this spell created are NOT "other creatures" — they were created
    // earlier in this same resolution — so the board wipe spares them.
    assert_eq!(
        mite_tokens(&runner).len(),
        5,
        "the Mites created at X=5 must survive the spell's own \"destroy all other creatures\""
    );
}

/// X=3: caster gains 3 life; 3 Mites created with the correct characteristics
/// (each 1/1, colorless, Artifact Creature — Phyrexian Mite, Toxic(1) +
/// CantBlock static); the `If X is 5 or more` clause does NOT fire, so the
/// seeded opponent creature — and the Mites — survive to be inspected.
#[test]
fn white_suns_twilight_x3_no_wipe() {
    let (runner, outcome) = cast_white_suns_twilight(3);

    outcome.assert_life_delta(P0, 3);

    let mites = mite_tokens(&runner);
    assert_eq!(mites.len(), 3, "X=3 creates 3 Mites");

    for id in &mites {
        let obj = runner.state().objects.get(id).expect("mite exists");
        assert_eq!(obj.power, Some(1), "Mite is 1/1");
        assert_eq!(obj.toughness, Some(1), "Mite is 1/1");
        assert!(
            obj.color.is_empty(),
            "Mite is colorless (no colors), got {:?}",
            obj.color
        );
        assert!(
            obj.card_types.core_types.contains(&CoreType::Artifact),
            "Mite is an artifact: {:?}",
            obj.card_types.core_types
        );
        assert!(
            obj.card_types.core_types.contains(&CoreType::Creature),
            "Mite is a creature: {:?}",
            obj.card_types.core_types
        );
        assert!(
            obj.card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Phyrexian")),
            "Mite has Phyrexian subtype: {:?}",
            obj.card_types.subtypes
        );
        assert!(
            obj.keywords.contains(&Keyword::Toxic(1)),
            "Mite has toxic 1: {:?}",
            obj.keywords
        );
        assert!(
            obj.static_definitions
                .iter_unchecked()
                .any(|d| d.mode == StaticMode::CantBlock),
            "Mite has a CantBlock static: {:?}",
            obj.static_definitions
        );
    }

    assert!(
        opponent_creature(&runner).is_some(),
        "X=3 (< 5): the conditional destroy must NOT fire; opponent creature survives"
    );
}

/// X=0: caster gains 0 life; 0 tokens; no destroy; no panic.
#[test]
fn white_suns_twilight_x0_is_a_no_op_without_panic() {
    let (runner, outcome) = cast_white_suns_twilight(0);

    outcome.assert_life_delta(P0, 0);
    assert_eq!(mite_tokens(&runner).len(), 0, "X=0 creates no Mites");
    assert!(
        opponent_creature(&runner).is_some(),
        "X=0 (< 5): no board wipe; opponent creature survives"
    );
}
