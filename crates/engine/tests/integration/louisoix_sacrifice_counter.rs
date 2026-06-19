//! Issue #408 — Louisoix's Sacrifice (counter target).
//!
//! Louisoix's Sacrifice reads "Counter target activated ability, triggered
//! ability, or noncreature spell." Before the fix, `parse_counter_ast` routed
//! the whole disjunction through bare `parse_target`, producing a degenerate
//! empty-`type_filters` `Typed { InZone: Stack }` filter — the noncreature
//! restriction was dropped and the runtime counter-target legality check
//! rejected the filter, so the counter no-opped.
//!
//! The fix adds `parse_stack_object_target` (a nom combinator in
//! `oracle_nom/target.rs`) that recognizes the three-way "activated ability,
//! triggered ability, or noncreature spell" disjunction and wires it into
//! `parse_counter_ast`. The produced filter is
//! `Or { StackAbility, Typed { Non(Creature), InZone: Stack } }`.
//!
//! These tests drive the real engine:
//!   - the parser (via `CardDatabase::from_export`) to confirm the produced
//!     `Effect::Counter` target filter;
//!   - `find_legal_targets` — the exact target-legality path `apply` uses at
//!     target-declaration time — to confirm activated abilities, triggered
//!     abilities, and noncreature spells ARE legal targets while a creature
//!     spell is NOT;
//!   - `counter::resolve` to confirm each legal target type is actually
//!     countered (CR 701.6a).

use engine::database::card_db::CardDatabase;
use engine::game::targeting::find_legal_targets;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityKind, Effect, ResolvedAbility, TargetFilter, TargetRef, TypeFilter,
};
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::zones::Zone;
use engine::types::PlayerId;

use crate::support::shared_card_db as load_db;

/// Extract the `Effect::Counter` target filter from Louisoix's Sacrifice's
/// parsed card definition.
fn louisoix_counter_target(db: &CardDatabase) -> TargetFilter {
    let face = db
        .get_face_by_name("Louisoix's Sacrifice")
        .expect("Louisoix's Sacrifice should be in the card database");
    face.abilities
        .iter()
        .find_map(|a| match a.effect.as_ref() {
            Effect::Counter { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("Louisoix's Sacrifice should parse a Counter effect")
}

/// Issue #408 — the parser must produce the full three-way disjunction, not a
/// degenerate empty-`type_filters` stack filter. The noncreature restriction
/// must survive on the spell disjunct.
#[test]
fn louisoix_sacrifice_parses_disjunctive_counter_target() {
    let Some(db) = load_db() else {
        return;
    };

    let target = louisoix_counter_target(db);

    let TargetFilter::Or { filters } = &target else {
        panic!(
            "expected Or {{ StackAbility, noncreature-spell }}, got {target:?} \
             — the degenerate empty-typed `Typed` regression has returned"
        );
    };
    assert_eq!(
        filters.len(),
        2,
        "disjunction must have ability + spell legs"
    );

    // Ability leg — any activated/triggered ability on the stack.
    assert!(
        filters.iter().any(|f| matches!(
            f,
            TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: None,
            }
        )),
        "missing the activated/triggered ability disjunct: {target:?}"
    );

    // Spell leg — noncreature restriction is carried as `Non(Creature)` and
    // the leg is pinned to the stack zone.
    let spell_leg = filters
        .iter()
        .find_map(|f| match f {
            TargetFilter::Typed(tf) => Some(tf),
            _ => None,
        })
        .expect("missing the typed noncreature-spell disjunct");
    assert_eq!(
        spell_leg.type_filters,
        vec![TypeFilter::Non(Box::new(TypeFilter::Creature))],
        "the spell disjunct must EXCLUDE creature spells (noncreature restriction)"
    );
    assert!(
        spell_leg.properties.iter().any(|p| matches!(
            p,
            engine::types::ability::FilterProp::InZone { zone: Zone::Stack }
        )),
        "the spell disjunct must be pinned to the stack zone"
    );
}

/// Build a `GameState` whose stack carries one activated ability, one
/// triggered ability, one noncreature (instant) spell, and one creature spell.
/// Returns the four object ids in that order.
fn stack_with_four_entries() -> (GameState, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut state = GameState::new_two_player(42);

    let perm = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Ability Source".to_string(),
        Zone::Battlefield,
    );

    let activated = ObjectId(901);
    let triggered = ObjectId(902);
    state.stack.push_back(StackEntry {
        id: activated,
        source_id: perm,
        controller: PlayerId(1),
        kind: StackEntryKind::ActivatedAbility {
            source_id: perm,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "Act".to_string(),
                    description: None,
                },
                vec![],
                perm,
                PlayerId(1),
            ),
        },
    });
    state.stack.push_back(StackEntry {
        id: triggered,
        source_id: perm,
        controller: PlayerId(1),
        kind: StackEntryKind::TriggeredAbility {
            source_id: perm,
            ability: Box::new(ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "Trig".to_string(),
                    description: None,
                },
                vec![],
                perm,
                PlayerId(1),
            )),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
        },
    });

    // Noncreature spell — an instant.
    let noncreature_spell = create_object(
        &mut state,
        CardId(20),
        PlayerId(1),
        "Lightning Bolt".to_string(),
        Zone::Stack,
    );
    // Creature spell — must NOT be a legal target.
    let creature_spell = create_object(
        &mut state,
        CardId(21),
        PlayerId(1),
        "Grizzly Bears".to_string(),
        Zone::Stack,
    );
    {
        let instant = CardType {
            core_types: vec![CoreType::Instant],
            ..Default::default()
        };
        let obj = state.objects.get_mut(&noncreature_spell).unwrap();
        obj.card_types = instant.clone();
        obj.base_card_types = instant;
    }
    {
        let creature = CardType {
            core_types: vec![CoreType::Creature],
            ..Default::default()
        };
        let obj = state.objects.get_mut(&creature_spell).unwrap();
        obj.card_types = creature.clone();
        obj.base_card_types = creature;
    }
    for (id, card_id) in [
        (noncreature_spell, CardId(20)),
        (creature_spell, CardId(21)),
    ] {
        state.stack.push_back(StackEntry {
            id,
            source_id: id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id,
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
    }

    (
        state,
        activated,
        triggered,
        noncreature_spell,
        creature_spell,
    )
}

/// Issue #408 — `find_legal_targets` (the path `apply` uses at target
/// declaration) must offer the activated ability, the triggered ability, and
/// the noncreature spell, and must REJECT the creature spell.
#[test]
fn louisoix_counter_target_legality() {
    let Some(db) = load_db() else {
        return;
    };

    let filter = louisoix_counter_target(db);
    let (state, activated, triggered, noncreature_spell, creature_spell) =
        stack_with_four_entries();

    // Louisoix's Sacrifice is itself a spell on the stack; its controller is
    // P0. Use a fresh object id as the counter source.
    let source = ObjectId(1000);
    let legal = find_legal_targets(&state, &filter, PlayerId(0), source);

    let is_legal = |id: ObjectId| legal.contains(&TargetRef::Object(id));

    assert!(
        is_legal(activated),
        "an activated ability on the stack must be a legal target"
    );
    assert!(
        is_legal(triggered),
        "a triggered ability on the stack must be a legal target"
    );
    assert!(
        is_legal(noncreature_spell),
        "a noncreature spell must be a legal target"
    );
    assert!(
        !is_legal(creature_spell),
        "a CREATURE spell must NOT be a legal target — the noncreature \
         restriction must be enforced at target-legality time"
    );
}

/// Drive `counter::resolve` for each legal target type — the runtime must
/// actually counter an activated ability, a triggered ability, and a
/// noncreature spell (CR 701.6a). The countered spell goes to its owner's
/// graveyard; countered abilities just leave the stack.
#[test]
fn louisoix_counter_resolves_each_legal_target() {
    let Some(db) = load_db() else {
        return;
    };

    let filter = louisoix_counter_target(db);

    // --- Counter an activated ability ---
    {
        let (mut state, activated, _, _, _) = stack_with_four_entries();
        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: filter.clone(),
                source_rider: None,
            },
            vec![TargetRef::Object(activated)],
            ObjectId(1000),
            PlayerId(0),
        );
        let mut events = Vec::new();
        engine::game::effects::counter::resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            !state.stack.iter().any(|e| e.id == activated),
            "the activated ability must be removed from the stack"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::SpellCountered { .. })),
            "a SpellCountered event must be emitted for the countered ability"
        );
    }

    // --- Counter a triggered ability ---
    {
        let (mut state, _, triggered, _, _) = stack_with_four_entries();
        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: filter.clone(),
                source_rider: None,
            },
            vec![TargetRef::Object(triggered)],
            ObjectId(1000),
            PlayerId(0),
        );
        let mut events = Vec::new();
        engine::game::effects::counter::resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            !state.stack.iter().any(|e| e.id == triggered),
            "the triggered ability must be removed from the stack"
        );
    }

    // --- Counter a noncreature spell (CR 701.6a: → owner's graveyard) ---
    {
        let (mut state, _, _, noncreature_spell, _) = stack_with_four_entries();
        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: filter.clone(),
                source_rider: None,
            },
            vec![TargetRef::Object(noncreature_spell)],
            ObjectId(1000),
            PlayerId(0),
        );
        let mut events = Vec::new();
        engine::game::effects::counter::resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            !state.stack.iter().any(|e| e.id == noncreature_spell),
            "the noncreature spell must be removed from the stack"
        );
        assert!(
            state.players[1].graveyard.contains(&noncreature_spell),
            "CR 701.6a: a countered spell goes to its owner's graveyard"
        );
    }
}

/// Regression guard — a plain "Counter target spell" (Counterspell) must still
/// yield a stack-spell filter; the new combinator must not steal the simple
/// case or change Counterspell's behavior.
#[test]
fn counterspell_still_targets_spells_only() {
    let Some(db) = load_db() else {
        return;
    };

    let face = db
        .get_face_by_name("Counterspell")
        .expect("Counterspell should be in the card database");
    let counter_effect = face
        .abilities
        .iter()
        .find(|a| matches!(a.kind, AbilityKind::Spell))
        .map(|a| a.effect.as_ref())
        .expect("Counterspell should have a spell ability");

    let Effect::Counter { target, .. } = counter_effect else {
        panic!("Counterspell should parse a Counter effect, got {counter_effect:?}");
    };

    // Build a stack with one creature spell and one activated ability. Assert
    // Counterspell CAN target the creature spell — "Counter target spell" has
    // no type restriction, so a creature spell is a legal target (this
    // distinguishes it from Louisoix's noncreature) — and CANNOT target the
    // ability, since "Counter target spell" has no ability disjunct.
    let (state, activated, _, _, creature_spell) = stack_with_four_entries();
    let legal = find_legal_targets(&state, target, PlayerId(0), ObjectId(1000));
    assert!(
        legal.contains(&TargetRef::Object(creature_spell)),
        "Counterspell (\"Counter target spell\") must still be able to \
         target a creature spell — got legal set {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Object(activated)),
        "Counterspell must not target activated abilities — got legal set {legal:?}"
    );
}
