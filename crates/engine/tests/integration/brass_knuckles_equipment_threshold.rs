//! Brass Knuckles / Balan, Wandering Knight — the attachment-threshold static
//! condition: "as long as two or more Equipment are attached to it".
//!
//! Oracle text below is byte-identical to `data/mtgjson/AtomicCards.json`:
//!
//!   Brass Knuckles | {4} | Artifact — Equipment
//!     When you cast this spell, copy it. (The copy becomes a token.)
//!     Equipped creature has double strike as long as two or more Equipment are attached to it.
//!     Equip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)
//!
//!   Balan, Wandering Knight | {2}{W}{W} | Legendary Creature — Cat Knight
//!     First strike
//!     Balan has double strike as long as two or more Equipment are attached to it.
//!     {1}{W}: Attach all Equipment you control to Balan.
//!
//! WHAT IS UNDER TEST. The condition, not the grant. Before this change the
//! clause parsed to `StaticCondition::Unrecognized`, which `game::layers`
//! evaluates as `true` — so the engine granted double strike to a creature
//! wearing only ONE Equipment, strictly stronger than the printed card. These
//! tests discriminate 1 attachment from 2, so a condition stuck at `true` fails
//! the negative half and a condition stuck at `false` fails the positive half.
//!
//! Two PRODUCERS are covered because the two cards reach the same combinator by
//! different routes, and a fix that bound only one of them would pass the other:
//!   - Brass Knuckles: `affected` is an equipped-creature filter, so the layer
//!     system's per-recipient fork binds "it" to the EQUIPPED CREATURE
//!     (CR 301.5a) — recipient != source.
//!   - Balan: `affected` is `SelfRef`, so recipient == source.
//!
//! CR 611.3a is the authorizing rule for reading the count live: a continuous
//! effect from a static ability "isn't locked in; it applies at any given moment
//! to whatever its text indicates". CR 613.1f places the grant in layer 6.
//! CR 702.4a defines double strike. CR 704.5p is why every filler Equipment
//! carries the `Equipment` subtype: an attached noncreature permanent that is
//! neither Aura, Equipment, nor Fortification is unattached by the next SBA
//! check, which would silently make these tests measure nothing.
//!
//! NOT a cast-pipeline test, deliberately. Brass Knuckles' first line copies the
//! spell, and per CR 301.5b ("Equipment ... don't enter the battlefield attached
//! to a creature") and CR 301.5e ("If the Equipment is a token, it's created and
//! enters the battlefield unattached") that token copy contributes NOTHING to the
//! attachment count. Casting would add a trigger and a token while changing the
//! quantity under test by zero. The behavior under test is a layer evaluation, so
//! these tests drive the layer evaluation directly.

use engine::game::game_object::AttachTarget;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const BRASS_KNUCKLES_ORACLE: &str = "When you cast this spell, copy it. (The copy becomes a token.)\nEquipped creature has double strike as long as two or more Equipment are attached to it.\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)";

const BALAN_ORACLE: &str = "First strike\nBalan has double strike as long as two or more Equipment are attached to it.\n{1}{W}: Attach all Equipment you control to Balan.";

/// Force a full layer re-derive, then read the EFFECTIVE keyword set (CR 613).
/// Mirrors `a_sigil_of_myrkul.rs`'s `has_deathtouch`.
fn has_double_strike(runner: &mut GameRunner, id: ObjectId) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], &Keyword::DoubleStrike)
}

/// CR 301.5: attach `equipment` to `host`, mirroring how the equip action wires
/// `attached_to` + `attachments` (`game::effects::attach`). Production also calls
/// `mark_layers_full`; `has_double_strike` does that on every read.
fn attach(runner: &mut GameRunner, equipment: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    state.objects.get_mut(&equipment).unwrap().attached_to = Some(AttachTarget::Object(host));
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(equipment);
}

/// A bare Equipment with no rules text — padding for the attachment count that
/// cannot contribute statics of its own. CR 704.5p: the `Equipment` subtype is
/// mandatory or the SBA check unattaches it.
fn add_bare_equipment(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature(P0, name, 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id()
}

/// CR 301.5a + CR 611.3a: Brass Knuckles grants double strike to the EQUIPPED
/// CREATURE only while two or more Equipment are attached to that creature.
///
/// The negative half is the discriminator: with Brass Knuckles as the creature's
/// ONLY attachment the count is 1, so the grant must be OFF. Reverting the
/// parser change makes the condition `Unrecognized`, which evaluates `true`, and
/// this assertion fails. The positive half on the SAME board is its paired
/// reach-guard: it proves the static exists, is live, and does grant — so the
/// negative cannot pass merely because the card failed to parse.
#[test]
fn brass_knuckles_double_strike_requires_two_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear = scenario.add_creature(P0, "Equipped Bear", 2, 2).id();
    let knuckles = scenario
        .add_artifact_from_oracle(P0, "Brass Knuckles", BRASS_KNUCKLES_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let second = add_bare_equipment(&mut scenario, "Bone Saw");

    let mut runner = scenario.build();

    // Count == 1: Brass Knuckles IS the one and only Equipment on the bear.
    attach(&mut runner, knuckles, bear);
    assert!(
        !has_double_strike(&mut runner, bear),
        "CR 611.3a: with only Brass Knuckles attached the count is 1, so the \
         \"two or more Equipment\" gate must be OFF. A condition stuck at `true` \
         (the pre-fix `StaticCondition::Unrecognized` behavior) fails here."
    );

    // Count == 2: same board, one more Equipment on the same creature.
    attach(&mut runner, second, bear);
    assert!(
        has_double_strike(&mut runner, bear),
        "CR 613.1f + CR 702.4a: with two Equipment attached the gate is satisfied \
         and the equipped creature must have double strike. This is the paired \
         reach-guard proving the static parsed and is live."
    );
}

/// CR 301.5a: the count is taken against the EQUIPPED CREATURE ("it"), not the
/// battlefield at large. A second Equipment on a DIFFERENT creature must not
/// satisfy the gate — that would be a board-global count, and it would also be
/// what a source-bound (rather than recipient-bound) referent produces here.
#[test]
fn brass_knuckles_counts_only_equipment_on_the_equipped_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear = scenario.add_creature(P0, "Equipped Bear", 2, 2).id();
    let other = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let knuckles = scenario
        .add_artifact_from_oracle(P0, "Brass Knuckles", BRASS_KNUCKLES_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let elsewhere = add_bare_equipment(&mut scenario, "Bone Saw");

    let mut runner = scenario.build();
    attach(&mut runner, knuckles, bear);
    attach(&mut runner, elsewhere, other);

    assert!(
        !has_double_strike(&mut runner, bear),
        "the second Equipment is on a DIFFERENT creature, so the equipped \
         creature's attachment count is still 1 and the gate stays OFF"
    );
    assert!(
        !has_double_strike(&mut runner, other),
        "the other creature is not equipped by Brass Knuckles, so it is not a \
         recipient of the static at all"
    );

    // Reach-guard: move the second Equipment onto the equipped creature and the
    // same board flips. Without this the two negatives above would be vacuous.
    runner
        .state_mut()
        .objects
        .get_mut(&other)
        .unwrap()
        .attachments
        .clear();
    attach(&mut runner, elsewhere, bear);
    assert!(
        has_double_strike(&mut runner, bear),
        "with both Equipment now on the equipped creature the count is 2 and the \
         gate turns ON"
    );
}

/// CR 611.3a: Balan reaches the same combinator through the `SelfRef` producer,
/// where the static's recipient IS its source. Guarding it separately proves the
/// referent binds correctly on both routes.
///
/// `from_oracle_text_with_keywords` supplies the inline `First strike` line so it
/// is not swallowed as unparsed text; the `FirstStrike` assertion is a second
/// reach-guard showing the card's other statics are live even while double strike
/// is correctly OFF.
#[test]
fn balan_double_strike_requires_two_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let balan = scenario
        .add_creature(P0, "Balan, Wandering Knight", 2, 2)
        .as_legendary()
        .with_subtypes(vec!["Cat", "Knight"])
        .from_oracle_text_with_keywords(&["First strike"], BALAN_ORACLE)
        .id();
    let first = add_bare_equipment(&mut scenario, "Bone Saw");
    let second = add_bare_equipment(&mut scenario, "Sai of the Shinobi");

    let mut runner = scenario.build();

    assert!(
        has_keyword(&runner.state().objects[&balan], &Keyword::FirstStrike),
        "reach-guard: Balan's printed first strike must be live, proving the card \
         parsed at all"
    );

    // Count == 1.
    attach(&mut runner, first, balan);
    assert!(
        !has_double_strike(&mut runner, balan),
        "CR 611.3a: one Equipment attached to Balan does not satisfy \
         \"two or more\", so the gate must be OFF"
    );

    // Count == 2, same board.
    attach(&mut runner, second, balan);
    assert!(
        has_double_strike(&mut runner, balan),
        "two Equipment attached to Balan satisfies the gate; recipient == source \
         on this producer and the referent must still bind"
    );
    assert!(
        has_keyword(&runner.state().objects[&balan], &Keyword::FirstStrike),
        "first strike is unaffected by the double-strike gate"
    );
}
