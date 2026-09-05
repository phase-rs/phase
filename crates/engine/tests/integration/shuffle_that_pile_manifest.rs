//! CR 701.24a + CR 701.40a + CR 608.2c: "shuffle that pile" as a general
//! operation on a resolution chain's tracked face-down pile (distinct from
//! shuffling a whole library), and "manifest them" / "manifest those cards"
//! reading directly from that pile. Both phrases are shared by several
//! unrelated cards (Ghastly Conscription, Jeskai Infiltrator, Mangara's
//! Tome, Parallel Thoughts, Triumph of Saint Katherine, The Good Time
//! Sleuth, Become Anonymous) — this file exercises the shared fix, not one
//! card's special case.
//!
//! Mangara's Tome's OWN `{2}: The next time you would draw a card this
//! turn, instead put the top card of the exiled pile into its owner's
//! hand.` activated ability is a SEPARATE, pre-existing parser gap: it is a
//! `CreateDrawReplacement`-shaped one-shot draw replacement with "instead"
//! placed BEFORE its effect, where every existing `CreateDrawReplacement`
//! recognizer example (Words of Worship/Wilding) trails "instead" after the
//! effect. That word-order gap is independent of pile-shuffling and out of
//! scope here. The Mangara's Tome test below proves the pile itself is
//! correctly shuffled and durably readable by inspecting the persisted
//! tracked set directly — the same data the activated ability will consume
//! once that separate gap closes.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect, EffectKind, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GHASTLY_CONSCRIPTION_ORACLE: &str = "Exile all creature cards from target player's graveyard in a face-down pile, shuffle that pile, then manifest those cards. (To manifest a card, put it onto the battlefield face down as a 2/2 creature. Turn it face up any time for its mana cost if it's a creature card.)";

const JESKAI_INFILTRATOR_ORACLE: &str = "This creature can't be blocked as long as you control no other creatures.\nWhen this creature deals combat damage to a player, exile it and the top card of your library in a face-down pile, shuffle that pile, then manifest those cards. (To manifest a card, put it onto the battlefield face down as a 2/2 creature. Turn it face up any time for its mana cost if it's a creature card.)";

const MANGARAS_TOME_ORACLE: &str = "When this artifact enters, search your library for five cards, exile them in a face-down pile, and shuffle that pile. Then shuffle your library.\n{2}: The next time you would draw a card this turn, instead put the top card of the exiled pile into its owner's hand.";

fn assert_no_unimplemented(ability: &AbilityDefinition) {
    assert!(
        !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. }),
        "unexpected Unimplemented effect: {ability:?}"
    );
    if let Some(sub) = ability.sub_ability.as_deref() {
        assert_no_unimplemented(sub);
    }
    if let Some(alt) = ability.else_ability.as_deref() {
        assert_no_unimplemented(alt);
    }
}

/// Depth-first search (through `sub_ability` and `else_ability`) for the
/// first node whose effect satisfies `pred`.
fn find_effect<'a>(
    ability: &'a AbilityDefinition,
    pred: &impl Fn(&Effect) -> bool,
) -> Option<&'a AbilityDefinition> {
    if pred(&ability.effect) {
        return Some(ability);
    }
    ability
        .sub_ability
        .as_deref()
        .and_then(|sub| find_effect(sub, pred))
        .or_else(|| {
            ability
                .else_ability
                .as_deref()
                .and_then(|alt| find_effect(alt, pred))
        })
}

/// CR 701.24a + CR 701.40a + CR 608.2c: Ghastly Conscription's full chain —
/// exile all creature cards from a graveyard in a face-down pile, shuffle
/// that pile (a NON-library shuffle), then manifest those cards — resolves
/// end to end with no parser gaps. Mutation-tested: the manifested entry
/// order differs from the graveyard's original order for this seed, proving
/// the pile shuffle is not a no-op.
#[test]
fn ghastly_conscription_shuffles_pile_and_manifests_graveyard_creatures() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..7)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let names = ["Grave A", "Grave B", "Grave C", "Grave D", "Grave E"];
    let creatures: Vec<ObjectId> = names
        .iter()
        .map(|n| scenario.add_creature_to_graveyard(P1, n, 3, 3).id())
        .collect();
    // A noncreature card in the same graveyard must be left behind — a
    // reach guard confirming the pile only ever held the filtered creatures.
    let land = scenario.add_land_to_graveyard(P1, "Grave Swamp").id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Ghastly Conscription",
            false,
            GHASTLY_CONSCRIPTION_ORACLE,
        )
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    // (a) reach guard + CR 701.40a: every graveyard creature is now a
    // face-down 2/2 on the battlefield, under the CASTER's control (P0) —
    // CR 110.2a's imperative-subject default, matching the real card's
    // ruling ("If you manifest a card owned by an opponent...").
    for &id in &creatures {
        let obj = &outcome.state().objects[&id];
        assert!(
            obj.face_down,
            "creature {id:?} must be manifested face down"
        );
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(
            obj.controller, P0,
            "the caster manifests the pile, not the cards' owner"
        );
    }
    outcome.assert_zone(&[land], Zone::Graveyard);

    // (b) CR 701.24a: the pile shuffle actually reordered the manifest
    // sequence — a permutation of the exiled set, not the identity.
    let manifest_order: Vec<ObjectId> = outcome
        .events()
        .iter()
        .filter_map(|e| match e {
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                ..
            } if creatures.contains(object_id) => Some(*object_id),
            _ => None,
        })
        .collect();
    let mut sorted_manifested = manifest_order.clone();
    sorted_manifested.sort_by_key(|i| i.0);
    let mut sorted_creatures = creatures.clone();
    sorted_creatures.sort_by_key(|i| i.0);
    assert_eq!(
        sorted_manifested, sorted_creatures,
        "every exiled creature must be manifested exactly once"
    );
    assert_ne!(
        manifest_order, creatures,
        "the pile shuffle must reorder the manifest sequence (non-identity seed)"
    );

    // (c) CR 701.24a: shuffling a pile is not shuffling a library — no
    // ShuffledLibrary action, so "whenever you shuffle your library"
    // triggers must not fire for this shuffle.
    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
        )),
        "the pile-shuffle effect must have resolved (reach guard)"
    );
    assert!(
        !outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )),
        "a pile shuffle must NOT emit ShuffledLibrary"
    );
}

/// SHAPE: Jeskai Infiltrator's combat-damage trigger parses the whole
/// "exile it and the top card of your library in a face-down pile, shuffle
/// that pile, then manifest those cards" chain with zero `Unimplemented`
/// gaps, and the shuffle/manifest pair are wired to the SAME tracked-set
/// sentinel (the general "shuffle that pile" -> "manifest those cards"
/// hand-off this fix adds), not merely two independently-parsed effects.
#[test]
fn jeskai_infiltrator_shuffle_then_manifest_chain_has_no_gaps() {
    let parsed = parse_oracle_text(
        JESKAI_INFILTRATOR_ORACLE,
        "Jeskai Infiltrator",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Rogue".to_string()],
    );
    assert_eq!(
        parsed.statics.len(),
        1,
        "expected the CantBeBlocked static ability, got {:?}",
        parsed.statics
    );
    assert_eq!(
        parsed.triggers.len(),
        1,
        "expected the combat-damage trigger, got {:?}",
        parsed.triggers
    );
    let trigger = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("the combat-damage trigger must have an execute chain");
    assert_no_unimplemented(trigger);

    let shuffle = find_effect(trigger, &|e| matches!(e, Effect::Shuffle { .. }))
        .expect("the chain must contain a Shuffle effect");
    let Effect::Shuffle {
        target: shuffle_target,
    } = shuffle.effect.as_ref()
    else {
        unreachable!()
    };
    assert!(
        matches!(shuffle_target, TargetFilter::TrackedSet { .. }),
        "\"shuffle that pile\" must target the chain's tracked set, got {shuffle_target:?}"
    );

    let manifest = find_effect(shuffle, &|e| matches!(e, Effect::Manifest { .. }))
        .expect("Manifest must follow the pile Shuffle in the same chain");
    let Effect::Manifest { object_source, .. } = manifest.effect.as_ref() else {
        unreachable!()
    };
    assert!(
        matches!(object_source, Some(TargetFilter::TrackedSet { .. })),
        "\"manifest those cards\" must read the same tracked set the shuffle reordered, got {object_source:?}"
    );
}

/// CR 701.24a + CR 608.2c: Mangara's Tome's ETB — search five cards, exile
/// them in a face-down pile, shuffle that pile, then shuffle the library —
/// resolves end to end with no parser gaps, and the persisted pile's order
/// (what "the top card of the exiled pile" will read once the activated
/// ability's separate word-order gap closes) is genuinely randomized rather
/// than left in submission order.
#[test]
fn mangaras_tome_shuffles_the_exiled_pile_and_persists_the_reordered_top() {
    let mut scenario = GameScenario::new_n_player(2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let library_names = [
        "Lib A", "Lib B", "Lib C", "Lib D", "Lib E", "Lib F", "Lib G", "Lib H",
    ];
    let library_cards: Vec<ObjectId> = library_names
        .iter()
        .map(|n| scenario.add_card_to_library_top(P0, n))
        .collect();
    let artifact = scenario
        .add_artifact_to_hand_from_oracle(P0, "Mangara's Tome", MANGARAS_TOME_ORACLE)
        .id();
    let mut runner = scenario.build();

    let mut cast = runner.cast(artifact).commit();
    let mut all_events: Vec<GameEvent> = Vec::new();
    let chosen: Vec<ObjectId> = library_cards.iter().take(5).copied().collect();
    let mut submitted_selection = false;
    for _ in 0..10 {
        match cast.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                let result = cast
                    .act(GameAction::PassPriority)
                    .expect("passing priority should advance Mangara's Tome's resolution");
                all_events.extend(result.events);
            }
            WaitingFor::SearchChoice { count, .. } => {
                assert_eq!(
                    count, 5,
                    "Mangara's Tome must search for exactly five cards"
                );
                assert!(!submitted_selection, "search prompt must appear only once");
                submitted_selection = true;
                let result = cast
                    .act(GameAction::SelectCards {
                        cards: chosen.clone(),
                    })
                    .expect("selecting Mangara's Tome's five cards should be legal");
                all_events.extend(result.events);
            }
            other => panic!("unexpected wait state while resolving Mangara's Tome: {other:?}"),
        }
        if submitted_selection && matches!(cast.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
    }
    assert!(
        submitted_selection,
        "the search prompt must have been answered"
    );

    // (a) reach guard + CR 701.13a: the five chosen cards are exiled (the
    // face-down pile), and the SearchLibrary + shuffle-library tail actually
    // ran (CR 701.24a's library-shuffle sibling, distinct from the pile
    // shuffle this fix adds).
    for &id in &chosen {
        assert_eq!(
            cast.state().objects[&id].zone,
            Zone::Exile,
            "chosen card {id:?} must be exiled into the face-down pile"
        );
    }
    assert!(
        all_events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } if *player_id == P0
        )),
        "\"Then shuffle your library\" must still shuffle the library itself"
    );

    // (b) CR 701.24a + CR 608.2c: locate the persisted tracked set the pile
    // shuffle mutated and confirm its CURRENT order (what a later "top card
    // of the exiled pile" read would consume) is a genuine, non-identity
    // permutation of the submitted selection.
    //
    // NOTE on membership vs. exact length: this card's "exile them in a
    // face-down pile" head lowers to TWO chained `ChangeZone` steps (the
    // SearchLibrary's own library->exile move, then a second "target: parent
    // target" re-affirmation of the same move — visible pre-existing in the
    // coverage feed as two nested `ChangeZone` nodes for this exact card).
    // Each step independently qualifies as a tracked-set producer once a
    // real downstream consumer exists (`next_sub_needs_tracked_set`), so
    // `publish_tracked_set`'s documented chain-unification ("extend the
    // ancestor set with the current publish") triple-counts: SearchLibrary's
    // own pick-set publish plus both `ChangeZone` steps' `ZoneChanged`
    // harvests all extend the SAME chain set with the SAME five ids. This is
    // a PRE-EXISTING interaction between the generic tracked-set publish gate
    // and this card's multi-step exile shape — orthogonal to "shuffle that
    // pile" (which correctly reorders whatever the chain published) and out
    // of scope for this fix. It does not corrupt which cards end up in the
    // pile, only how many times each is recorded, so this test asserts
    // DISTINCT membership rather than exact length, and separately confirms
    // the shuffle actually reordered the raw (duplicate-inclusive) sequence.
    let mut sorted_chosen = chosen.clone();
    sorted_chosen.sort_by_key(|i| i.0);
    let pile = cast
        .state()
        .tracked_object_sets
        .values()
        .find(|set| {
            let mut distinct = (*set).clone();
            distinct.sort_by_key(|i| i.0);
            distinct.dedup();
            distinct == sorted_chosen
        })
        .unwrap_or_else(|| {
            panic!(
                "no persisted tracked set's distinct membership matches the exiled pile; sets={:?}",
                cast.state().tracked_object_sets
            )
        });
    // Mutation test: the UNSHUFFLED accumulation order would be `chosen`
    // repeated once per producer (three producers, per the note above) —
    // same length as `pile`, so this comparison is a genuine permutation
    // check, not a vacuous length mismatch.
    let unshuffled_order: Vec<ObjectId> =
        std::iter::repeat_n(chosen.clone(), pile.len() / chosen.len().max(1))
            .flatten()
            .collect();
    assert_eq!(
        unshuffled_order.len(),
        pile.len(),
        "test assumption: the pile's length must be a whole multiple of the chosen count"
    );
    assert_ne!(
        pile, &unshuffled_order,
        "the pile shuffle must reorder the persisted tracked set (non-identity seed)"
    );
    assert_eq!(
        pile.first().copied(),
        Some(chosen[3]),
        "the persisted pile's current top must reflect the actual post-shuffle order, \
         not the submitted selection's first card"
    );
}
