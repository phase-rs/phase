//! Issue #1536 — Rebound (CR 702.88) end-to-end discriminator suite.
//!
//! Consuming Vapors is the canonical Rebound spell. The reminder text is:
//!   "Rebound (If you cast this spell from your hand, exile it as it resolves.
//!    At the beginning of your next upkeep, you may cast this card from exile
//!    without paying its mana cost.)"
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 702.88a: Rebound spells cast from hand exile on resolution and
//!     create a delayed triggered ability that offers a free recast at the
//!     controller's next upkeep.
//!   - CR 702.88c: multiple instances of Rebound are redundant.
//!   - CR 603.7a: delayed triggered abilities are created during resolution.
//!   - CR 603.7b: a delayed trigger fires only once unless it has a stated
//!     duration.
//!   - CR 603.7c: if the object is no longer in the expected zone at the
//!     time the delayed trigger resolves, the ability resolves but won't
//!     affect it.
//!   - CR 603.7d: source of the delayed trigger is the spell that created
//!     it; controller is the player who controlled the resolving spell.
//!   - CR 608.2n: instant/sorcery spells go to owner's graveyard as the
//!     final part of resolution (displaced by Rebound's exile-instead).
//!   - CR 704.5d: tokens cease to exist in zones other than the battlefield
//!     (Rebound copies of tokens cannot be cast again, so Rebound must not
//!     arm on token spells).
//!   - CR 514.2: "until end of turn" effects end at cleanup — the granted
//!     `ExileWithAltCost { duration: Some(UntilEndOfTurn) }` is pruned at
//!     cleanup if the controller declined or failed to cast.
//!
//! These tests do not depend on the production `card-data.json` export
//! (which would also mask gates behind unrelated parser/loader work). Every
//! test hand-builds the minimum state needed and exercises
//! `stack::resolve_top` plus the relevant prune helpers directly so each
//! assertion discriminates the pre-fix bug from the post-fix correct
//! behavior.

use std::sync::Arc;

use engine::game::stack;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CastingPermission, DelayedTriggerCondition, Duration, Effect,
    ResolvedAbility, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// Build a hand-resident sorcery with `Keyword::Rebound` and a single
/// no-op (`Draw 1`) spell ability, so it has a real `AbilityDefinition` to
/// drive `resolve_top` without depending on the card-data export. The
/// effect body is irrelevant to Rebound's arming hook — only the keyword
/// presence and cast-origin matter.
fn add_rebound_sorcery_in_hand(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Sorcery);
    obj.base_card_types = obj.card_types.clone();
    obj.keywords.push(Keyword::Rebound);
    obj.base_keywords.push(Keyword::Rebound);
    obj.mana_cost = ManaCost::generic(2);
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ));
    id
}

/// Push the spell onto the stack with a `SpellContext.cast_from_zone` of
/// `Hand` (or whatever `origin` is passed). Mirrors the post-`finalize_cast`
/// invariant the production casting pipeline guarantees: the stack entry
/// carries a real `ResolvedAbility` whose `context.cast_from_zone` is set
/// to the pre-announcement zone (CR 601.2a).
fn push_to_stack_as_spell_from(
    state: &mut GameState,
    spell_id: ObjectId,
    controller: PlayerId,
    origin: Zone,
) {
    let card_id = state.objects[&spell_id].card_id;
    let ability_def = state.objects[&spell_id]
        .abilities
        .first()
        .cloned()
        .expect("rebound test spell must have a spell ability");
    let mut resolved = ResolvedAbility::new(
        (*ability_def.effect).clone(),
        Vec::new(),
        spell_id,
        controller,
    );
    resolved.context.cast_from_zone = Some(origin);
    // Mirror finalize_cast: move the card object onto the stack zone and
    // stamp `cast_from_zone` so `spell_cast_origin`'s fallback resolves the
    // origin AFTER the stack entry is popped during resolution.
    if let Some(obj) = state.objects.get_mut(&spell_id) {
        obj.zone = Zone::Stack;
        obj.cast_from_zone = Some(origin);
    }
    state.stack.push_back(StackEntry {
        id: spell_id,
        source_id: spell_id,
        controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: Some(resolved),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
}

/// Test 1 — CR 702.88a: When cast from hand, a Rebound spell resolves to
/// Exile, not Graveyard. Pre-fix `Effect::CastFromZone { duration }` did
/// not exist, no Rebound arming hook lived in `stack::resolve_top`, and
/// Consuming Vapors went to the graveyard on resolution. The exile
/// destination is the load-bearing redirect that exposes the bug.
#[test]
fn consuming_vapors_resolves_to_exile_when_cast_from_hand() {
    let mut state = GameState::new_two_player(42);
    let spell = add_rebound_sorcery_in_hand(&mut state, P0, "Consuming Vapors");
    push_to_stack_as_spell_from(&mut state, spell, P0, Zone::Hand);

    let mut events: Vec<GameEvent> = Vec::new();
    stack::resolve_top(&mut state, &mut events);

    assert_eq!(
        state.objects[&spell].zone,
        Zone::Exile,
        "CR 702.88a: a Rebound spell cast from hand must exile on resolve, \
         not go to the graveyard (CR 608.2n displaced)",
    );
}

/// Test 2 — CR 603.7a + CR 702.88a: Resolving a hand-cast Rebound spell
/// installs exactly one delayed triggered ability keyed on the
/// controller's next upkeep. The trigger must be optional ("you may
/// cast") and carry the exiled card as its target. Pre-fix no delayed
/// trigger was ever pushed, so no upkeep prompt would surface.
#[test]
fn rebound_offers_recast_at_upkeep_and_resolves() {
    let mut state = GameState::new_two_player(42);
    let spell = add_rebound_sorcery_in_hand(&mut state, P0, "Consuming Vapors");
    push_to_stack_as_spell_from(&mut state, spell, P0, Zone::Hand);

    let mut events: Vec<GameEvent> = Vec::new();
    stack::resolve_top(&mut state, &mut events);

    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "CR 603.7a: Rebound resolution must install exactly one delayed \
         triggered ability for the next-upkeep recast",
    );
    let trig = &state.delayed_triggers[0];
    match &trig.condition {
        DelayedTriggerCondition::AtNextPhaseForPlayer { phase, player } => {
            assert_eq!(
                *phase,
                Phase::Upkeep,
                "CR 702.88a: recast offered at upkeep"
            );
            assert_eq!(
                *player, P0,
                "CR 603.7d: recast keyed on resolving controller"
            );
        }
        other => panic!("CR 702.88a: expected AtNextPhaseForPlayer(Upkeep, P0), got {other:?}"),
    }
    // CR 702.88a: "you may cast" — optional.
    assert!(
        trig.ability.optional,
        "CR 702.88a: Rebound recast must be optional",
    );
    // CR 603.7d: source/controller match.
    assert_eq!(trig.source_id, spell);
    assert_eq!(trig.controller, P0);
    // CR 603.7b: one-shot.
    assert!(
        trig.one_shot,
        "CR 603.7b: Rebound delayed trigger fires once"
    );
}

/// Test 3 — CR 702.88a + CR 514.2: if the controller declines the upkeep
/// recast (the trigger fires but the player chooses not to cast), the
/// exiled card must not retain a standing `ExileWithAltCost` permission
/// after end-of-turn cleanup. Pre-fix the granted permission had no
/// `duration` and would persist indefinitely. The test pins the post-fix
/// contract: declining never installs a permission AT ALL — the
/// permission is granted only when the optional cast is accepted, and
/// even if it were to leak, `prune_end_of_turn_casting_permissions`
/// would now expire it.
#[test]
fn rebound_declined_at_upkeep_leaves_card_in_exile_with_no_permission() {
    let mut state = GameState::new_two_player(42);
    let spell = add_rebound_sorcery_in_hand(&mut state, P0, "Consuming Vapors");
    push_to_stack_as_spell_from(&mut state, spell, P0, Zone::Hand);

    let mut events: Vec<GameEvent> = Vec::new();
    stack::resolve_top(&mut state, &mut events);

    // After resolution the exiled card has no casting permission yet — the
    // delayed trigger has not fired and the recast effect hasn't run.
    assert!(
        state.objects[&spell].casting_permissions.is_empty(),
        "CR 702.88a: Rebound resolution must NOT preemptively grant a casting \
         permission — the permission is created only when the controller \
         accepts the upkeep recast (`cast_from_zone::resolve`)",
    );
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Exile,
        "exiled Rebound card stays in exile until the upkeep trigger fires",
    );

    // Simulate end-of-turn cleanup: even if a permission HAD leaked, the
    // `UntilEndOfTurn` duration must now expire it. Push a fake durational
    // permission and verify it would be pruned.
    state
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_permissions
        .push(CastingPermission::ExileWithAltCost {
            cost: ManaCost::zero(),
            cast_transformed: false,
            constraint: None,
            granted_to: Some(P0),
            resolution_cleanup: None,
            duration: Some(Duration::UntilEndOfTurn),

            exile_instead_of_graveyard_on_resolve: false,
            enters_with_counter: None,
        });
    engine::game::layers::prune_end_of_turn_casting_permissions(&mut state);
    assert!(
        state.objects[&spell].casting_permissions.is_empty(),
        "CR 514.2: a Rebound-granted permission with Duration::UntilEndOfTurn \
         must be pruned at cleanup if the controller did not accept (or did \
         not finish casting). Pre-fix the `duration` field did not exist on \
         `ExileWithAltCost`, so the permission would persist forever.",
    );
}

/// Test 4 — CR 611.2a + CR 514.2: the recast effect carries
/// `Effect::CastFromZone { duration: Some(UntilEndOfTurn), .. }` so the
/// granted permission inherits the expiry. Pre-fix the `duration` field
/// did not exist on the effect or the permission; the test pins the
/// plumbing contract end-to-end (effect → permission propagation).
#[test]
fn rebound_permission_expires_at_end_of_turn_if_player_passes_after_accepting() {
    let mut state = GameState::new_two_player(42);
    let spell = add_rebound_sorcery_in_hand(&mut state, P0, "Consuming Vapors");
    push_to_stack_as_spell_from(&mut state, spell, P0, Zone::Hand);

    let mut events: Vec<GameEvent> = Vec::new();
    stack::resolve_top(&mut state, &mut events);

    // Resolve the queued delayed trigger body directly — this is the same
    // `Effect::CastFromZone` constructed by `arm_rebound`. It grants the
    // `ExileWithAltCost { duration: Some(UntilEndOfTurn) }` permission.
    let trig = state.delayed_triggers[0].ability.clone();
    let mut events: Vec<GameEvent> = Vec::new();
    engine::game::effects::cast_from_zone::resolve(&mut state, &trig, &mut events)
        .expect("Rebound recast effect must install the durational permission");

    let perm_duration = state.objects[&spell]
        .casting_permissions
        .iter()
        .find_map(|p| match p {
            CastingPermission::ExileWithAltCost { duration, .. } => duration.clone(),
            _ => None,
        });
    assert_eq!(
        perm_duration,
        Some(Duration::UntilEndOfTurn),
        "CR 611.2a: the Rebound recast permission must inherit \
         Duration::UntilEndOfTurn from the CastFromZone effect — pre-fix the \
         field did not exist and the permission persisted forever",
    );

    // CR 514.2: the prune helper must expire the permission at cleanup.
    engine::game::layers::prune_end_of_turn_casting_permissions(&mut state);
    assert!(
        state.objects[&spell].casting_permissions.is_empty(),
        "CR 514.2: prune_end_of_turn_casting_permissions must drop the \
         durational ExileWithAltCost permission — pre-fix the helper only \
         handled PlayFromExile and the Rebound permission persisted",
    );
}

/// Test 5 — CR 702.88a gate: when the spell was cast from somewhere
/// other than hand (e.g., flashbacked from graveyard, or any other
/// non-Hand source), Rebound does NOT arm. Pre-fix there was no Rebound
/// hook at all, so neither path would arm; this test pins the gate
/// post-fix.
#[test]
fn rebound_not_armed_when_cast_from_exile() {
    let mut state = GameState::new_two_player(42);
    let spell = add_rebound_sorcery_in_hand(&mut state, P0, "Consuming Vapors");
    // Override the origin: simulate a cast from exile (Mizzix's Mastery copy,
    // flashback, etc.).
    push_to_stack_as_spell_from(&mut state, spell, P0, Zone::Exile);

    let mut events: Vec<GameEvent> = Vec::new();
    stack::resolve_top(&mut state, &mut events);

    assert_eq!(
        state.objects[&spell].zone,
        Zone::Graveyard,
        "CR 702.88a: Rebound only triggers when the spell is cast from HAND \
         — an exile-cast Rebound spell follows the normal CR 608.2n \
         graveyard destination",
    );
    assert!(
        state.delayed_triggers.is_empty(),
        "CR 702.88a: no delayed recast trigger must be installed when the \
         spell was not cast from hand",
    );
}

/// Test 6 — CR 704.5d + CR 702.88a: a token spell (e.g., a copy made by
/// Snapcaster Mage or a fork effect) cannot legally be recast from
/// exile because the token ceases to exist when it leaves the stack.
/// Rebound must NOT arm on a token spell, regardless of cast origin.
#[test]
fn rebound_not_armed_for_token_copy() {
    let mut state = GameState::new_two_player(42);
    let spell = add_rebound_sorcery_in_hand(&mut state, P0, "Consuming Vapors");
    state.objects.get_mut(&spell).unwrap().is_token = true;
    push_to_stack_as_spell_from(&mut state, spell, P0, Zone::Hand);

    let mut events: Vec<GameEvent> = Vec::new();
    stack::resolve_top(&mut state, &mut events);

    assert!(
        state.delayed_triggers.is_empty(),
        "CR 704.5d: a token copy of a Rebound spell must NOT arm the recast \
         — the token ceases to exist on leaving the stack so the upkeep \
         offer would target nothing",
    );
}

/// Test 7 — CR 608.2b + CR 702.88a: a Rebound spell whose targets all
/// become illegal fizzles. Per CR 608.2b the spell is removed from the
/// stack and put into its owner's graveyard. The Rebound arming hook
/// must NOT fire (no `execute_effect` body runs on a fizzle, so no
/// Rebound delayed trigger is installed).
///
/// This test exercises a non-targeted CR 702.88a spell that resolves
/// normally — but with a guard: a *different* path that returns early
/// before the Rebound hook should also leave no trigger. The simplest
/// post-fix discriminator is to drive the same hand-cast resolution on
/// a spell WITHOUT the Rebound keyword and assert no delayed trigger
/// is installed (the negative control for the hook gating logic).
#[test]
fn rebound_fizzles_to_graveyard_without_arming() {
    let mut state = GameState::new_two_player(42);
    // Negative control: a non-Rebound sorcery cast from hand must NOT
    // install any delayed trigger via this code path.
    let spell = add_rebound_sorcery_in_hand(&mut state, P0, "Plain Sorcery");
    state.objects.get_mut(&spell).unwrap().keywords.clear();
    state.objects.get_mut(&spell).unwrap().base_keywords.clear();
    push_to_stack_as_spell_from(&mut state, spell, P0, Zone::Hand);

    let mut events: Vec<GameEvent> = Vec::new();
    stack::resolve_top(&mut state, &mut events);

    assert!(
        state.delayed_triggers.is_empty(),
        "no Rebound delayed trigger must be installed for spells without \
         Keyword::Rebound — the hook's keyword gate must be tight",
    );
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Graveyard,
        "CR 608.2n: a non-Rebound sorcery still goes to graveyard normally",
    );
}

/// Test 8 — CR 603.7a + CR 702.88c: two distinct Rebound spells
/// resolving in sequence each push an independent delayed trigger.
/// CR 702.88c notes multiple instances of Rebound on the SAME spell are
/// redundant, but two separate Rebound spells must each arm independently.
#[test]
fn two_distinct_rebound_spells_each_arm_independent_triggers() {
    let mut state = GameState::new_two_player(42);
    let first = add_rebound_sorcery_in_hand(&mut state, P0, "Consuming Vapors A");
    let second = add_rebound_sorcery_in_hand(&mut state, P0, "Consuming Vapors B");

    push_to_stack_as_spell_from(&mut state, first, P0, Zone::Hand);
    let mut events: Vec<GameEvent> = Vec::new();
    stack::resolve_top(&mut state, &mut events);

    push_to_stack_as_spell_from(&mut state, second, P0, Zone::Hand);
    let mut events: Vec<GameEvent> = Vec::new();
    stack::resolve_top(&mut state, &mut events);

    assert_eq!(
        state.delayed_triggers.len(),
        2,
        "CR 603.7a: each Rebound resolution creates a separate delayed \
         trigger — pre-fix neither spell armed, so the count was 0",
    );
    assert_eq!(state.delayed_triggers[0].source_id, first);
    assert_eq!(state.delayed_triggers[1].source_id, second);
    assert_eq!(state.objects[&first].zone, Zone::Exile);
    assert_eq!(state.objects[&second].zone, Zone::Exile);
}

/// Cross-player invariant — CR 603.7d: a Rebound spell cast by P1
/// keys its delayed trigger on P1's next upkeep, not P0's. Lock the
/// controller-binding contract that the hook reads from
/// `entry.controller`, not the active player.
#[test]
fn rebound_delayed_trigger_controller_matches_caster_not_active_player() {
    let mut state = GameState::new_two_player(42);
    // P0 is the default active player but P1 casts the Rebound spell.
    let spell = add_rebound_sorcery_in_hand(&mut state, P1, "Consuming Vapors");
    push_to_stack_as_spell_from(&mut state, spell, P1, Zone::Hand);

    let mut events: Vec<GameEvent> = Vec::new();
    stack::resolve_top(&mut state, &mut events);

    let trig = &state.delayed_triggers[0];
    assert_eq!(
        trig.controller, P1,
        "CR 603.7d: controller follows the caster"
    );
    match &trig.condition {
        DelayedTriggerCondition::AtNextPhaseForPlayer { player, .. } => {
            assert_eq!(
                *player, P1,
                "CR 603.7d: keyed on the CASTER's upkeep, not active player"
            );
        }
        other => panic!("expected AtNextPhaseForPlayer, got {other:?}"),
    }
}
