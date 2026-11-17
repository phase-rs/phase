//! "Exile it instead" damage spells: runtime observations of the printed behavior. The rows
//! for Ob Nixilis's Cruelty, Pinecone Strike, Brutal Expulsion and Sparks Ablaze's rider
//! clause were census false positives (confirmed-misparses.toml) released by this file;
//! Feed the Flames, Narset's Rebuke, Khaaaaaaaaaaaannn!, Suplex, Bouncer's Beatdown and Bot
//! Bashing Time share the clause shape and are kept as extra class coverage (the first five
//! were released upstream in #10952). Sparks Ablaze's planeswalker branch is the same rider
//! as Brutal Expulsion's planeswalker test. Oracle texts are the MTGJSON `oracle_text` of
//! each face as exported in card-data.json (checked against Scryfall).
//!
//! Clause shape: "If that creature would die this turn, exile it instead." (CR 614.1a +
//! CR 614.6), a replacement scoped to the one damaged/targeted creature. Each test: a target
//! the spell kills (damage, or -5/-5 for Ob Nixilis's Cruelty) is exiled instead of dying.
//! The shared `single_target` harness proves scope on a second board where the target survives
//! the spell: while that rider is installed and unconsumed, a different creature dying reaches
//! the graveyard, and the damaged survivor's own later death is exiled. Bouncer's Beatdown, the
//! both-modes test and the planeswalker test (0 loyalty, CR 704.5i) have their own setups.
//!
//! Deliberately NOT covered: "If a creature dealt damage this way would die ..." faces
//! (Pillar of Flame, Annihilating Fire, ...). Their rider is not gated on damage having
//! actually been dealt (CR 615.6 prevention), so their census rows stay.

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHOCK: &str = "Shock deals 2 damage to any target.";

/// Whether the spell under test is an instant or a sorcery (CR 304 / CR 307).
#[derive(Clone, Copy)]
enum SpellKind {
    Instant,
    Sorcery,
}

impl SpellKind {
    /// The scenario builder takes this as a boolean at its call boundary only.
    fn is_instant(self) -> bool {
        matches!(self, Self::Instant)
    }
}

struct Case {
    oracle: &'static str,
    kind: SpellKind,
    modes: Option<&'static [usize]>,
    x: Option<u32>,
    damage: i32,
}

fn cast_spell(
    scenario: GameScenario,
    spell: ObjectId,
    c: &Case,
    targets: &[ObjectId],
) -> (GameRunner, CastOutcome) {
    let mut runner = scenario.build();
    let mut cast = runner.cast(spell);
    if let Some(m) = c.modes {
        cast = cast.modes(m);
    }
    if let Some(x) = c.x {
        cast = cast.x(x);
    }
    if !targets.is_empty() {
        cast = cast.target_objects(targets);
    }
    let outcome = cast.resolve();
    (runner, outcome)
}

fn stage(c: &Case, name: &str) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..8)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut b = scenario.add_spell_to_hand_from_oracle(P0, name, c.kind.is_instant(), c.oracle);
    if c.x.is_some() {
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        });
    }
    let spell = b.id();
    (scenario, spell)
}

/// Single-target spell, two boards.
/// * Lethal: target toughness == damage, so the target is exiled instead of dying.
/// * Scope: the target survives the spell (`damage + 2` toughness), so its rider is still
///   installed and unconsumed when an unrelated creature dies; that death must reach the
///   graveyard, and the damaged survivor's own later death must still be exiled. Applying the
///   rider to the target first would consume it (`consume_on_apply`) and prove nothing.
fn single_target(c: Case, name: &str) {
    let (mut scenario, spell) = stage(&c, name);
    let target = scenario
        .add_creature(P1, "Target Bear", c.damage, c.damage)
        .id();
    let (_, outcome) = cast_spell(scenario, spell, &c, &[target]);
    assert_eq!(
        outcome.zone_of(target),
        Zone::Exile,
        "{name}: a creature that would die is exiled instead"
    );

    let (mut scenario, spell) = stage(&c, name);
    let survivor = scenario
        .add_creature(P1, "Survivor Bear", c.damage + 2, c.damage + 2)
        .id();
    let bystander = scenario.add_creature(P1, "Bystander Bear", 2, 2).id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let finisher = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let (mut runner, outcome) = cast_spell(scenario, spell, &c, &[survivor]);
    assert_eq!(
        outcome.zone_of(survivor),
        Zone::Battlefield,
        "{name}: the survivor outlives the spell, leaving its rider live"
    );
    let after = runner.cast(shock).target_object(bystander).resolve();
    assert_eq!(
        after.zone_of(bystander),
        Zone::Graveyard,
        "{name}: the replacement is scoped to the damaged creature only"
    );
    let finished = runner.cast(finisher).target_object(survivor).resolve();
    assert_eq!(
        finished.zone_of(survivor),
        Zone::Exile,
        "{name}: the damaged survivor's later death is still exiled"
    );
}

macro_rules! single {
    ($fn:ident, $name:expr, $oracle:expr, $kind:expr, $modes:expr, $x:expr, $dmg:expr) => {
        #[test]
        fn $fn() {
            single_target(
                Case {
                    oracle: $oracle,
                    kind: $kind,
                    modes: $modes,
                    x: $x,
                    damage: $dmg,
                },
                $name,
            );
        }
    };
}

single!(bot_bashing_time, "Bot Bashing Time",
    "Bot Bashing Time deals 6 damage to target creature. If that creature would die this turn, exile it instead.",
    SpellKind::Sorcery, None, None, 6);
single!(feed_the_flames, "Feed the Flames",
    "Feed the Flames deals 5 damage to target creature. If that creature would die this turn, exile it instead.",
    SpellKind::Instant, None, None, 5);
single!(narsets_rebuke, "Narset's Rebuke",
    "Narset's Rebuke deals 5 damage to target creature. Add {U}{R}{W}. If that creature would die this turn, exile it instead.",
    SpellKind::Instant, None, None, 5);
single!(khaaaaaaaaaaaannn, "Khaaaaaaaaaaaannn!",
    "Khaaaaaaaaaaaannn! deals twice X damage to target creature. If that creature would die this turn, exile it instead.",
    SpellKind::Instant, None, Some(2), 4);
single!(ob_nixilis_cruelty, "Ob Nixilis's Cruelty",
    "Target creature gets -5/-5 until end of turn. If that creature would die this turn, exile it instead.",
    SpellKind::Instant, None, None, 5);
single!(sparks_ablaze, "Sparks Ablaze",
    "Sparks Ablaze deals 2 damage to target creature or planeswalker. If that creature or planeswalker would die this turn, exile it instead.\nThe next planeswalker spell you cast costs {1} less to cast.",
    SpellKind::Instant, None, None, 2);
single!(pinecone_strike, "Pinecone Strike",
    "Choose one or both —\n• Pinecone Strike deals 3 damage to target creature. If that creature would die this turn, exile it instead.\n• Destroy target artifact token.",
    SpellKind::Instant, Some(&[0]), None, 3);
single!(suplex, "Suplex",
    "Choose one —\n• Suplex deals 3 damage to target creature. If that creature would die this turn, exile it instead.\n• Exile target artifact.",
    SpellKind::Sorcery, Some(&[0]), None, 3);
single!(brutal_expulsion, "Brutal Expulsion",
    "Devoid (This card has no color.)\nChoose one or both —\n• Return target spell or creature to its owner's hand.\n• Brutal Expulsion deals 2 damage to target creature or planeswalker. If that creature or planeswalker would die this turn, exile it instead.",
    SpellKind::Instant, Some(&[1]), None, 2);

const BOUNCERS_BEATDOWN: &str = "This spell costs {2} less to cast if it targets a black permanent.\nBouncer's Beatdown deals X damage to target creature or planeswalker, where X is the greatest power among creatures you control. If that creature or planeswalker would die this turn, exile it instead.";

/// P0's greatest power is 3, so X is 3: P1 gets a `toughness`/`toughness` creature to target.
fn beatdown_board(toughness: i32) -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Small", 1, 1);
    scenario.add_creature(P0, "Big", 3, 3);
    let target = scenario
        .add_creature(P1, "Target Bear", toughness, toughness)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bouncer's Beatdown", true, BOUNCERS_BEATDOWN)
        .id();
    (scenario, spell, target)
}

/// X is the greatest power among creatures P0 controls (3): a 3-toughness target is exiled.
#[test]
fn bouncers_beatdown() {
    let (scenario, spell, target) = beatdown_board(3);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();
    assert_eq!(outcome.zone_of(target), Zone::Exile);
}

/// The rider survives on a 5-toughness target (X = 3), so a bystander's death is a graveyard
/// death while the damaged survivor's later Shock death is still exiled.
#[test]
fn bouncers_beatdown_rider_is_scoped_to_the_damaged_creature() {
    let (mut scenario, spell, survivor) = beatdown_board(5);
    let bystander = scenario.add_creature(P1, "Bystander Bear", 2, 2).id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let finisher = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(survivor).resolve();
    assert_eq!(outcome.zone_of(survivor), Zone::Battlefield);
    let after = runner.cast(shock).target_object(bystander).resolve();
    assert_eq!(
        after.zone_of(bystander),
        Zone::Graveyard,
        "the rider is scoped to the damaged creature"
    );
    let finished = runner.cast(finisher).target_object(survivor).resolve();
    assert_eq!(
        finished.zone_of(survivor),
        Zone::Exile,
        "the rider still exiles the damaged survivor"
    );
}

const BRUTAL_EXPULSION: &str = "Devoid (This card has no color.)\nChoose one or both —\n• Return target spell or creature to its owner's hand.\n• Brutal Expulsion deals 2 damage to target creature or planeswalker. If that creature or planeswalker would die this turn, exile it instead.";

/// Both modes chosen: the rider must bind to the DAMAGED creature (mode 2's target), not to
/// the creature bounced by mode 1.
#[test]
fn brutal_expulsion_both_modes_binds_rider_to_the_damaged_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bounced = scenario.add_creature(P1, "Bounced Bear", 2, 2).id();
    let damaged = scenario.add_creature(P1, "Damaged Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Brutal Expulsion", true, BRUTAL_EXPULSION)
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .modes(&[0, 1])
        .target_objects(&[bounced, damaged])
        .resolve();
    assert_eq!(
        outcome.zone_of(bounced),
        Zone::Hand,
        "mode 1 bounces its target"
    );
    assert_eq!(
        outcome.zone_of(damaged),
        Zone::Exile,
        "the rider binds to the creature dealt lethal damage by mode 2"
    );
}

/// CR 704.5i + CR 614.6: "creature or planeswalker" — a planeswalker that reaches 0 loyalty
/// from Brutal Expulsion's damage is put into the graveyard by a state-based action, which
/// is a separate path from creature death; the rider must exile it instead.
#[test]
fn brutal_expulsion_exiles_a_planeswalker_that_loses_all_loyalty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Test Walker", "Gideon", 2, "")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Brutal Expulsion", true, BRUTAL_EXPULSION)
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .modes(&[1])
        .target_object(walker)
        .resolve();
    assert_eq!(
        outcome.zone_of(walker),
        Zone::Exile,
        "the damaged planeswalker is exiled instead of dying to 0 loyalty"
    );
}
