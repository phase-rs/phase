//! Koh, the Face Stealer — "Koh has all activated and triggered abilities of the
//! last chosen card." (CR 613.1f Layer-6 ability grant; CR 602.1 activated +
//! CR 603.1 triggered; CR 611.2c live-tracking; CR 400.7 zone-change invalidation.)
//!
//! Exercises the four new building blocks end-to-end against the REAL layer
//! evaluation and trigger pipeline:
//!   * `ContinuousModification::GrantAllTriggeredAbilitiesOf { ChosenCard }` →
//!     `expand_granted_triggered_abilities` → `GrantTrigger` on the recipient.
//!   * `ContinuousModification::GrantAllActivatedAbilitiesOf { ChosenCard }`.
//!   * `TargetFilter::ChosenCard` (reads the source's `ChosenAttribute::Card`,
//!     guarded by `zone == Exile`).
//!   * `Effect::RememberCard` (replace-on-rechoose writer).
//!
//! Lead conditions:
//!   #4 — a granted TRIGGERED ability must actually FIRE for Koh (not merely be
//!        present): `granted_upkeep_trigger_fires_for_koh` advances to Koh's
//!        controller's upkeep through the real runner and asserts the granted
//!        "gain 2 life" trigger resolved.
//!   #3 — DUAL invalidation:
//!        `grant_drops_when_chosen_card_leaves_exile` (the chosen card leaving
//!        exile drops the grant — CR 400.7) and
//!        `rechoose_overwrites_so_only_newest_card_is_granted` (re-choosing via the
//!        real `Effect::RememberCard` writer replaces, never accumulates).

use std::sync::Arc;

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::GameRunner;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ChosenAttribute, ContinuousModification, Effect,
    ManaContribution, ManaProduction, StaticDefinition, TargetFilter, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{ExileLink, ExileLinkKind, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// Koh's two grant statics, exactly as the parser lowers them (verified by the
/// parse-shape test below): all activated AND all triggered abilities of the last
/// chosen card. Affects Koh itself (`SelfRef`).
fn koh_grant_statics() -> Vec<StaticDefinition> {
    vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![
            ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: TargetFilter::ChosenCard,
                cap: None,
            },
            ContinuousModification::GrantAllTriggeredAbilitiesOf {
                source: TargetFilter::ChosenCard,
            },
        ])]
}

/// Place Koh on the battlefield under P0 with the grant statics, P0 holding
/// priority in their precombat main phase.
fn build_koh(state: &mut GameState) -> ObjectId {
    let koh = create_object(
        state,
        CardId(1000),
        P0,
        "Koh, the Face Stealer".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&koh).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    obj.static_definitions = koh_grant_statics().into();
    koh
}

/// A creature card sitting in exile, carrying `triggers` (printed) and
/// `abilities` (printed). Not yet chosen — chosen via `ChosenAttribute::Card`.
fn build_exiled_creature(
    state: &mut GameState,
    card_id: u64,
    triggers: Vec<TriggerDefinition>,
    abilities: Vec<AbilityDefinition>,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        format!("Exiled Face {card_id}"),
        Zone::Exile,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    obj.base_trigger_definitions = Arc::new(triggers.clone());
    obj.trigger_definitions = triggers.into();
    obj.abilities = Arc::new(abilities);
    id
}

/// Record `card` as Koh's last chosen card (what `Effect::RememberCard` writes).
fn set_chosen(state: &mut GameState, koh: ObjectId, card: ObjectId) {
    let obj = state.objects.get_mut(&koh).unwrap();
    obj.chosen_attributes
        .retain(|a| !matches!(a, ChosenAttribute::Card(_)));
    obj.chosen_attributes.push(ChosenAttribute::Card(card));
}

/// Parse a single triggered ability from oracle text (for the firing test's
/// granted trigger — real parser shape, not a hand-built `TriggerDefinition`).
fn trigger_from_oracle(oracle: &str) -> TriggerDefinition {
    let parsed = parse_oracle_text(
        oracle,
        "Probe Face",
        &[],
        &["Creature".to_string()],
        &["Shapeshifter".to_string()],
    );
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "probe trigger oracle must parse cleanly: {oracle}"
    );
    parsed
        .triggers
        .into_iter()
        .next()
        .expect("oracle must yield one triggered ability")
}

/// A free, non-tap mana ability of a given color — a donatable activated ability
/// with an observable, color-distinct effect.
fn mana_ability(color: ManaColor) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![color],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
}

fn koh_granted_mana_colors(state: &GameState, koh: ObjectId) -> Vec<ManaColor> {
    state.objects[&koh]
        .abilities
        .iter()
        .filter_map(|a| match a.effect.as_ref() {
            Effect::Mana {
                produced: ManaProduction::Fixed { colors, .. },
                ..
            } => Some(colors.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn koh_has_upkeep_trigger(state: &GameState, koh: ObjectId) -> bool {
    state.objects[&koh]
        .trigger_definitions
        .iter_unchecked()
        .any(|t| matches!(t.mode, TriggerMode::Phase) && t.phase == Some(Phase::Upkeep))
}

// ─── Condition #4: a granted TRIGGERED ability actually FIRES for Koh ─────────

/// CR 613.1f + CR 603.1: the chosen exiled card's "at the beginning of your
/// upkeep, you gain 2 life" trigger is granted to Koh and FIRES on P0's upkeep,
/// resolving through the real runner. The discriminator: with NO chosen card the
/// trigger is absent and no life is gained (asserted in the same test).
#[test]
fn granted_upkeep_trigger_fires_for_koh() {
    let mut state = GameState::new_two_player(7);
    state.phase = Phase::Untap;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let koh = build_koh(&mut state);
    let upkeep_trigger = trigger_from_oracle("At the beginning of your upkeep, you gain 2 life.");
    let face = build_exiled_creature(&mut state, 2000, vec![upkeep_trigger], vec![]);

    // Negative baseline: nothing chosen yet → grant absent.
    evaluate_layers(&mut state);
    assert!(
        !koh_has_upkeep_trigger(&state, koh),
        "with no last-chosen card, Koh must NOT have the granted upkeep trigger"
    );

    // Choose the exiled face → the grant materializes the trigger onto Koh.
    set_chosen(&mut state, koh, face);
    evaluate_layers(&mut state);
    assert!(
        koh_has_upkeep_trigger(&state, koh),
        "after choosing the exiled face, Koh must be granted its upkeep trigger \
         (indexed by Koh as the holding object)"
    );

    let mut runner = GameRunner::from_state(state);
    let life_before = runner.life(P0);

    // Advance to P0's upkeep through the real runner → the granted trigger fires.
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "the granted upkeep trigger must FIRE for Koh and resolve (gain 2 life); \
         present-but-not-firing would leave life unchanged"
    );
}

// ─── Condition #3a: chosen card leaving exile drops the grant (CR 400.7) ──────

/// CR 400.7 + CR 611.2c: `TargetFilter::ChosenCard` is live and zone-guarded —
/// once the chosen card is no longer in exile, the grant drops on the next layer
/// pass (the stored id no longer matches an exiled object).
#[test]
fn grant_drops_when_chosen_card_leaves_exile() {
    let mut state = GameState::new_two_player(7);
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let koh = build_koh(&mut state);
    let face = build_exiled_creature(
        &mut state,
        2000,
        vec![],
        vec![mana_ability(ManaColor::Green)],
    );
    set_chosen(&mut state, koh, face);

    evaluate_layers(&mut state);
    assert_eq!(
        koh_granted_mana_colors(&state, koh),
        vec![ManaColor::Green],
        "while the chosen face is in exile, Koh is granted its Green mana ability"
    );

    // The chosen card leaves exile (e.g., it is put into a graveyard). CR 400.7:
    // it is a new object; the stored id no longer names an exiled object.
    state.objects.get_mut(&face).unwrap().zone = Zone::Graveyard;
    if let Some(p) = state.players.iter_mut().find(|p| p.id == P0) {
        p.graveyard.push_back(face);
    }

    evaluate_layers(&mut state);
    assert!(
        koh_granted_mana_colors(&state, koh).is_empty(),
        "once the chosen card leaves exile the grant must drop (zone-guarded \
         ChosenCard no longer matches)"
    );
}

// ─── Condition #3b: re-choose overwrites — only the newest card is granted ────

/// CR 608.2c "the last chosen card": the real `Effect::RememberCard` writer is
/// replace-on-rechoose (retain-then-push a single `ChosenAttribute::Card`), so
/// after choosing a second face Koh is granted ONLY the newest face's abilities,
/// never the union of both.
#[test]
fn rechoose_overwrites_so_only_newest_card_is_granted() {
    let mut state = GameState::new_two_player(7);
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let koh = build_koh(&mut state);
    let face_g = build_exiled_creature(
        &mut state,
        2000,
        vec![],
        vec![mana_ability(ManaColor::Green)],
    );
    let face_r =
        build_exiled_creature(&mut state, 3000, vec![], vec![mana_ability(ManaColor::Red)]);

    // First choice → Green, via the real RememberCard writer (SpecificObject
    // target resolves the pick directly, no tracked-set plumbing needed).
    resolve_remember_card(&mut state, koh, face_g);
    evaluate_layers(&mut state);
    assert_eq!(
        koh_granted_mana_colors(&state, koh),
        vec![ManaColor::Green],
        "after choosing the Green face, Koh has exactly its Green ability"
    );

    // Re-choose → Red. Must REPLACE, not accumulate.
    resolve_remember_card(&mut state, koh, face_r);
    let card_attrs = state.objects[&koh]
        .chosen_attributes
        .iter()
        .filter(|a| matches!(a, ChosenAttribute::Card(_)))
        .count();
    assert_eq!(
        card_attrs, 1,
        "RememberCard must keep exactly ONE Card attribute (replace-on-rechoose)"
    );

    evaluate_layers(&mut state);
    assert_eq!(
        koh_granted_mana_colors(&state, koh),
        vec![ManaColor::Red],
        "after re-choosing the Red face, Koh has ONLY the Red ability — not the \
         cumulative {{Green, Red}} (which a non-replacing writer would produce)"
    );
}

/// Resolve `Effect::RememberCard` through the real resolver, recording `card` onto
/// `koh`. Targets the card directly (`SpecificObject`) so the writer's
/// retain-then-push is exercised without tracked-set setup.
fn resolve_remember_card(state: &mut GameState, koh: ObjectId, card: ObjectId) {
    let def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RememberCard {
            target: TargetFilter::SpecificObject { id: card },
        },
    );
    let ability = build_resolved_from_def(&def, koh, P0);
    let mut events = Vec::new();
    resolve_ability_chain(state, &ability, &mut events, 0).expect("RememberCard must resolve");
}

// ─── Maintainer HIGH (PR #4611): Koh can choose OPPONENT-owned exiled cards ────

/// CR 400.1 + CR 607.2a (PR #4611, maintainer matthewevans): Koh exiles
/// *opponents'* creatures, so "Choose a creature card exiled with Koh" must
/// offer opponent-OWNED exiled cards. The activated ability lowers to
/// `ChooseFromZone { zone_owner: AllOwners, filter: And[Creature, ExiledBySource] }`,
/// which scans the whole shared exile zone (CR 400.1) and lets the linked-exile
/// reference (CR 607.2a) do all scoping — ownership is irrelevant. Driven
/// end-to-end through the REAL parse→resolve→select path: the parsed activated
/// ability parks a `ChooseFromZoneChoice` offering the opponent's exiled
/// creature; selecting it runs the `RememberCard` sub-ability, which records it
/// as Koh's `ChosenAttribute::Card` (the seam `TargetFilter::ChosenCard` and the
/// Layer-6 grant statics read).
///
/// DISCRIMINATOR: the ONLY exiled creature is opponent-owned. Under the old
/// `ZoneOwner::Controller` scope the per-owner gate empties the candidate pool,
/// so NO prompt parks and NO card is recorded — both the prompt-parks assertion
/// and the chosen-card assertion below flip to failure (the revert-probe).
#[test]
fn koh_can_choose_opponent_owned_creature_exiled_with_koh() {
    const KOH: &str = "When Koh enters, exile up to one other target creature.\nWhenever another nontoken creature dies, you may exile it.\nPay 1 life: Choose a creature card exiled with Koh.\nKoh has all activated and triggered abilities of the last chosen card.";
    let opponent = PlayerId(1);

    let mut state = GameState::new_two_player(7);
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let koh = build_koh(&mut state);

    // An OPPONENT-owned creature card in the shared exile zone (CR 400.1),
    // exiled WITH Koh — the common case, since Koh exiles opponents' creatures.
    let opp_face = create_object(
        &mut state,
        CardId(7000),
        opponent,
        "Opponent's Exiled Face".to_string(),
        Zone::Exile,
    );
    {
        let obj = state.objects.get_mut(&opp_face).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        // A donatable activated ability with an observable, color-distinct
        // effect — what Koh's Layer-6 grant should surface once this opponent's
        // card is the last chosen card.
        obj.abilities = Arc::new(vec![mana_ability(ManaColor::Green)]);
    }
    // CR 607.2a: link the opponent-owned card to Koh as "exiled with" it.
    state.exile_links.push(ExileLink {
        exiled_id: opp_face,
        source_id: koh,
        kind: ExileLinkKind::TrackedBySource,
    });

    // Negative baseline: with nothing chosen yet, Koh is granted no ability —
    // the discriminator for the end-to-end grant assertion below.
    evaluate_layers(&mut state);
    assert!(
        koh_granted_mana_colors(&state, koh).is_empty(),
        "before any card is chosen, Koh must have no granted ability"
    );

    // Real parser path: Koh's activated ability (ChooseFromZone + RememberCard sub).
    let parsed = parse_oracle_text(
        KOH,
        "Koh, the Face Stealer",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Shapeshifter".to_string()],
    );
    let activated = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("Koh's activated ability must parse")
        .clone();

    // Resolve the activated ability through the real chain → parks on the choice.
    let ability = build_resolved_from_def(&activated, koh, P0);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0)
        .expect("Koh's choose-from-exile must resolve");

    // The interactive prompt parked and OFFERS the opponent-owned card. Under the
    // old Controller scope the pool is empty and `waiting_for` would not be a
    // ChooseFromZoneChoice at all — this match arm is the revert-probe failure.
    match &state.waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "Koh's controller chooses");
            assert!(
                cards.contains(&opp_face),
                "the opponent-owned creature exiled with Koh must be offered, got {cards:?}"
            );
        }
        other => panic!(
            "expected ChooseFromZoneChoice offering the opponent-owned exiled creature; \
             got {other:?} (a Controller-scope owner gate would empty the pool)"
        ),
    }

    // Select the opponent's card → the RememberCard sub-ability records it.
    engine::game::engine::apply(
        &mut state,
        P0,
        GameAction::SelectCards {
            cards: vec![opp_face],
        },
    )
    .expect("selecting the opponent-owned exiled creature must succeed");

    // Production seam: Koh now remembers the OPPONENT-owned card as its last
    // chosen card (what `TargetFilter::ChosenCard` / the grant statics read).
    let remembered: Vec<ObjectId> = state.objects[&koh]
        .chosen_attributes
        .iter()
        .filter_map(|a| match a {
            ChosenAttribute::Card(id) => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(
        remembered,
        vec![opp_face],
        "Koh must record the chosen OPPONENT-owned exiled creature as its last \
         chosen card (RememberCard wrote ChosenAttribute::Card across owners)"
    );

    // End-to-end payoff — the user-facing bug: with the opponent-owned card now
    // recorded as Koh's last chosen card, the Layer-6 grant
    // (`TargetFilter::ChosenCard`, owner-agnostic and zone-guarded to Exile)
    // surfaces that card's activated ability ONTO Koh. This is the whole point
    // of Koh choosing opponent-owned exiled creatures: stealing their abilities.
    evaluate_layers(&mut state);
    assert_eq!(
        koh_granted_mana_colors(&state, koh),
        vec![ManaColor::Green],
        "Koh must be granted the OPPONENT-owned chosen card's activated ability \
         (against the empty baseline above — proving cross-owner selection drives \
         the grant end-to-end)"
    );
}

// ─── gap=0 proxy: the whole card lowers with no Unimplemented ─────────────────

/// The full card parses with NO `Unimplemented` clause: the activated ability is a
/// `ChooseFromZone` (from `ExiledBySource`) carrying a `RememberCard` sub-ability,
/// and the static carries BOTH grant modifications sourced from `ChosenCard`.
#[test]
fn full_card_parses_no_unimplemented() {
    const KOH: &str = "When Koh enters, exile up to one other target creature.\nWhenever another nontoken creature dies, you may exile it.\nPay 1 life: Choose a creature card exiled with Koh.\nKoh has all activated and triggered abilities of the last chosen card.";

    let parsed = parse_oracle_text(
        KOH,
        "Koh, the Face Stealer",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Shapeshifter".to_string()],
    );

    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "no clause may remain Unimplemented (gap=0)"
    );

    // Activated: ChooseFromZone(ExiledBySource) + RememberCard sub-ability.
    let activated = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("activated ability must parse");
    assert!(
        matches!(activated.effect.as_ref(), Effect::ChooseFromZone { .. }),
        "activated effect must be ChooseFromZone, got {:?}",
        activated.effect
    );
    let sub = activated
        .sub_ability
        .as_ref()
        .expect("ChooseFromZone must carry a RememberCard sub-ability");
    assert!(
        matches!(sub.effect.as_ref(), Effect::RememberCard { .. }),
        "sub-ability must be RememberCard, got {:?}",
        sub.effect
    );

    // Static: both grant modifications sourced from ChosenCard.
    let grants: Vec<&ContinuousModification> = parsed
        .statics
        .iter()
        .flat_map(|s| s.modifications.iter())
        .collect();
    assert!(
        grants.iter().any(|m| matches!(
            m,
            ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: TargetFilter::ChosenCard,
                ..
            }
        )),
        "static must grant all activated abilities of the chosen card"
    );
    assert!(
        grants.iter().any(|m| matches!(
            m,
            ContinuousModification::GrantAllTriggeredAbilitiesOf {
                source: TargetFilter::ChosenCard,
            }
        )),
        "static must grant all triggered abilities of the chosen card"
    );
}
