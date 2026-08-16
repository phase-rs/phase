//! Discriminating runtime regression for **End-Blaze Epiphany** (std
//! impulse-exile batch) — the delayed-trigger body:
//!
//! > exile a number of cards from the top of your library equal to its power,
//! > then choose a card exiled this way. Until the end of your next turn, you
//! > may play that card.
//!
//! "then choose a card exiled this way" is the impulse-exile choose anaphor over
//! the chain's tracked set (the cards exiled by the preceding `ChangeZone
//! Library->Exile`). It must lower to `Effect::ChooseFromZone { count: 1, zone:
//! Exile, selection: Chosen }`, which raises an interactive
//! `WaitingFor::ChooseFromZoneChoice` answered by `GameAction::SelectCards`. The
//! trailing "you may play that card" then grants `PlayFromExile` to the chosen
//! card only (`TargetFilter::TrackedSet`, narrowed by the choose).
//!
//! Before the fix the bare "choose a card exiled this way" clause matched no
//! anaphor combinator (`parse_choose_anaphoric` only handles "N of them/those")
//! and fell to `Effect::Unimplemented` — no `ChooseFromZoneChoice` prompt was
//! raised, the tracked set was never narrowed to one card, and no play
//! permission landed on a single chosen card.
//!
//! The test seeds the chain tracked set with the two exiled-this-way cards (the
//! upstream "exile a number of cards equal to its power" exile clause — a
//! separate, pre-existing parser gap on the *exile count*, outside this
//! impulse-exile-choose batch — does not currently populate the set, so seeding
//! it isolates the behavior THIS change owns: the choose anaphor). It then
//! drives the SAME production path the runtime uses for the choose -> grant
//! sub-chain: `resolve_ability_chain` -> `WaitingFor::ChooseFromZoneChoice` ->
//! `engine::game::apply(SelectCards)` -> `drain_pending_continuation` ->
//! `GrantCastingPermission`.
//!
//! DISCRIMINATOR: after seeding two exiled cards and answering the prompt with
//! ONE of them, exactly that one card carries P0's `PlayFromExile` permission;
//! the unchosen exiled card carries none. With the choose reverted to
//! `Unimplemented`, no `ChooseFromZoneChoice` is raised at all — the
//! `WaitingFor` match panics, and the single-card permission assertion can never
//! hold.
//!
//! CR 608.2c: the controller follows the spell's instructions in order.
//! CR 603.7: End-Blaze's "when that creature dies" is a delayed triggered ability.
//! CR 400.7i: the controller may play the exiled cards.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, CardSelectionMode, CastingPermission, Effect, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId, TrackedSetId};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const END_BLAZE_TRIGGER_BODY: &str = "exile a number of cards from the top of your library equal \
to its power, then choose a card exiled this way. Until the end of your next turn, you may play \
that card.";

#[test]
fn end_blaze_epiphany_grants_play_permission_to_chosen_exiled_card_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario.add_creature(P0, "End-Blaze Epiphany", 0, 0).id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    // The two cards that were "exiled this way" (already in exile, as the
    // upstream exile clause would have moved them). A third card in exile that
    // was NOT exiled this way must never be offered or granted.
    let exiled_a = create_object(state, CardId(900), P0, "Exiled A".to_string(), Zone::Exile);
    let exiled_b = create_object(state, CardId(901), P0, "Exiled B".to_string(), Zone::Exile);
    let unrelated = create_object(state, CardId(902), P0, "Unrelated".to_string(), Zone::Exile);

    // Seed the chain tracked set with exactly the two exiled-this-way cards —
    // the set the preceding ChangeZone(Library->Exile) publishes at runtime.
    state
        .tracked_object_sets
        .insert(TrackedSetId(1), vec![exiled_a, exiled_b]);
    state.next_tracked_set_id = 2;
    state.chain_tracked_set_id = Some(TrackedSetId(1));

    // Parser path the real card uses; the choose -> grant sub-chain is the part
    // this change owns.
    let def = parse_effect_chain(END_BLAZE_TRIGGER_BODY, AbilityKind::Spell);
    let choose = def
        .sub_ability
        .clone()
        .expect("End-Blaze body chains exile -> choose");

    // "choose a card exiled this way" must lower to a chosen ChooseFromZone over
    // the tracked exile set (count 1, no extra filter — the tracked set IS the
    // referent).
    assert!(
        matches!(
            &*choose.effect,
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                selection: CardSelectionMode::Chosen,
                filter: None,
                ..
            }
        ),
        "\"choose a card exiled this way\" must lower to a chosen ChooseFromZone \
         over the tracked exile set, got {:?}",
        choose.effect
    );
    // The trailing "you may play that card" grants PlayFromExile to the tracked
    // (chosen) card.
    let grant = choose
        .sub_ability
        .as_ref()
        .expect("choose chains into the play-permission grant");
    assert!(
        matches!(
            &*grant.effect,
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile { .. },
                target: TargetFilter::TrackedSet { .. },
                ..
            }
        ),
        "trailing clause must grant PlayFromExile to the tracked (chosen) card, got {:?}",
        grant.effect
    );

    // Resolve the choose -> grant sub-chain through the production resolver.
    let ability = build_resolved_from_def(&choose, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("choose-then-grant chain must resolve to a prompt");

    // PRODUCTION STEP: the choose raises an interactive ChooseFromZoneChoice.
    let offered = match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the controller makes the choose");
            cards.clone()
        }
        other => panic!(
            "\"choose a card exiled this way\" must raise ChooseFromZoneChoice; \
             reverting the parse yields Unimplemented and a different WaitingFor: {other:?}"
        ),
    };

    // The offered set is exactly the two exiled-this-way cards — the unrelated
    // exile entry was never tracked, so it cannot be offered.
    assert!(
        offered.contains(&exiled_a) && offered.contains(&exiled_b),
        "both exiled-this-way cards must be offered, got {offered:?}"
    );
    assert!(
        !offered.contains(&unrelated),
        "a card not exiled this way must not be offered"
    );
    assert_eq!(offered.len(), 2, "only the two tracked cards are choosable");

    // PRODUCTION STEP: answer with ONE card through the same handler
    // `GameRunner::act` calls.
    let chosen: ObjectId = exiled_a;
    engine::game::apply(
        runner.state_mut(),
        P0,
        GameAction::SelectCards {
            cards: vec![chosen],
        },
    )
    .expect("selecting one offered card must be a legal answer");

    // DISCRIMINATOR: the chosen card carries P0's PlayFromExile permission.
    let chosen_perms = &runner.state().objects[&chosen].casting_permissions;
    assert!(
        chosen_perms.iter().any(|perm| matches!(
            perm,
            CastingPermission::PlayFromExile { granted_to: P0, .. }
        )),
        "the chosen exiled-this-way card must carry P0's PlayFromExile permission, got {chosen_perms:?}"
    );

    // NEGATIVE: the unchosen exiled card receives NO play permission — the grant
    // bound to the narrowed tracked set (one card), not the whole exile window.
    let unchosen = exiled_b;
    assert!(
        runner.state().objects[&unchosen]
            .casting_permissions
            .iter()
            .all(|perm| !matches!(
                perm,
                CastingPermission::PlayFromExile { granted_to: P0, .. }
            )),
        "the unchosen exiled card must NOT carry the play permission"
    );
}
