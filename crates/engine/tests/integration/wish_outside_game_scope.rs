//! Pre-M10 Wish reach — the legacy rule a custom format can opt back into,
//! exercised through a **parsed card and a real cast**.
//!
//! Today CR 400.11 says "outside the game is not a zone" and CR 400.11a says
//! only a sideboard is outside it, so a Wish cannot see exile. Before M10 there
//! was no exile zone — cards were *removed from the game*, which counted as
//! outside it — so a Wish could retrieve an owned removed card.
//!
//! The unit tests beside `game::wish_scope` cover the pool decision itself.
//! What these add is that the decision is actually reached from Oracle text
//! through `GameAction::CastSpell`: a parser that stopped producing
//! `Effect::SearchOutsideGame` for this wording, or a cast path that never got
//! to the resolver, would leave those unit tests green.
//!
//! Every assertion is paired against the identical board under a modern custom
//! format, so a failure to widen and a failure to set the board up are
//! distinguishable.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::custom_format::{test_rules_with_legacy, LegacyRuleSet, WishOutsideGameScope};
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, OutsideGameChoiceSource, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Burning Wish / Cunning Wish's search clause, which is the class this axis is
/// about: a card you own from outside the game. Verified against Burning Wish's
/// Oracle text ("You may reveal a sorcery card you own from outside the game and
/// put it into your hand. Exile Burning Wish.").
///
/// The self-exile rider is deliberately omitted: it would put the wish itself
/// into the very exile zone these tests measure, so a card carrying it could not
/// assert an exact candidate list. The clause under test is unchanged.
const WISH_ORACLE: &str =
    "Reveal a sorcery card you own from outside the game and put it into your hand.";

fn format_with(scope: WishOutsideGameScope) -> FormatConfig {
    FormatConfig::for_custom_rules(&test_rules_with_legacy(LegacyRuleSet {
        wish_scope: scope,
        ..LegacyRuleSet::default()
    }))
}

/// Seeds a face-up sorcery card in exile, owned by `owner` — the pre-M10
/// "removed from the game" card a wish would have been able to name.
fn exile_sorcery(runner: &mut GameRunner, owner: PlayerId, name: &str) -> ObjectId {
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    let id =
        engine::game::zones::create_object(state, card_id, owner, name.to_string(), Zone::Exile);
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.card_types = CardType {
            core_types: vec![CoreType::Sorcery],
            ..Default::default()
        };
        obj.base_card_types = obj.card_types.clone();
        obj.face_down = false;
    }
    id
}

/// Casts the wish and resolves it, returning the runner parked on whatever the
/// resolution produced.
fn cast_the_wish(runner: &mut GameRunner, wish: ObjectId) {
    let card_id = runner.state().objects[&wish].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: wish,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the wish is castable");
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner
                .act(GameAction::PassPriority)
                .expect("pass to resolve the wish");
        } else {
            break;
        }
    }
}

/// A game in the precombat main phase with a castable wish in P0's hand.
fn game_with_a_wish(scope: WishOutsideGameScope) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wish = scenario
        .add_spell_to_hand_from_oracle(P0, "Burning Wish", false, WISH_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().format_config = format_with(scope);
    (runner, wish)
}

fn offered_exile_ids(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::OutsideGameChoice { choices, .. } => choices
            .iter()
            .filter_map(|choice| match &choice.source {
                OutsideGameChoiceSource::FaceUpExile { object_id } => Some(*object_id),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The axis, end to end from Oracle text: a cast wish reaches exile only under
/// the legacy scope.
#[test]
fn a_cast_wish_reaches_owned_face_up_exile_only_under_the_legacy_scope() {
    let (mut modern, wish) = game_with_a_wish(WishOutsideGameScope::PostM10SideboardOnly);
    let exiled = exile_sorcery(&mut modern, P0, "Removed Ritual");
    cast_the_wish(&mut modern, wish);
    assert!(
        !offered_exile_ids(&modern).contains(&exiled),
        "CR 400.11: exile is an in-game zone, so a modern wish cannot see it — \
         got {:?}",
        modern.state().waiting_for
    );

    let (mut legacy, wish) = game_with_a_wish(WishOutsideGameScope::PreM10ReachesExile);
    let exiled = exile_sorcery(&mut legacy, P0, "Removed Ritual");
    cast_the_wish(&mut legacy, wish);
    assert_eq!(
        offered_exile_ids(&legacy),
        vec![exiled],
        "pre-M10: a removed-from-the-game card the wish's controller owns is \
         outside the game and may be chosen — got {:?}",
        legacy.state().waiting_for
    );
}

/// CR 701.23j: "that player may choose an appropriate card **they own**". The
/// widening changes which pool is consulted, never who may be chosen from it —
/// a card P0 merely CONTROLS in exile is still P1's card.
#[test]
fn a_cast_wish_never_reaches_a_card_the_caster_does_not_own() {
    let (mut runner, wish) = game_with_a_wish(WishOutsideGameScope::PreM10ReachesExile);
    let owned_by_opponent = exile_sorcery(&mut runner, P1, "Opponent's Ritual");
    // Controlled by the caster, owned by the opponent: the case a naive
    // controller-keyed filter would wrongly admit.
    if let Some(obj) = runner.state_mut().objects.get_mut(&owned_by_opponent) {
        obj.controller = P0;
    }
    // A card P0 really does own, so the wish has something to offer and this
    // test cannot pass merely because the search found nothing at all.
    let owned_by_caster = exile_sorcery(&mut runner, P0, "My Ritual");

    cast_the_wish(&mut runner, wish);

    assert_eq!(
        offered_exile_ids(&runner),
        vec![owned_by_caster],
        "only the card the caster OWNS may be offered, whoever controls it — \
         got {:?}",
        runner.state().waiting_for
    );
}
