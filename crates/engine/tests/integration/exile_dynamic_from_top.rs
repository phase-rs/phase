//! Runtime discriminators for the dynamic-count top-of-library exile parser arm
//! (`parse_dynamic_exile_from_top` in `parser/oracle_effect/imperative.rs`).
//!
//! The class — "exile [dynamic count] cards from the top of <owner>'s library" —
//! was silently lowered by the generic `ChangeZone(Library→Exile)` fallback,
//! which has no count slot, so the exile count was dropped entirely. The new arm
//! lowers it to `Effect::ExileTop { player, count, face_down }` with the count
//! bound to the ability-defined dynamic quantity.
//!
//! These tests drive the REAL resolver (`resolve_ability_chain` /
//! `engine::game::apply`) over verbatim Oracle text, covering all four axis
//! values with a revert-probe for each. The three fixtures are:
//! owner=self × Form A ("that many") = Feldon, Ronom Excavator;
//! owner=self × Form B ("equal to <expr>") = Bone Mask;
//! owner=other × Form B' ("equal to <expr>" before the source) = Rakdos, the Muscle.
//!
//! owner=other × Form A is intentionally absent from the flipped set: every
//! Form-A "that many" card exiles "your library" = Controller; the only "their
//! library" Form-A card, Expedited Inheritance, is already `ExileTop` at base and
//! is a NO-OP, so no fixture reaches that combination.
//!
//! CR 701.13 (Exile) + CR 401.1 (library) + CR 608.2c (later text defines the
//! earlier count). CR 115.1: "target player's library" owner. CR 202.3: Rakdos's
//! "its mana value".

use engine::game::ability_utils::{build_resolved_from_def, build_resolved_from_def_with_targets};
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, CastingPermission, Effect, TargetRef};
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Seed `count` named cards onto the TOP of `player`'s library (top-first) and
/// return their ids in top-to-bottom order. `create_object(..Library)` appends,
/// and `exile_top` takes `library.iter().take(n)` from the front, so the first
/// id created is the top card.
fn seed_library(
    state: &mut GameState,
    player: engine::types::player::PlayerId,
    count: usize,
) -> Vec<ObjectId> {
    let base = state.next_object_id + 5000;
    (0..count)
        .map(|i| {
            create_object(
                state,
                CardId(base + i as u64),
                player,
                format!("Lib {player:?} #{i}"),
                Zone::Library,
            )
        })
        .collect()
}

fn library_len(state: &GameState, player: engine::types::player::PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .library
        .len()
}

// ---------------------------------------------------------------------------
// owner=self × Form A — Feldon, Ronom Excavator
// ---------------------------------------------------------------------------

/// > Whenever Feldon is dealt damage, exile that many cards from the top of your
/// > library. Choose one of them. Until the end of your next turn, you may play
/// > that card.
///
/// Form A: "that many" = the triggering damage amount (`EventContextAmount`).
/// DISCRIMINATOR: with the arm, dealing N=3 damage exiles EXACTLY 3 cards off the
/// top of the controller's library and the trailing "choose one of them" is
/// offered exactly those 3. Reverting the arm lowers the exile to a count-less
/// `ChangeZone`, which does not exile the top three (the choose is offered a
/// different/empty set), flipping every assertion below.
#[test]
fn feldon_form_a_that_many_exiles_exactly_the_damage_amount() {
    const FELDON_BODY: &str = "exile that many cards from the top of your library. Choose one of \
them. Until the end of your next turn, you may play that card.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let feldon = scenario
        .add_creature(P0, "Feldon, Ronom Excavator", 2, 3)
        .id();
    let mut runner = scenario.build();

    // Five cards on top of P0's library; only the top three must be exiled.
    let lib = seed_library(runner.state_mut(), P0, 5);
    let before = library_len(runner.state(), P0);

    // Trigger event: Feldon dealt 3 damage. "that many" reads this amount.
    runner.state_mut().current_trigger_event = Some(GameEvent::DamageDealt {
        source_id: ObjectId(9999),
        target: TargetRef::Object(feldon),
        amount: 3,
        is_combat: false,
        excess: 0,
    });

    let def = parse_effect_chain(FELDON_BODY, AbilityKind::Spell);
    let resolved = build_resolved_from_def(&def, feldon, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("Feldon exile→choose chain resolves (pausing at the choose)");

    // COUNT DISCRIMINATOR: exactly the top 3 moved Library→Exile; the 4th/5th stay.
    assert_eq!(
        library_len(runner.state(), P0),
        before - 3,
        "exactly 3 cards (the dealt-damage amount) must leave the library"
    );
    for top in &lib[..3] {
        assert_eq!(
            runner.state().objects[top].zone,
            Zone::Exile,
            "{top:?} (a top-3 card) must be exiled"
        );
    }
    for keep in &lib[3..] {
        assert_eq!(
            runner.state().objects[keep].zone,
            Zone::Library,
            "{keep:?} (below the top 3) must remain in the library"
        );
    }

    // REACH GUARD: the exiled 3 are published as the tracked set and the trailing
    // "choose one of them" offers exactly those 3 — proving the ExileTop count
    // flowed into the chain's choose/grant wire (not a vacuous negative).
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the controller chooses");
            assert_eq!(
                cards.len(),
                3,
                "the choose offers exactly the 3 exiled cards"
            );
            for top in &lib[..3] {
                assert!(cards.contains(top), "{top:?} must be offered to the choose");
            }
        }
        other => panic!(
            "expected the exiled-3 to be offered to \"choose one of them\"; got {other:?} \
             (reverting the arm yields a count-less ChangeZone and a different WaitingFor)"
        ),
    }
}

// ---------------------------------------------------------------------------
// owner=self × Form B — Bone Mask (dynamic ref binds, not a constant)
// ---------------------------------------------------------------------------

/// > Exile cards from the top of your library equal to the damage prevented this
/// > way.
///
/// Form B (no "a number of" determiner). "the damage prevented this way" is a
/// dynamic event-context quantity (`EventContextAmount`, fed by
/// `last_effect_count`). DISCRIMINATOR: seeding the prevented amount to 3 and
/// resolving exiles EXACTLY 3 — proving a DYNAMIC ref bound into `ExileTop.count`,
/// not a constant/default (a `Fixed{1}` would exile 1; the reverted count-less
/// `ChangeZone` exiles a different amount). This is the key seam: the count is
/// bound at parse time to a reference resolved from live state, not baked in.
#[test]
fn bone_mask_form_b_dynamic_ref_binds_exile_count() {
    const BONE_MASK_EXILE: &str =
        "Exile cards from the top of your library equal to the damage prevented this way.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bone_mask = scenario.add_creature(P0, "Bone Mask", 0, 0).id();
    let mut runner = scenario.build();

    // Six library cards so exiling the seeded 4 provably leaves 2 — distinguishing
    // the dynamic bind (4) from a constant `Fixed{1}` (1), a count-less ChangeZone
    // (0), AND an "exile the whole library" degenerate (6).
    let _lib = seed_library(runner.state_mut(), P0, 6);
    let before = library_len(runner.state(), P0);

    // The prevention applier stamps `last_effect_count` with the prevented amount;
    // "the damage prevented this way" (EventContextAmount) reads it. Seed 4 so the
    // resolved count must track the seeded prevention amount, not a default.
    runner.state_mut().last_effect_count = Some(4);

    let def = parse_effect_chain(BONE_MASK_EXILE, AbilityKind::Activated);
    let resolved = build_resolved_from_def(&def, bone_mask, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("Bone Mask exile clause resolves");

    // The dynamic ref resolved to the seeded 4 — NOT 1 (a constant), NOT 0 (a
    // count-less ChangeZone), and NOT 6 (the whole library).
    assert_eq!(
        library_len(runner.state(), P0),
        before - 4,
        "the dynamic \"damage prevented this way\" ref must bind and exile exactly the seeded 4"
    );
    assert_eq!(
        library_len(runner.state(), P0),
        2,
        "2 library cards must remain — the count tracked the seeded prevention amount (4), not all 6"
    );
}

// ---------------------------------------------------------------------------
// owner=other × Form B' — Rakdos, the Muscle (owner axis + honest mana-any gap)
// ---------------------------------------------------------------------------

/// > Whenever you sacrifice another creature, exile cards equal to its mana value
/// > from the top of target player's library. Until your next end step, you may
/// > play those cards, and mana of any type can be spent to cast those spells.
///
/// Form B' (count clause BEFORE the library source). owner = "target player's" =
/// `TargetFilter::Player`; count = "its mana value" = the sacrificed creature's
/// mana value (`ObjectManaValue{Anaphoric}`, read from the sacrifice event
/// source).
///
/// OWNER-AXIS DISCRIMINATOR (R4): the cards leave the TARGETED player's (P1's)
/// library, NOT the controller's (P0's). Reverting the arm lowers the exile to a
/// count-less `ChangeZone`, which does not remove the sacrificed-MV cards off
/// P1's library top — flipping the P1-library assertion.
///
/// HONEST-GAP (R5): the "mana of any type can be spent" clause remains a
/// pre-existing dropped rider — the granted `PlayFromExile` carries
/// `mana_spend_permission: None`. The fix must neither upgrade that silent drop
/// into a false "supported" (a `Some(AnyType)` the runtime doesn't honor) nor a
/// worse misparse.
#[test]
fn rakdos_form_b_prime_exiles_targeted_players_library_owner_axis() {
    const RAKDOS_BODY: &str =
        "exile cards equal to its mana value from the top of target player's \
library. Until your next end step, you may play those cards, and mana of any type can be spent to \
cast those spells.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rakdos = scenario.add_creature(P0, "Rakdos, the Muscle", 6, 5).id();
    let mut runner = scenario.build();

    // A creature sacrificed by P0 with mana value 2 ("its mana value" referent),
    // still present in `state.objects` (graveyard) so the event source resolves.
    let sacrificed = create_object(
        runner.state_mut(),
        CardId(4242),
        P0,
        "Sacrificed MV2".to_string(),
        Zone::Graveyard,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&sacrificed)
        .unwrap()
        .mana_cost = ManaCost::generic(2);

    // Distinct libraries: exile must hit P1's (the target), never P0's (controller).
    let p0_lib = seed_library(runner.state_mut(), P0, 4);
    let p1_lib = seed_library(runner.state_mut(), P1, 4);

    // Trigger event: P0 sacrificed the MV-2 creature. "its mana value" reads the
    // event source's mana value (Anaphoric → EventSource).
    runner.state_mut().current_trigger_event = Some(GameEvent::PermanentSacrificed {
        object_id: sacrificed,
        player_id: P0,
    });

    let def = parse_effect_chain(RAKDOS_BODY, AbilityKind::Spell);
    // "target player's library" owner = TargetFilter::Player → reads ability.targets.
    let resolved =
        build_resolved_from_def_with_targets(&def, rakdos, P0, vec![TargetRef::Player(P1)]);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("Rakdos exile→grant chain resolves");

    // OWNER-AXIS DISCRIMINATOR: exactly MV=2 cards left the TARGETED player's
    // (P1's) library; the controller's (P0's) library is untouched.
    assert_eq!(
        library_len(runner.state(), P1),
        2,
        "2 cards (the sacrificed creature's mana value) must leave the TARGETED player's library"
    );
    assert_eq!(
        library_len(runner.state(), P0),
        4,
        "the controller's own library must NOT be touched (owner = Player, not Controller)"
    );
    for exiled in &p1_lib[..2] {
        assert_eq!(
            runner.state().objects[exiled].zone,
            Zone::Exile,
            "{exiled:?} must be exiled"
        );
    }
    for kept in &p0_lib {
        assert_eq!(
            runner.state().objects[kept].zone,
            Zone::Library,
            "{kept:?} (controller's library) must remain"
        );
    }

    // HONEST-GAP (R5): the exiled P1 cards carry PlayFromExile granted to the
    // controller, with the "mana of any type" rider still DROPPED
    // (mana_spend_permission == None) — the exile-count fix did not upgrade the
    // pre-existing silent drop.
    let granted_card = p1_lib[0];
    let perms = &runner.state().objects[&granted_card].casting_permissions;
    let play_perm = perms.iter().find_map(|perm| match perm {
        CastingPermission::PlayFromExile {
            granted_to,
            mana_spend_permission,
            ..
        } if *granted_to == P0 => Some(*mana_spend_permission),
        _ => None,
    });
    let mana_spend =
        play_perm.expect("exiled card must carry the controller's PlayFromExile grant");
    assert!(
        mana_spend.is_none(),
        "the \"mana of any type can be spent\" rider is a pre-existing honest gap and must stay \
         dropped (mana_spend_permission == None), not upgraded to a false-supported Some(..); got {mana_spend:?}"
    );
}

// ---------------------------------------------------------------------------
// DEFERRED-GAP TRIPWIRES — pin the CURRENT declined behavior of honest follow-ups
// so a future fix that supports them FLIPS these assertions (prompting an update).
// These are deliberately parse-shape checks: they assert an UNSUPPORTED clause is
// NOT yet lowered to `ExileTop`, i.e. that the count remains honestly dropped.
// ---------------------------------------------------------------------------

/// FOLLOW-UP: Dead Man's Chest — "exile cards equal to its power from the top of
/// **its owner's** library". The owner recognizer's closed 6-owner table has no
/// anaphoric-object-owner ("its owner's") entry, so the dynamic arm DECLINES and
/// the exile stays a count-less `ChangeZone` (the count/library-owner are lost).
/// When an "its owner" → player `TargetFilter` is added, this clause will lower to
/// `ExileTop` and this tripwire FLIPS — update it then.
#[test]
fn dead_mans_chest_owner_gap_still_declines_to_non_exiletop() {
    let def = parse_effect_chain(
        "exile cards equal to its power from the top of its owner's library.",
        AbilityKind::Spell,
    );
    assert!(
        !matches!(&*def.effect, Effect::ExileTop { .. }),
        "DEFERRED: \"its owner's library\" must still DECLINE (not ExileTop) until an \
         anaphoric-object-owner filter exists; got {:?}",
        def.effect
    );
}

/// FOLLOW-UP: the bare "from the top" form with NO "of <owner> library" qualifier
/// (Vault 112: Sadistic Simulation / Magus of the Mind — "shuffle your library,
/// then exile that many cards from the top"). The owner recognizer requires the
/// "of <owner> library" suffix, so the dynamic arm DECLINES. That clause's real
/// defect is a SEPARATE chain-loss after the preceding `Shuffle` (outside
/// `parse_exile_ast`), so declining here is correct. This tripwire pins that the
/// bare form is not (yet) an `ExileTop` via this arm.
#[test]
fn bare_from_the_top_without_owner_declines_to_non_exiletop() {
    let def = parse_effect_chain("exile that many cards from the top.", AbilityKind::Spell);
    assert!(
        !matches!(&*def.effect, Effect::ExileTop { .. }),
        "DEFERRED: bare \"from the top\" (no \"of <owner> library\") must DECLINE via this arm; \
         got {:?}",
        def.effect
    );
}
