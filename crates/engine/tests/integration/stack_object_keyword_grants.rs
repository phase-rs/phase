//! CR 613.1 + CR 611.2c + CR 400.7a: a continuous effect that grants a keyword to an
//! object on the STACK must actually land on that object.
//!
//! THE DEFECT (task #125). A triggered ability whose `GenericEffect` grants a keyword to
//! `TriggeringSource` registers a transient continuous effect (TCE) whose `affected` filter
//! is `TargetFilter::SpecificObject { id }` (`effects/effect.rs::register_transient_effect`).
//! The layer pass then computes the ZONES it must scan for that filter's affected population
//! via `layers::continuous_effect_scan_zones`. `SpecificObject` is an IDENTITY reference — it
//! carries no zone marker — so `TargetFilter::extract_in_zone()` returned `None` for it, the
//! accumulated zone set came back empty, and the pass fell back to `[Zone::Battlefield]`.
//!
//! A spell on the stack is not in `Zone::Battlefield`, so it was never in the scanned
//! population and the grant was **silently dropped**. The effect resolved, the TCE was
//! registered, and nothing happened.
//!
//! THE CLASS IS EXACTLY 2 FACES, and the discriminator is the battlefield/stack line.
//! Measured over the card-data export: 16 faces carry the identical
//! `AddKeyword → TriggeringSource` AST shape, but 14 of them fire on BATTLEFIELD trigger
//! modes (Crews / SaddlesOrCrews / Attacks / ChangesZone), where `TriggeringSource` resolves
//! to a battlefield permanent and the grant landed fine. Only the 2 `SpellCast` faces aim at
//! a STACK object:
//!
//!   * Taigam, Ojutai Master — grants `Rebound` to the instant/sorcery just cast.
//!   * Waystone's Guidance   — grants `Mobilize 1` to the first creature spell each turn.
//!
//! Both are exercised below from their VERBATIM Oracle text through the real cast pipeline.
//! `ogre_battledriver_*` is the BATTLEFIELD REGRESSION WITNESS: the same AST shape on the
//! battlefield side of the line, which must be UNCHANGED by the fix.
//!
//! THE TWO FACES ARE NOT EQUALLY BROKEN — measured, and worth stating precisely, because the
//! naive reading ("both cards are dead") is wrong. Both faces drop the grant while the spell
//! is ON THE STACK. But `ObjectId` is stable across the stack → battlefield move, so a
//! PERMANENT spell picks the grant back up from the battlefield scan the instant it resolves.
//! Consequently Waystone's mobilize reached the creature even pre-fix, and only TAIGAM had a
//! broken end-to-end outcome — rebound is the one keyword here that must FUNCTION while the
//! spell is still on the stack (CR 702.88a). Each test below states which side of that line it
//! is on, and no test in this file claims to witness a defect it did not actually go red on.
//!
//! CR references (each grep-verified against docs/MagicCompRules.txt):
//!
//! - CR 613.1: an object's characteristics are computed by applying all applicable continuous
//!   effects in layers. It speaks of OBJECTS, not permanents.
//! - CR 611.2c: the set of objects a resolution-generated continuous effect affects is
//!   determined when the effect begins, and does not change afterward.
//! - CR 400.7a: effects from triggered abilities that change the characteristics of a PERMANENT
//!   SPELL on the stack continue to apply to the permanent that spell becomes.
//! - CR 702.88a: rebound is a static ability that FUNCTIONS WHILE THE SPELL IS ON THE STACK; a
//!   rebound spell cast from hand is exiled as it resolves instead of being put into its
//!   owner's graveyard, and offers a next-upkeep recast.
//! - CR 608.2n: an instant/sorcery goes to its owner's graveyard as the final step of
//!   resolution — the destination rebound displaces. This is what makes the Exile assertion a
//!   real end-to-end discriminator rather than a restatement of the keyword assertion.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Taigam, Ojutai Master — 3/4. VERBATIM Oracle text (verified against card-data.json).
const TAIGAM: &str = "Instant, sorcery, and Dragon spells you control can't be countered.\nWhenever you cast an instant or sorcery spell from your hand, if Taigam attacked this turn, that spell gains rebound.";

/// Waystone's Guidance — enchantment. VERBATIM Oracle text (verified against card-data.json).
const WAYSTONES_GUIDANCE: &str = "Attacking tokens you control get +1/+0.\nWhenever you cast your first creature spell each turn, that spell gains mobilize 1.";

/// Ogre Battledriver — 3/3. VERBATIM Oracle text (verified against card-data.json).
/// The BATTLEFIELD-side member of the same AST class: `AddKeyword(Haste) → TriggeringSource`.
const OGRE_BATTLEDRIVER: &str = "Whenever another creature you control enters, that creature gets +2/+0 and gains haste until end of turn. (It can attack and {T} this turn.)";

/// Does `id` carry a keyword of the same KIND as `probe`? Mirrors the engine's own
/// `keywords::has_keyword`, which compares by discriminant — so `Mobilize(_)` matches any N.
fn has_kw(state: &GameState, id: ObjectId, probe: &Keyword) -> bool {
    state
        .objects
        .get(&id)
        .is_some_and(|o| engine::game::keywords::has_keyword(o, probe))
}

/// Put Taigam on the battlefield, swing with him (so his `AttackedThisTurn`
/// intervening-if is TRUE), and return to a main phase holding `spell` in hand.
///
/// `attacks` is the axis that turns the intervening-if on and off — it is the
/// CR 603.4 control, not a convenience.
///
/// The scenario is PLACED at `PreCombatMain` via `at_phase` rather than advanced into it:
/// advancing from turn 1 would run the draw step against an empty library, decking the player
/// and ending the game before a single spell could be cast.
fn taigam_board(
    spell_is_instant: bool,
    attacks: bool,
) -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let taigam = scenario
        .add_creature_from_oracle(P0, "Taigam, Ojutai Master", 3, 4, TAIGAM)
        .id();
    let spell = if spell_is_instant {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Test Instant", true, "You gain 3 life.")
            .id()
    } else {
        // NEGATIVE CONTROL vehicle: a CREATURE spell. Taigam's `valid_card` is
        // Or[Instant, Sorcery], so his trigger must NOT fire for this one.
        scenario
            .add_creature_to_hand_from_oracle(P0, "Test Creature", 2, 2, "")
            .id()
    };
    let mut runner = scenario.build();

    if attacks {
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(taigam, AttackTarget::Player(P1))])
            .expect("Taigam must be able to attack");
        runner.combat_damage();
        runner.advance_to_phase(Phase::PostCombatMain);
    }

    (runner, taigam, spell)
}

// ===========================================================================
// HARNESS POSITIVE CONTROL — makes every zero below meaningful.
// ===========================================================================

/// PREMISE. Taigam's trigger must actually FIRE: after committing the cast, the stack must
/// hold TWO entries — the spell itself and Taigam's triggered ability on top of it.
///
/// Without this, a missing `Rebound` below could mean "the trigger never fired" (a targeting
/// or intervening-if bug) rather than "the grant was dropped by the layer pass". This is the
/// assertion that localizes the defect to the LAYER and not to the trigger machinery.
#[test]
fn premise_taigam_trigger_fires_when_he_attacked() {
    let (mut runner, taigam, spell) = taigam_board(true, true);
    runner.cast(spell).commit();

    assert!(
        runner.state().objects.contains_key(&taigam),
        "PREMISE: Taigam must still be alive — this is NOT the purged-source seam"
    );
    assert_eq!(
        runner.state().stack.len(),
        2,
        "PREMISE: the stack must hold the spell AND Taigam's triggered ability. If this is 1 \
         the trigger never fired and every assertion in this file is testing the wrong seam."
    );
}

/// CR 603.4 FIRST-CHECK CONTROL. If Taigam did NOT attack, the intervening-if is false and
/// the ability never triggers — so the spell must not gain rebound for a reason that has
/// nothing to do with the layer pass. This is what stops the fix from being a blanket
/// "grant rebound to everything".
#[test]
fn taigam_that_did_not_attack_never_triggers() {
    let (mut runner, _taigam, spell) = taigam_board(true, false);
    runner.cast(spell).commit();

    assert_eq!(
        runner.state().stack.len(),
        1,
        "CR 603.4: with Taigam not having attacked, the intervening-if is FALSE at the trigger \
         event and only the spell itself is on the stack"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "CR 608.2n: an instant with no rebound goes to the graveyard"
    );
}

// ===========================================================================
// FACE 1 — Taigam, Ojutai Master. PRIMARY WITNESS, RED before the fix.
// ===========================================================================

/// PRIMARY WITNESS — RED before the fix.
///
/// Taigam attacked, so his trigger fires and resolves. Resolving it registers a TCE granting
/// `Rebound` to the spell — which is ON THE STACK. Pre-fix the layer pass scanned only
/// `Zone::Battlefield` for the `SpecificObject` affected filter, never saw the stack object,
/// and the keyword never appeared.
///
/// This asserts the keyword on the STACK OBJECT ITSELF, at the moment CR 702.88a says rebound
/// must function ("a static ability that functions while the spell is on the stack") — before
/// the spell resolves.
#[test]
fn taigam_grants_rebound_to_the_spell_while_it_is_on_the_stack() {
    let (mut runner, _taigam, spell) = taigam_board(true, true);
    runner.cast(spell).commit();

    // Resolve ONLY Taigam's triggered ability (top of stack). The spell stays on the stack
    // underneath it — which is exactly where rebound has to be live.
    runner.resolve_top();

    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "PREMISE: the spell must still be ON THE STACK when we look for the granted keyword"
    );
    assert!(
        has_kw(runner.state(), spell, &Keyword::Rebound),
        "CR 613.1 + CR 702.88a: Taigam's resolved trigger grants rebound to the spell, and \
         rebound is a static ability that FUNCTIONS WHILE THE SPELL IS ON THE STACK. Pre-fix \
         the layer pass scanned only Zone::Battlefield for the SpecificObject affected filter, \
         so the grant was silently dropped and this read []."
    );
}

/// END-TO-END WITNESS — RED before the fix.
///
/// The keyword assertion above could in principle be satisfied without the rest of the rules
/// engine noticing. This drives the DOWNSTREAM rules behavior: a spell that has rebound as it
/// resolves is EXILED instead of going to the graveyard (CR 702.88a displacing CR 608.2n), and
/// arms exactly one next-upkeep recast (CR 603.7a).
///
/// Pre-fix: `mercy zone = Graveyard`, zero delayed triggers.
#[test]
fn taigam_granted_rebound_exiles_the_spell_and_arms_the_recast_cr_702_88a() {
    let (mut runner, _taigam, spell) = taigam_board(true, true);
    runner.cast(spell).commit();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Exile,
        "CR 702.88a: a spell that has rebound as it resolves is EXILED instead of being put \
         into its owner's graveyard (CR 608.2n displaced). Pre-fix the grant never landed, so \
         the spell resolved normally into the graveyard."
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "CR 603.7a + CR 702.88a: rebound arms exactly one next-upkeep 'you may cast this card \
         from exile' delayed triggered ability"
    );
}

/// NEGATIVE CONTROL — the fix must not FABRICATE grants.
///
/// Taigam's `valid_card` is Or[Instant, Sorcery]. Casting a CREATURE spell with an attacking
/// Taigam on board must not trigger him at all, so the creature spell must never gain rebound.
/// A fix that made every stack object eligible for every TCE would fail here.
#[test]
fn taigam_does_not_grant_rebound_to_a_creature_spell() {
    let (mut runner, _taigam, creature) = taigam_board(false, true);
    runner.cast(creature).commit();

    assert_eq!(
        runner.state().stack.len(),
        1,
        "Taigam triggers only on instant/sorcery spells — a creature spell must not trigger him"
    );
    runner.advance_until_stack_empty();
    assert!(
        !has_kw(runner.state(), creature, &Keyword::Rebound),
        "the fix must widen the layer pass's ZONE DOMAIN, not its affected set — a spell \
         Taigam never triggered on must not acquire rebound"
    );
}

// ===========================================================================
// FACE 2 — Waystone's Guidance. SECOND WITNESS, RED before the fix.
// ===========================================================================

/// SECOND FACE — RED before the fix. This is what converts the defect from a single-card
/// anecdote into a measured 2-face CLASS: a different keyword (`Mobilize 1`, not `Rebound`),
/// a different trigger constraint (`NthSpellThisTurn`, not an intervening-if), a different
/// spell type (creature, not instant) — and the identical inert grant.
#[test]
fn waystones_guidance_grants_mobilize_to_the_creature_spell_on_the_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Waystone's Guidance", 0, 0, WAYSTONES_GUIDANCE)
        .as_enchantment();
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Creature", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    runner.cast(creature).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "PREMISE: Waystone's Guidance must trigger on the first creature spell cast this turn. \
         If this is 1 the trigger never fired and the assertion below tests the wrong seam."
    );

    // Resolve ONLY the triggered ability; the creature spell stays on the stack beneath it.
    runner.resolve_top();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Stack,
        "PREMISE: the creature spell must still be on the stack when the grant is applied"
    );
    assert!(
        has_kw(
            runner.state(),
            creature,
            &Keyword::Mobilize(engine::types::ability::QuantityExpr::Fixed { value: 1 })
        ),
        "CR 613.1: the resolved trigger grants mobilize 1 to the creature SPELL. Pre-fix the \
         layer pass never scanned the stack for the SpecificObject affected filter, so the \
         grant was silently dropped — the identical inertness Taigam shows with rebound."
    );
}

/// CR 400.7a REGRESSION PIN — GREEN BOTH BEFORE AND AFTER. **This is NOT a witness.**
///
/// "Effects from spells, activated abilities, and triggered abilities that change the
/// characteristics or controller of a permanent spell on the stack continue to apply to the
/// permanent that spell becomes."
///
/// MEASURED, AND IT CORRECTS THE ORIGINAL PREMISE. This assertion passed BEFORE the fix. The
/// reason is that `ObjectId` is stable across the stack → battlefield move, so the moment the
/// creature spell resolves, the very same `SpecificObject` TCE is picked up by the (already
/// working) battlefield scan and the grant lands. So Waystone's Guidance was NOT inert
/// end-to-end the way Taigam is:
///
///   * BOTH faces drop the grant during the STACK WINDOW — that is the 2-face class, and it is
///     witnessed by the sibling test above, which DID go red.
///   * Only TAIGAM has a broken observable OUTCOME, because rebound is the one keyword that
///     must FUNCTION while the spell is on the stack (CR 702.88a). Mobilize is an attack
///     trigger, so its stack-window absence was unobservable at end state.
///
/// It is kept because it locks the CR 400.7a carry-over that the fix must not break: the
/// zone-domain widening re-scans the TCE in the object's CURRENT zone every pass, so a
/// regression that pinned the scan to `Zone::Stack` (rather than following the object) would
/// break exactly here while leaving the stack-window witness green.
#[test]
fn waystones_granted_mobilize_survives_onto_the_permanent_cr_400_7a() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Waystone's Guidance", 0, 0, WAYSTONES_GUIDANCE)
        .as_enchantment();
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Creature", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    runner.cast(creature).commit();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "PREMISE: the creature spell must have resolved onto the battlefield"
    );
    assert!(
        has_kw(
            runner.state(),
            creature,
            &Keyword::Mobilize(engine::types::ability::QuantityExpr::Fixed { value: 1 })
        ),
        "CR 400.7a: a triggered ability's effect that changed the characteristics of a PERMANENT \
         SPELL on the stack continues to apply to the permanent that spell becomes. The creature \
         must still have mobilize 1 on the battlefield — otherwise the grant is useless, since \
         mobilize is an attack trigger that can never fire on a spell."
    );
}

// ===========================================================================
// BATTLEFIELD REGRESSION WITNESS — must be GREEN both before AND after.
// ===========================================================================

/// REGRESSION WITNESS — the battlefield side of the same AST class.
///
/// Ogre Battledriver carries the IDENTICAL `GenericEffect{ static_abilities: [AddKeyword →
/// ParentTarget], target: TriggeringSource }` shape as Taigam and Waystone. The only
/// difference is that its `TriggeringSource` resolves to a battlefield permanent. It stands
/// for the 14 faces in the class that already worked.
///
/// This must pass BEFORE the fix (proving the battlefield path was never broken) and AFTER
/// (proving the zone-domain widening did not disturb it). The fix changes what zone a
/// `SpecificObject` filter is scanned in; for a battlefield object that zone is still
/// `Zone::Battlefield`, so this behavior must be byte-for-byte unchanged.
#[test]
fn ogre_battledriver_still_grants_haste_on_the_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Ogre Battledriver", 3, 3, OGRE_BATTLEDRIVER)
        .id();
    let entering = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Creature", 2, 2, "")
        .id();
    let mut runner = scenario.build();

    runner.cast(entering).commit();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&entering].zone,
        Zone::Battlefield,
        "PREMISE: the creature must have entered the battlefield"
    );
    assert!(
        has_kw(runner.state(), entering, &Keyword::Haste),
        "REGRESSION: Ogre Battledriver's haste grant is the BATTLEFIELD member of the same AST \
         class. It worked before the stack fix and must still work after — the fix widens the \
         SCAN ZONE of a SpecificObject filter to the object's actual zone, which for a \
         battlefield permanent is still Zone::Battlefield."
    );
    assert_eq!(
        runner.state().objects[&entering].power,
        Some(4),
        "REGRESSION: the +2/+0 rides on the SAME TCE as the haste grant — if the zone-domain \
         change had disturbed the battlefield population, the pump would vanish with it"
    );
}
