//! Beam me up (TRK): Marooned's "can't be beamed up" prohibition (CR 101.2 +
//! CR 604.1 + CR 611.3a) and the beam me up graveyard cast route. The keyword
//! is the CR 601.3 permission to cast the card from your graveyard (CR 109.5)
//! for its beam me up cost (an alternative cost, CR 118.9) while also
//! returning a creature you control to its owner's hand (an additional cost,
//! CR 118.8, paid per CR 601.2h; CR 400.3 owner's hand, CR 400.7 new object).
//! The spell is exiled whenever it would leave the stack: on resolution, when
//! countered (CR 701.6a) and when every target is illegal (CR 608.2b). The
//! beam me up rule number needs manual verification. Oracle text verbatim from
//! MTGJSON / Scryfall.

use std::collections::BTreeSet;

use engine::ai_support::legal_actions;
use engine::game::casting::current_casting_variant_choice_options;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::AttachTarget;
use engine::game::keywords::{can_be_beamed_up, effective_beam_me_up_cost};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    DigRestOrder, Effect, FilterProp, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, CastingVariantChoiceOption, PayCostKind, StackEntryKind,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const MAROONED: &str = "Enchant artifact or creature\nWhen this Aura enters, you may pay {3}. If you do, tap enchanted permanent.\nEnchanted permanent doesn't untap during its controller's untap step and can't be beamed up.";
const MALFUNCTION: &str = "Enchant artifact or creature\nWhen this Aura enters, tap enchanted permanent.\nEnchanted permanent doesn't untap during its controller's untap step.";

/// Stage an Aura on the battlefield with its Oracle text run through the
/// production parser (mirrors `attachment_host_reference_untargeted.rs`).
fn aura_on_battlefield(
    sc: &mut GameScenario,
    player: PlayerId,
    name: &str,
    text: &str,
) -> ObjectId {
    sc.add_creature(player, name, 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["enchant"], text)
        .id()
}

/// CR 303.4b: attach `attachment` to `host` and re-derive the board.
fn attach(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&attachment)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host));
    let host_obj = runner.state_mut().objects.get_mut(&host).unwrap();
    if !host_obj.attachments.contains(&attachment) {
        host_obj.attachments.push(attachment);
    }
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

/// Undo `attach`: the Aura is unattached and the host no longer lists it.
fn detach(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&attachment)
        .unwrap()
        .attached_to = None;
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .retain(|id| *id != attachment);
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

/// The beam me up eligibility probe: "creature … that can be beamed up".
fn beamable_creature() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::CanBeBeamedUp]))
}

fn probe_admits(runner: &GameRunner, source: ObjectId, id: ObjectId) -> bool {
    let state = runner.state();
    matches_target_filter(
        state,
        id,
        &beamable_creature(),
        &FilterContext::from_source(state, source),
    )
}

struct Board {
    runner: GameRunner,
    a: ObjectId,
    b: ObjectId,
    aura: ObjectId,
}

/// P0 controls Grizzly Bears (A) and Savannah Lions (B); `aura_controller`
/// controls the Aura (`aura_text`), staged unattached on the battlefield.
fn board(aura_controller: PlayerId, aura_name: &str, aura_text: &str) -> Board {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let a = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = sc.add_creature(P0, "Savannah Lions", 2, 1).id();
    let aura = aura_on_battlefield(&mut sc, aura_controller, aura_name, aura_text);
    let mut runner = sc.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    Board { runner, a, b, aura }
}

fn has_static(runner: &GameRunner, id: ObjectId, mode: &StaticMode) -> bool {
    runner.state().objects[&id]
        .static_definitions
        .iter_all()
        .any(|def| def.mode == *mode)
}

/// P1-R5: on a real Marooned board the reader excludes Marooned's host and admits
/// another creature. An opponent's (P1's) Marooned on P0's creature is the
/// realistic case. Reach control: before `attach`, the same board admits A, so
/// the exclusion comes from the host binding (`EnchantedBy`), not from
/// Marooned's mere presence.
#[test]
fn marooned_host_cannot_be_beamed_up_but_another_creature_can() {
    let Board {
        mut runner,
        a,
        b,
        aura,
    } = board(P1, "Marooned", MAROONED);
    assert!(
        has_static(&runner, aura, &StaticMode::CantBeBeamedUp),
        "REACH: Marooned must carry the parsed CantBeBeamedUp static"
    );

    // Reach control: unattached Marooned names no host.
    assert!(can_be_beamed_up(runner.state(), a));
    assert!(probe_admits(&runner, a, a));

    attach(&mut runner, aura, a);

    assert!(
        !can_be_beamed_up(runner.state(), a),
        "Marooned's host can't be beamed up"
    );
    assert!(
        !probe_admits(&runner, a, a),
        "FilterProp::CanBeBeamedUp must reject Marooned's host"
    );
    // Paired positive: the evaluator runs and answers true for another creature.
    assert!(can_be_beamed_up(runner.state(), b));
    assert!(probe_admits(&runner, a, b));
}

/// P1-R6: the prohibition is keyed on the `CantBeBeamedUp` static, not on the
/// Aura's other static or its attachment. Malfunction carries the identical
/// `CantUntap` on the enchanted permanent but prints no beam me up clause.
#[test]
fn malfunction_host_can_still_be_beamed_up() {
    let Board {
        mut runner,
        a,
        b,
        aura,
    } = board(P1, "Malfunction", MALFUNCTION);
    assert!(
        has_static(&runner, aura, &StaticMode::CantUntap),
        "REACH: Malfunction must carry its parsed CantUntap static"
    );
    assert!(!has_static(&runner, aura, &StaticMode::CantBeBeamedUp));

    attach(&mut runner, aura, a);

    assert!(can_be_beamed_up(runner.state(), a));
    assert!(probe_admits(&runner, a, a));
    assert!(can_be_beamed_up(runner.state(), b));
}

/// P1-R7a (CR 611.3a): the binding is live and source-relative. Moving Marooned
/// from A to B moves the prohibition with it; no latched id survives.
#[test]
fn moving_marooned_moves_the_prohibition() {
    let Board {
        mut runner,
        a,
        b,
        aura,
    } = board(P1, "Marooned", MAROONED);
    attach(&mut runner, aura, a);
    assert!(!can_be_beamed_up(runner.state(), a));
    assert!(can_be_beamed_up(runner.state(), b));

    detach(&mut runner, aura, a);
    attach(&mut runner, aura, b);

    assert!(
        can_be_beamed_up(runner.state(), a),
        "A is no longer enchanted by Marooned"
    );
    assert!(probe_admits(&runner, a, a));
    assert!(
        !can_be_beamed_up(runner.state(), b),
        "Marooned's new host can't be beamed up"
    );
    assert!(!probe_admits(&runner, a, b));
}

/// P1-R7b: Marooned's text has no controller clause, so a Marooned controlled by
/// the host's own controller (P0) still blocks its host.
#[test]
fn own_marooned_still_blocks_its_host() {
    let Board {
        mut runner,
        a,
        b,
        aura,
    } = board(P0, "Marooned", MAROONED);
    attach(&mut runner, aura, a);

    assert!(!can_be_beamed_up(runner.state(), a));
    assert!(!probe_admits(&runner, a, a));
    assert!(can_be_beamed_up(runner.state(), b));
}

const OPEN_COMMUNICATIONS: &str = "Draw a card.\nBeam me up {2}{U} (You may cast this card from your graveyard for {2}{U} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const ENERGIZE: &str = "Return target creature an opponent controls to its owner's hand.\nBeam me up {3}{U} (You may cast this card from your graveyard for {3}{U} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const PURE_ENERGY: &str = "Distribute two +1/+1 counters among one or two target creatures.\nBeam me up {2}{W} (You may cast this card from your graveyard for {2}{W} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const FINAL_FRONTIER: &str = "Search your library for a basic land card or Planet card, put it onto the battlefield tapped, then shuffle.\nBeam me up {1}{G} (You may cast this card from your graveyard for {1}{G} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const ADVERSARY_BY_ALGORITHM: &str = "Create a 3/3 colorless Illusion artifact creature token.\nBeam me up {3}{G} (You may cast this card from your graveyard for {3}{G} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const COFFEE: &str = "Look at the top five cards of your library. You may reveal a Plains card or nonland permanent card with mana value 3 or less from among them and put it into your hand. Put the rest on the bottom in a random order.\nBeam me up {2}{W} (You may cast this card from your graveyard for {2}{W} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const BOARDING_ACTION: &str = "Create a 2/1 red Klingon Warrior creature token with haste.\nBeam me up {2}{R} (You may cast this card from your graveyard for {2}{R} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const FEDERATION_RESUPPLY: &str = "Search your library for an Aura, Equipment, or Spacecraft card, reveal it, put it into your hand, then shuffle.\nBeam me up {3}{W} (You may cast this card from your graveyard for {3}{W} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const PATTERN_BUFFER_PRESERVATION: &str = "Look at the top two cards of your library. Put one of them into your hand and the other into your graveyard.\nBeam me up {1}{B} (You may cast this card from your graveyard for {1}{B} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const TRANSPORTER_MALFUNCTION: &str = "Transporter Malfunction deals 3 damage to any target.\nBeam me up {3}{R}{R}{R} (You may cast this card from your graveyard for {3}{R}{R}{R} if you also return a creature you control to its owner's hand. Then exile this spell.)";
const KESS: &str = "Flying\nOnce during each of your turns, you may cast an instant or sorcery spell from your graveyard. If a spell cast this way would be put into your graveyard, exile it instead.";
const FAMILIARS_RUSE: &str = "As an additional cost to cast this spell, return a creature you control to its owner's hand.\nCounter target spell.";
const COUNTERSPELL: &str = "Counter target spell.";
const LLANOWAR_ELVES: &str = "{T}: Add {G}.";
const GOBLIN_ELECTROMANCER: &str = "Instant and sorcery spells you cast cost {1} less to cast.";
const BIRDS_OF_PARADISE: &str = "Flying\n{T}: Add one mana of any color.";

fn cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

fn mana(color: ManaType, count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(color, ObjectId(0), false, vec![]); count]
}

fn offers_cast_of(runner: &GameRunner, object: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == object),
    )
}

/// Stage a printed beam me up sorcery in `owner`'s graveyard.
fn sorcery_in_graveyard(
    sc: &mut GameScenario,
    owner: PlayerId,
    name: &str,
    printed: ManaCost,
    text: &str,
) -> ObjectId {
    sc.add_spell_to_graveyard(owner, name, false)
        .with_mana_cost(printed)
        .from_oracle_text(text)
        .id()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn pool_total(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].mana_pool.total()
}

fn hand(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .hand
        .iter()
        .copied()
        .collect()
}

fn library(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

fn set(ids: &[ObjectId]) -> BTreeSet<ObjectId> {
    ids.iter().copied().collect()
}

/// Set a library card's printed types after the board is built.
fn retype(
    runner: &mut GameRunner,
    id: ObjectId,
    core: &[CoreType],
    supertypes: &[Supertype],
    subtypes: &[&str],
) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = core.to_vec();
    obj.card_types.supertypes = supertypes.to_vec();
    obj.card_types.subtypes = subtypes.iter().map(|s| s.to_string()).collect();
    obj.base_card_types = obj.card_types.clone();
}

/// What the cast pipeline asked for while `cast_with` drove it.
#[derive(Debug, Default)]
struct CastRecord {
    /// `WaitingFor::PayCost { kind: ReturnToHand }` choices, if raised.
    return_choices: Option<Vec<ObjectId>>,
    /// `WaitingFor::CastingVariantChoice` options, if raised.
    variant_options: Option<Vec<CastingVariantChoiceOption>>,
    /// Whether a target was declared before the return prompt (CR 601.2c
    /// before CR 601.2h).
    targets_before_return: Option<bool>,
}

/// How `cast_with` answers each cast-time prompt.
struct CastIntent<'a> {
    targets: &'a [TargetRef],
    /// The creature returned for a `ReturnToHand` cost prompt.
    returned: Option<ObjectId>,
    distribution: Option<Vec<(TargetRef, u32)>>,
    /// Elects a `CastingVariantChoice` option. `None`: the prompt must not
    /// appear.
    variant: Option<fn(&CastingVariant) -> bool>,
}

impl<'a> CastIntent<'a> {
    fn returning(returned: ObjectId) -> Self {
        CastIntent {
            targets: &[],
            returned: Some(returned),
            distribution: None,
            variant: None,
        }
    }

    fn targeting(mut self, targets: &'a [TargetRef]) -> Self {
        self.targets = targets;
        self
    }
}

/// Submit `GameAction::CastSpell` for `card` and answer every cast-time prompt
/// through `apply()` until the spell is on the stack (CR 601.2a-i).
fn cast_with(runner: &mut GameRunner, card: ObjectId, intent: CastIntent<'_>) -> CastRecord {
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap_or_else(|err| panic!("cast {card:?}: {err:?}"));
    let mut record = CastRecord::default();
    let mut remaining_targets: Vec<TargetRef> = intent.targets.to_vec();
    let mut targeted = false;
    for _ in 0..32 {
        let waiting = runner.state().waiting_for.clone();
        let action = match waiting {
            WaitingFor::CastingVariantChoice { options, .. } => {
                let elect = intent.variant.unwrap_or_else(|| {
                    panic!("unexpected CastingVariantChoice (U-3): {options:?}")
                });
                let index = options
                    .iter()
                    .position(|option| elect(&option.variant))
                    .unwrap_or_else(|| panic!("no elected option in {options:?}"));
                record.variant_options = Some(options);
                GameAction::ChooseCastingVariant { index }
            }
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                // CR 601.2c: one declared target per slot, in written order; an
                // optional slot with nothing declared is left empty.
                let target = match remaining_targets
                    .iter()
                    .position(|t| slot.legal_targets.contains(t))
                {
                    Some(pos) => Some(remaining_targets.remove(pos)),
                    None if slot.optional => None,
                    None => panic!("no declared target is legal: {:?}", slot.legal_targets),
                };
                targeted |= target.is_some();
                GameAction::ChooseTarget { target }
            }
            WaitingFor::MultiTargetSelection { legal_targets, .. } => {
                let cards: Vec<ObjectId> = remaining_targets
                    .iter()
                    .filter_map(|t| match t {
                        TargetRef::Object(id) if legal_targets.contains(id) => Some(*id),
                        _ => None,
                    })
                    .collect();
                remaining_targets.clear();
                targeted = true;
                GameAction::SelectCards { cards }
            }
            WaitingFor::DistributeAmong { .. } => GameAction::DistributeAmong {
                distribution: intent
                    .distribution
                    .clone()
                    .expect("DistributeAmong needs a declared distribution"),
            },
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::ReturnToHand,
                choices,
                count,
                min_count,
                ..
            } => {
                assert_eq!(player, P0, "the caster pays the return");
                assert_eq!((count, min_count), (1, 1), "exactly one creature");
                assert!(
                    record.return_choices.is_none(),
                    "one return prompt per cast"
                );
                record.return_choices = Some(choices.clone());
                record.targets_before_return = Some(targeted);
                let returned = intent
                    .returned
                    .expect("a return prompt needs a declared creature");
                assert!(
                    choices.contains(&returned),
                    "{returned:?} is not a return choice: {choices:?}"
                );
                GameAction::SelectCards {
                    cards: vec![returned],
                }
            }
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::Priority { .. } => return record,
            other => panic!("cast_with does not handle {other:?}"),
        };
        runner
            .act(action)
            .unwrap_or_else(|err| panic!("cast step for {card:?}: {err:?}"));
    }
    panic!("the cast of {card:?} never reached priority");
}

/// Pass priority until the stack is empty or a non-priority prompt is up.
fn pass_until_prompt(runner: &mut GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
    panic!("the stack did not settle: {:?}", runner.state().stack);
}

/// Completion: empty stack and a priority window (card-test).
fn assert_settled(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty(), "stack must be empty");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected priority, got {:?}",
        runner.state().waiting_for
    );
}

fn select(runner: &mut GameRunner, cards: Vec<ObjectId>) {
    runner
        .act(GameAction::SelectCards { cards })
        .expect("selection accepted");
}

/// R1 (CR 601.3 + CR 109.5 + CR 118.3): beam me up is offered from your
/// graveyard only with the beam me up mana and a creature you control that
/// can be beamed up; a hand copy still casts for its printed cost and goes to
/// the graveyard (CR 608.2n). Each negative is the positive board minus one
/// fact.
#[test]
fn beam_me_up_is_offered_from_the_graveyard_only_with_mana_and_a_returnable_creature() {
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum OfferBoard {
        Positive,
        NoCreature,
        ShortMana,
        OpponentsGraveyard,
        MaroonedOnlyCreature,
    }
    fn stage(board: OfferBoard) -> (GameRunner, ObjectId) {
        let mut sc = GameScenario::new();
        sc.at_phase(Phase::PreCombatMain);
        let owner = if board == OfferBoard::OpponentsGraveyard {
            P1
        } else {
            P0
        };
        let card = sorcery_in_graveyard(
            &mut sc,
            owner,
            "Open Communications",
            cost(0, &[ManaCostShard::Blue]),
            OPEN_COMMUNICATIONS,
        );
        let bear = (board != OfferBoard::NoCreature)
            .then(|| sc.add_creature(P0, "Grizzly Bears", 2, 2).id());
        if board == OfferBoard::OpponentsGraveyard {
            sc.add_creature(P1, "Savannah Lions", 2, 1);
        }
        let aura = (board == OfferBoard::MaroonedOnlyCreature)
            .then(|| aura_on_battlefield(&mut sc, P1, "Marooned", MAROONED));
        sc.add_card_to_library_top(P0, "Library Card");
        let blue = if board == OfferBoard::ShortMana { 2 } else { 3 };
        sc.with_mana_pool(P0, mana(ManaType::Blue, blue));
        let mut runner = sc.build();
        if let (Some(aura), Some(bear)) = (aura, bear) {
            attach(&mut runner, aura, bear);
        }
        (runner, card)
    }

    let (runner, card) = stage(OfferBoard::Positive);
    // Reach guard (U-5): the real graveyard object exposes its printed cost
    // through the off-zone keyword read.
    assert_eq!(
        effective_beam_me_up_cost(runner.state(), card),
        Some(cost(2, &[ManaCostShard::Blue])),
        "REACH: effective beam me up cost"
    );
    assert!(
        offers_cast_of(&runner, card),
        "the beam me up cast is offered"
    );

    for board in [
        OfferBoard::NoCreature,
        OfferBoard::ShortMana,
        OfferBoard::OpponentsGraveyard,
        OfferBoard::MaroonedOnlyCreature,
    ] {
        let (runner, card) = stage(board);
        assert!(
            !offers_cast_of(&runner, card),
            "{board:?}: no beam me up cast is offered"
        );
    }

    // Sibling: a hand copy casts for its printed {U} and goes to the graveyard.
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc.add_creature(P0, "Grizzly Bears", 2, 2);
    let in_hand = sc
        .add_spell_to_hand_from_oracle(P0, "Open Communications", false, OPEN_COMMUNICATIONS)
        .with_mana_cost(cost(0, &[ManaCostShard::Blue]))
        .id();
    sc.add_card_to_library_top(P0, "Library Card");
    sc.with_mana_pool(P0, mana(ManaType::Blue, 1));
    let mut runner = sc.build();
    let outcome = runner.cast(in_hand).resolve();
    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "Open Communications draws a card"
    );
    assert_eq!(
        outcome.zone_of(in_hand),
        Zone::Graveyard,
        "a hand cast goes to its owner's graveyard, not exile"
    );
}

/// R2 (CR 118.9 + CR 118.8 + CR 601.2h): the return is a real `PayCost`
/// prompt, the mana paid is the beam me up cost, and the spell is exiled.
#[test]
fn beam_me_up_returns_the_chosen_creature_and_exiles_the_spell() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Open Communications",
        cost(0, &[ManaCostShard::Blue]),
        OPEN_COMMUNICATIONS,
    );
    let a = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = sc.add_creature(P0, "Savannah Lions", 2, 1).id();
    let top = sc.add_card_to_library_top(P0, "Library Card");
    sc.with_mana_pool(P0, mana(ManaType::Blue, 3));
    let mut runner = sc.build();

    let record = cast_with(&mut runner, card, CastIntent::returning(b));
    assert_eq!(
        record.return_choices.as_deref().map(set),
        Some(set(&[a, b])),
        "the return prompt offers each creature P0 controls"
    );
    assert!(record.variant_options.is_none(), "U-3: no method menu");
    assert_eq!(zone(&runner, b), Zone::Hand, "the chosen creature returned");
    assert_eq!(zone(&runner, a), Zone::Battlefield, "the other stays");
    assert_eq!(
        pool_total(&runner, P0),
        0,
        "all three blue paid ({{2}}{{U}}, not the printed {{U}})"
    );

    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        set(&hand(&runner, P0)),
        set(&[b, top]),
        "B returned, one drawn"
    );
    assert_eq!(zone(&runner, card), Zone::Exile, "then exile this spell");
}

/// R3 (CR 400.3): "to its owner's hand" — a creature P0 controls but P1 owns
/// goes to P1's hand.
#[test]
fn beam_me_up_returns_a_stolen_creature_to_its_owners_hand() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Open Communications",
        cost(0, &[ManaCostShard::Blue]),
        OPEN_COMMUNICATIONS,
    );
    let a = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let stolen = sc
        .add_creature(P1, "Savannah Lions", 2, 1)
        .controlled_by(P0)
        .id();
    sc.add_card_to_library_top(P0, "Library Card");
    sc.with_mana_pool(P0, mana(ManaType::Blue, 3));
    let mut runner = sc.build();

    let record = cast_with(&mut runner, card, CastIntent::returning(stolen));
    assert_eq!(
        record.return_choices.as_deref().map(set),
        Some(set(&[a, stolen])),
        "REACH: the stolen creature is controlled by P0, so it is a choice"
    );
    assert!(hand(&runner, P1).contains(&stolen), "owner's hand");
    assert!(
        !hand(&runner, P0).contains(&stolen),
        "not the caster's hand"
    );
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R4 Energize: the opponent's creature returns to its owner's hand.
#[test]
fn energize_cast_with_beam_me_up_bounces_and_is_exiled() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Energize",
        cost(1, &[ManaCostShard::Blue]),
        ENERGIZE,
    );
    let mine = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let theirs = sc.add_creature(P1, "Savannah Lions", 2, 1).id();
    sc.with_mana_pool(P0, mana(ManaType::Blue, 4));
    let mut runner = sc.build();

    let targets = [TargetRef::Object(theirs)];
    let record = cast_with(
        &mut runner,
        card,
        CastIntent::returning(mine).targeting(&targets),
    );
    assert_eq!(record.return_choices.as_deref(), Some(&[mine][..]));
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert!(hand(&runner, P1).contains(&theirs), "bounced to P1's hand");
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R4 Pure Energy: two +1/+1 counters distributed 1 + 1 over two targets.
#[test]
fn pure_energy_cast_with_beam_me_up_distributes_counters_and_is_exiled() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Pure Energy",
        cost(1, &[ManaCostShard::White]),
        PURE_ENERGY,
    );
    let returned = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let first = sc.add_creature(P0, "Savannah Lions", 2, 1).id();
    let second = sc.add_creature(P1, "Elite Vanguard", 2, 1).id();
    sc.with_mana_pool(P0, mana(ManaType::White, 3));
    let mut runner = sc.build();

    let targets = [TargetRef::Object(first), TargetRef::Object(second)];
    let mut intent = CastIntent::returning(returned).targeting(&targets);
    intent.distribution = Some(vec![
        (TargetRef::Object(first), 1),
        (TargetRef::Object(second), 1),
    ]);
    cast_with(&mut runner, card, intent);
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    let p1p1 = |id: ObjectId| {
        runner.state().objects[&id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    };
    assert_eq!((p1p1(first), p1p1(second)), (1, 1), "1 + 1 distributed");
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R4 Final Frontier: a basic land from the library enters tapped, then the
/// library is shuffled.
#[test]
fn final_frontier_cast_with_beam_me_up_fetches_a_tapped_basic_and_is_exiled() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Final Frontier",
        cost(2, &[ManaCostShard::Green]),
        FINAL_FRONTIER,
    );
    let returned = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    for name in ["Filler A", "Filler B", "Filler C"] {
        sc.add_card_to_library_top(P0, name);
    }
    let forest = sc.add_card_to_library_top(P0, "Forest");
    sc.with_mana_pool(P0, mana(ManaType::Green, 2));
    let mut runner = sc.build();
    retype(
        &mut runner,
        forest,
        &[CoreType::Land],
        &[Supertype::Basic],
        &["Forest"],
    );

    cast_with(&mut runner, card, CastIntent::returning(returned));
    pass_until_prompt(&mut runner);
    let WaitingFor::SearchChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected SearchChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(cards, vec![forest], "only the basic land is findable");
    let before = runner.state().rng.get_word_pos();
    select(&mut runner, vec![forest]);
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert_eq!(zone(&runner, forest), Zone::Battlefield);
    assert!(runner.state().objects[&forest].tapped, "enters tapped");
    assert_ne!(
        runner.state().rng.get_word_pos(),
        before,
        "then shuffle consumes entropy"
    );
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R4 Adversary by Algorithm: one 3/3 colorless Illusion artifact creature
/// token.
#[test]
fn adversary_by_algorithm_cast_with_beam_me_up_makes_an_illusion_and_is_exiled() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Adversary by Algorithm",
        cost(2, &[ManaCostShard::Green]),
        ADVERSARY_BY_ALGORITHM,
    );
    let returned = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    sc.with_mana_pool(P0, mana(ManaType::Green, 4));
    let mut runner = sc.build();

    cast_with(&mut runner, card, CastIntent::returning(returned));
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    let tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .map(|id| &runner.state().objects[id])
        .filter(|obj| obj.is_token)
        .collect();
    assert_eq!(tokens.len(), 1, "one token");
    let token = tokens[0];
    assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
    assert!(token.color.is_empty(), "colorless");
    assert!(token.card_types.core_types.contains(&CoreType::Artifact));
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Illusion"));
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R4-Coffee (CR 608.2c + CR 401.4): There's Coffee in that Nebula reveals a
/// Plains card or a nonland permanent card with mana value 3 or less, and puts
/// the rest on the bottom in a random order (CR 101.1: the card's instruction
/// overrides the owner's arrangement).
#[test]
fn coffee_cast_with_beam_me_up_digs_and_randomizes_the_rest() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "There's Coffee in that Nebula",
        cost(1, &[ManaCostShard::White]),
        COFFEE,
    );
    let returned = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // Library, top first: Plains, MV-4 creature, MV-2 creature, Island,
    // sorcery; marker below them.
    let marker = sc.add_card_to_library_top(P0, "Bottom Marker");
    let sorcery = sc
        .add_spell_to_library_top(P0, "Library Sorcery", false)
        .with_mana_cost(cost(1, &[]))
        .id();
    let island = sc
        .add_land_to_library_top(P0, "Island")
        .with_subtypes(vec!["Island"])
        .id();
    let two_drop = sc
        .add_spell_to_library_top(P0, "Two Drop", false)
        .as_creature()
        .with_mana_cost(cost(2, &[]))
        .id();
    let four_drop = sc
        .add_spell_to_library_top(P0, "Four Drop", false)
        .as_creature()
        .with_mana_cost(cost(4, &[]))
        .id();
    let plains = sc
        .add_land_to_library_top(P0, "Plains")
        .with_subtypes(vec!["Plains"])
        .id();
    sc.with_mana_pool(P0, mana(ManaType::White, 3));
    let mut runner = sc.build();
    assert!(
        matches!(
            &*runner.state().objects[&card].abilities[0].effect,
            Effect::Dig {
                rest_order: DigRestOrder::Random,
                ..
            }
        ),
        "the printed rest order is random: {:?}",
        runner.state().objects[&card].abilities[0].effect
    );

    cast_with(&mut runner, card, CastIntent::returning(returned));
    pass_until_prompt(&mut runner);
    let WaitingFor::DigChoice {
        cards,
        selectable_cards,
        rest_order,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("expected DigChoice, got {:?}", runner.state().waiting_for);
    };
    assert_eq!(
        set(&cards),
        set(&[plains, four_drop, two_drop, island, sorcery])
    );
    assert_eq!(set(&selectable_cards), set(&[plains, two_drop]), "filter");
    assert_eq!(rest_order, DigRestOrder::Random, "the rest are randomized");

    let before = runner.state().rng.get_word_pos();
    select(&mut runner, vec![plains]);
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert!(
        runner.state().rng.get_word_pos() > before,
        "a random order consumes entropy"
    );
    assert!(hand(&runner, P0).contains(&plains), "the Plains is in hand");
    let lib = library(&runner, P0);
    assert_eq!(lib.len(), 5);
    assert_eq!(lib[0], marker, "the rest went under the marker");
    assert_eq!(set(&lib[1..]), set(&[four_drop, two_drop, island, sorcery]));
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R4 Boarding Action: one 2/1 red Klingon Warrior token with haste.
#[test]
fn boarding_action_cast_with_beam_me_up_makes_a_klingon_and_is_exiled() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Boarding Action",
        cost(1, &[ManaCostShard::Red]),
        BOARDING_ACTION,
    );
    let returned = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    sc.with_mana_pool(P0, mana(ManaType::Red, 3));
    let mut runner = sc.build();

    cast_with(&mut runner, card, CastIntent::returning(returned));
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    let tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .map(|id| &runner.state().objects[id])
        .filter(|obj| obj.is_token)
        .collect();
    assert_eq!(tokens.len(), 1, "one token");
    let token = tokens[0];
    assert_eq!((token.power, token.toughness), (Some(2), Some(1)));
    assert_eq!(token.color, vec![ManaColor::Red]);
    assert!(token.card_types.subtypes.iter().any(|s| s == "Klingon"));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Warrior"));
    assert!(token.has_keyword(&Keyword::Haste), "haste");
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R4 Federation Resupply: an Equipment card from the library to hand.
#[test]
fn federation_resupply_cast_with_beam_me_up_tutors_equipment_and_is_exiled() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Federation Resupply",
        cost(1, &[ManaCostShard::White]),
        FEDERATION_RESUPPLY,
    );
    let returned = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    for name in ["Filler A", "Filler B"] {
        sc.add_card_to_library_top(P0, name);
    }
    let equipment = sc.add_card_to_library_top(P0, "Bonesplitter");
    sc.with_mana_pool(P0, mana(ManaType::White, 4));
    let mut runner = sc.build();
    retype(
        &mut runner,
        equipment,
        &[CoreType::Artifact],
        &[],
        &["Equipment"],
    );

    cast_with(&mut runner, card, CastIntent::returning(returned));
    pass_until_prompt(&mut runner);
    let WaitingFor::SearchChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected SearchChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(cards, vec![equipment], "only the Equipment is findable");
    select(&mut runner, vec![equipment]);
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert!(hand(&runner, P0).contains(&equipment), "Equipment to hand");
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R4 Pattern Buffer Preservation: of the top two, the chosen one to hand and
/// the other to the graveyard.
#[test]
fn pattern_buffer_preservation_cast_with_beam_me_up_splits_two_and_is_exiled() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Pattern Buffer Preservation",
        cost(1, &[ManaCostShard::Black]),
        PATTERN_BUFFER_PRESERVATION,
    );
    let returned = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    sc.add_card_to_library_top(P0, "Third Card");
    let second = sc.add_card_to_library_top(P0, "Second Card");
    let first = sc.add_card_to_library_top(P0, "First Card");
    sc.with_mana_pool(P0, mana(ManaType::Black, 2));
    let mut runner = sc.build();

    cast_with(&mut runner, card, CastIntent::returning(returned));
    pass_until_prompt(&mut runner);
    let WaitingFor::DigChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!("expected DigChoice, got {:?}", runner.state().waiting_for);
    };
    assert_eq!(set(&cards), set(&[first, second]));
    // Non-identity pick: the second card, not the top one.
    select(&mut runner, vec![second]);
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert_eq!(zone(&runner, second), Zone::Hand);
    assert_eq!(zone(&runner, first), Zone::Graveyard);
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R4 Transporter Malfunction: 3 damage to any target (here P1).
#[test]
fn transporter_malfunction_cast_with_beam_me_up_deals_three_and_is_exiled() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Transporter Malfunction",
        cost(1, &[ManaCostShard::Red]),
        TRANSPORTER_MALFUNCTION,
    );
    let returned = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    sc.with_mana_pool(P0, mana(ManaType::Red, 6));
    let mut runner = sc.build();
    let life = runner.life(P1);

    let targets = [TargetRef::Player(P1)];
    cast_with(
        &mut runner,
        card,
        CastIntent::returning(returned).targeting(&targets),
    );
    assert_eq!(pool_total(&runner, P0), 0, "{{3}}{{R}}{{R}}{{R}} paid");
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert_eq!(runner.life(P1), life - 3);
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R5 (CR 701.6a): a countered beam me up spell is exiled; the same card
/// hand-cast and countered goes to the graveyard, so the exile is bound to the
/// method.
#[test]
fn countered_beam_me_up_spell_is_exiled() {
    fn countered(from_graveyard: bool) -> (GameRunner, ObjectId, i32) {
        let mut sc = GameScenario::new();
        sc.at_phase(Phase::PreCombatMain);
        let printed = cost(1, &[ManaCostShard::Red]);
        let card = if from_graveyard {
            sorcery_in_graveyard(
                &mut sc,
                P0,
                "Transporter Malfunction",
                printed,
                TRANSPORTER_MALFUNCTION,
            )
        } else {
            sc.add_spell_to_hand_from_oracle(
                P0,
                "Transporter Malfunction",
                false,
                TRANSPORTER_MALFUNCTION,
            )
            .with_mana_cost(printed)
            .id()
        };
        let returned = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let counterspell = sc
            .add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL)
            .with_mana_cost(cost(0, &[ManaCostShard::Blue, ManaCostShard::Blue]))
            .id();
        sc.with_mana_pool(P0, mana(ManaType::Red, 6));
        sc.with_mana_pool(P1, mana(ManaType::Blue, 2));
        let mut runner = sc.build();
        let life = runner.life(P1);

        let targets = [TargetRef::Player(P1)];
        let intent = if from_graveyard {
            CastIntent::returning(returned).targeting(&targets)
        } else {
            CastIntent {
                targets: &targets,
                returned: None,
                distribution: None,
                variant: None,
            }
        };
        cast_with(&mut runner, card, intent);
        runner.act(GameAction::PassPriority).expect("P0 passes");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
            "P1 holds priority"
        );
        let spell_target = [TargetRef::Object(card)];
        cast_with(
            &mut runner,
            counterspell,
            CastIntent {
                targets: &spell_target,
                returned: None,
                distribution: None,
                variant: None,
            },
        );
        pass_until_prompt(&mut runner);
        assert_settled(&runner);
        assert_eq!(zone(&runner, counterspell), Zone::Graveyard, "REACH");
        (runner, card, life)
    }

    let (runner, card, life) = countered(true);
    assert_eq!(
        zone(&runner, card),
        Zone::Exile,
        "countered beam me up exiles"
    );
    assert_eq!(runner.life(P1), life, "countered: no damage");

    let (runner, card, life) = countered(false);
    assert_eq!(
        zone(&runner, card),
        Zone::Graveyard,
        "a countered hand cast goes to the graveyard"
    );
    assert_eq!(runner.life(P1), life);
}

/// R6 (CR 601.2c + CR 601.2h + CR 400.7 + CR 608.2b): Pure Energy targets
/// only bear A, then A is returned as the cost; on resolution its only target
/// is illegal, so no counter is placed and the spell is exiled.
#[test]
fn fizzled_beam_me_up_spell_is_exiled() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Pure Energy",
        cost(1, &[ManaCostShard::White]),
        PURE_ENERGY,
    );
    let a = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = sc.add_creature(P0, "Savannah Lions", 2, 1).id();
    sc.with_mana_pool(P0, mana(ManaType::White, 3));
    let mut runner = sc.build();

    let targets = [TargetRef::Object(a)];
    let mut intent = CastIntent::returning(a).targeting(&targets);
    intent.distribution = Some(vec![(TargetRef::Object(a), 2)]);
    let record = cast_with(&mut runner, card, intent);
    assert_eq!(
        record.targets_before_return,
        Some(true),
        "U-2: targets (CR 601.2c) are chosen before the return is paid (CR 601.2h)"
    );
    assert_eq!(zone(&runner, a), Zone::Hand, "REACH: A was returned");
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        runner.state().objects[&b]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "no counters anywhere"
    );
    assert_eq!(
        zone(&runner, card),
        Zone::Exile,
        "fizzled beam me up exiles"
    );
}

/// R7 (CR 601.2b + CR 118.9a): with Kess, Dissident Mage, a graveyard sorcery
/// offers both its beam me up cast and Kess's printed-cost cast; the elected
/// method binds its own cost, prompt and permission slot.
#[test]
fn kess_offers_printed_and_beam_me_up_casts_of_a_graveyard_sorcery() {
    struct KessBoard {
        runner: GameRunner,
        energize: ObjectId,
        open_communications: ObjectId,
        kess: ObjectId,
        bear: ObjectId,
        theirs: ObjectId,
    }
    fn stage() -> KessBoard {
        let mut sc = GameScenario::new();
        sc.at_phase(Phase::PreCombatMain);
        let energize = sorcery_in_graveyard(
            &mut sc,
            P0,
            "Energize",
            cost(1, &[ManaCostShard::Blue]),
            ENERGIZE,
        );
        let open_communications = sorcery_in_graveyard(
            &mut sc,
            P0,
            "Open Communications",
            cost(0, &[ManaCostShard::Blue]),
            OPEN_COMMUNICATIONS,
        );
        let kess = sc
            .add_creature_from_oracle(P0, "Kess, Dissident Mage", 3, 4, KESS)
            .id();
        let bear = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let theirs = sc.add_creature(P1, "Savannah Lions", 2, 1).id();
        let mut pool = mana(ManaType::Blue, 2);
        pool.extend(mana(ManaType::Colorless, 4));
        sc.with_mana_pool(P0, pool);
        KessBoard {
            runner: sc.build(),
            energize,
            open_communications,
            kess,
            bear,
            theirs,
        }
    }
    fn kess_option(options: &[CastingVariantChoiceOption], kess: ObjectId) -> usize {
        options
            .iter()
            .filter(|o| {
                matches!(o.variant, CastingVariant::GraveyardPermission { source, .. } if source == kess)
            })
            .count()
    }

    // Run 1: Kess's printed-cost cast — no return prompt, {1}{U} paid.
    let KessBoard {
        mut runner,
        energize,
        open_communications,
        kess,
        bear,
        theirs,
    } = stage();
    let targets = [TargetRef::Object(theirs)];
    let record = cast_with(
        &mut runner,
        energize,
        CastIntent {
            targets: &targets,
            returned: None,
            distribution: None,
            variant: Some(|v| matches!(v, CastingVariant::GraveyardPermission { .. })),
        },
    );
    let options = record.variant_options.expect("a method menu");
    assert_eq!(options.len(), 2, "two methods: {options:?}");
    let beam: Vec<_> = options
        .iter()
        .filter(|o| o.variant == CastingVariant::BeamMeUp)
        .collect();
    assert_eq!(beam.len(), 1, "one beam me up option");
    assert_eq!(beam[0].mana_cost, cost(3, &[ManaCostShard::Blue]));
    assert_eq!(kess_option(&options, kess), 1, "one Kess option");
    let printed = options
        .iter()
        .find(|o| matches!(o.variant, CastingVariant::GraveyardPermission { .. }))
        .unwrap();
    assert_eq!(printed.mana_cost, cost(1, &[ManaCostShard::Blue]));
    assert!(
        record.return_choices.is_none(),
        "Kess's cast returns nothing"
    );
    assert_eq!(zone(&runner, bear), Zone::Battlefield);
    assert_eq!(pool_total(&runner, P0), 4, "{{1}}{{U}} paid");
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert!(hand(&runner, P1).contains(&theirs), "bounced");
    assert_eq!(zone(&runner, energize), Zone::Exile, "Kess's rider");
    // Kess's once-per-turn slot is spent: the paired negative for run 2.
    assert_eq!(
        kess_option(
            &current_casting_variant_choice_options(runner.state(), P0, open_communications),
            kess
        ),
        0,
        "Kess's slot was used"
    );

    // Run 2: the beam me up cast — the return prompt, {3}{U} paid, Kess's slot
    // untouched.
    let KessBoard {
        mut runner,
        energize,
        open_communications,
        kess,
        bear,
        theirs,
    } = stage();
    let targets = [TargetRef::Object(theirs)];
    let record = cast_with(
        &mut runner,
        energize,
        CastIntent {
            targets: &targets,
            returned: Some(bear),
            distribution: None,
            variant: Some(|v| *v == CastingVariant::BeamMeUp),
        },
    );
    assert_eq!(
        record.return_choices.as_deref().map(set),
        Some(set(&[bear, kess])),
        "the beam me up return prompt"
    );
    assert_eq!(zone(&runner, bear), Zone::Hand);
    assert_eq!(pool_total(&runner, P0), 2, "{{3}}{{U}} paid");
    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert!(hand(&runner, P1).contains(&theirs), "bounced");
    assert_eq!(zone(&runner, energize), Zone::Exile);
    assert_eq!(
        kess_option(
            &current_casting_variant_choice_options(runner.state(), P0, open_communications),
            kess
        ),
        1,
        "the beam me up cast did not use Kess's slot"
    );
}

/// R8 (CR 101.2 + CR 611.3a): Marooned's host is not a beam me up return
/// choice, whoever controls Marooned; unattached, it is. Familiar's Ruse's
/// return-a-creature cost is not beam me up, so it may still return the host.
#[test]
fn marooned_host_is_not_a_beam_me_up_choice() {
    struct MaroonedBoard {
        runner: GameRunner,
        card: ObjectId,
        a: ObjectId,
        b: ObjectId,
    }
    fn stage(aura_controller: PlayerId, attached: bool) -> MaroonedBoard {
        let mut sc = GameScenario::new();
        sc.at_phase(Phase::PreCombatMain);
        let card = sorcery_in_graveyard(
            &mut sc,
            P0,
            "Open Communications",
            cost(0, &[ManaCostShard::Blue]),
            OPEN_COMMUNICATIONS,
        );
        let a = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let b = sc.add_creature(P0, "Savannah Lions", 2, 1).id();
        let aura = aura_on_battlefield(&mut sc, aura_controller, "Marooned", MAROONED);
        sc.add_card_to_library_top(P0, "Library Card");
        sc.with_mana_pool(P0, mana(ManaType::Blue, 3));
        let mut runner = sc.build();
        runner.state_mut().layers_dirty.mark_full();
        evaluate_layers(runner.state_mut());
        if attached {
            attach(&mut runner, aura, a);
        }
        MaroonedBoard { runner, card, a, b }
    }

    // An opponent's Marooned on A: only B can be returned.
    let MaroonedBoard {
        mut runner,
        card,
        a,
        b,
    } = stage(P1, true);
    let record = cast_with(&mut runner, card, CastIntent::returning(b));
    assert_eq!(
        record.return_choices,
        Some(vec![b]),
        "Marooned's host is excluded"
    );
    assert_eq!(zone(&runner, a), Zone::Battlefield);

    // Paired positive: unattached Marooned names no host.
    let MaroonedBoard {
        mut runner,
        card,
        a,
        b,
    } = stage(P1, false);
    let record = cast_with(&mut runner, card, CastIntent::returning(a));
    let choices = set(&record.return_choices.expect("a return prompt"));
    assert!(choices.contains(&a) && choices.contains(&b), "{choices:?}");

    // Multi-authority: P0's own Marooned still blocks its host.
    let MaroonedBoard {
        mut runner,
        card,
        b,
        ..
    } = stage(P0, true);
    let record = cast_with(&mut runner, card, CastIntent::returning(b));
    assert_eq!(
        record.return_choices,
        Some(vec![b]),
        "own Marooned blocks too"
    );

    // Scope hostile: Familiar's Ruse's return cost may return Marooned's host.
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let a = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    sc.add_creature(P0, "Savannah Lions", 2, 1);
    let aura = aura_on_battlefield(&mut sc, P1, "Marooned", MAROONED);
    let open_communications = sc
        .add_spell_to_hand_from_oracle(P0, "Open Communications", false, OPEN_COMMUNICATIONS)
        .with_mana_cost(cost(0, &[ManaCostShard::Blue]))
        .id();
    let ruse = sc
        .add_spell_to_hand_from_oracle(P0, "Familiar's Ruse", true, FAMILIARS_RUSE)
        .with_mana_cost(cost(0, &[ManaCostShard::Blue, ManaCostShard::Blue]))
        .id();
    sc.add_card_to_library_top(P0, "Library Card");
    sc.with_mana_pool(P0, mana(ManaType::Blue, 3));
    let mut runner = sc.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    attach(&mut runner, aura, a);
    assert!(
        !can_be_beamed_up(runner.state(), a),
        "REACH: A is Marooned's host"
    );
    cast_with(
        &mut runner,
        open_communications,
        CastIntent {
            targets: &[],
            returned: None,
            distribution: None,
            variant: None,
        },
    );
    let spell_target = [TargetRef::Object(open_communications)];
    let record = cast_with(
        &mut runner,
        ruse,
        CastIntent::returning(a).targeting(&spell_target),
    );
    assert!(
        record
            .return_choices
            .expect("Familiar's Ruse's return prompt")
            .contains(&a),
        "Familiar's Ruse may return Marooned's host"
    );
    assert_eq!(zone(&runner, a), Zone::Hand);
}

/// The mana actually spent on `spell` while it is on the stack (CR 601.2h).
fn mana_spent_on(runner: &GameRunner, spell: ObjectId) -> u32 {
    runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::Spell {
                actual_mana_spent, ..
            } if entry.id == spell => Some(*actual_mana_spent),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{spell:?} is not a spell on the stack"))
}

fn activate_for_mana(runner: &mut GameRunner, source: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap_or_else(|err| panic!("activate {source:?} for mana: {err:?}"));
}

/// Final Frontier in P0's graveyard with Llanowar Elves and one Forest: the
/// Elf's {G} and the Forest's mana pay {1}{G} only if the Elf can tap for mana
/// before it is returned.
fn final_frontier_with_elf_and_forest() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Final Frontier",
        cost(2, &[ManaCostShard::Green]),
        FINAL_FRONTIER,
    );
    let elf = sc
        .add_creature_from_oracle(P0, "Llanowar Elves", 1, 1, LLANOWAR_ELVES)
        .id();
    let forest = sc.add_basic_land(P0, ManaColor::Green);
    sc.add_card_to_library_top(P0, "Library Card");
    (sc.build(), card, elf, forest)
}

/// R9 (CR 601.2g + CR 601.2h): mana abilities are activated before costs are
/// paid, so the creature returned for beam me up can first tap for mana. The
/// automatic payment taps the Elf and the Forest, then returns the Elf.
#[test]
fn beam_me_up_returned_elf_taps_for_mana_first_on_auto_payment() {
    let (mut runner, card, elf, forest) = final_frontier_with_elf_and_forest();
    assert!(
        offers_cast_of(&runner, card),
        "REACH: the Elf and the Forest can pay {{1}}{{G}}"
    );

    let record = cast_with(&mut runner, card, CastIntent::returning(elf));
    assert_eq!(record.return_choices, Some(vec![elf]));
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast completes: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(mana_spent_on(&runner, card), 2, "{{1}}{{G}} was paid");
    assert_eq!(zone(&runner, elf), Zone::Hand, "then the Elf is returned");
    assert!(runner.state().objects[&forest].tapped, "the Forest paid");
    assert_eq!(pool_total(&runner, P0), 0);
}

/// R9 (CR 601.2g + CR 601.2h): with manual payment the selected Elf stays on
/// the battlefield through the mana-ability window, taps for {G} through the
/// public action, and is returned only when the payment commits.
#[test]
fn beam_me_up_returned_elf_stays_through_the_mana_window() {
    let (mut runner, card, elf, forest) = final_frontier_with_elf_and_forest();
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("cast Final Frontier with beam me up");
    let WaitingFor::PayCost {
        kind: PayCostKind::ReturnToHand,
        choices,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the return prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(choices, vec![elf]);
    select(&mut runner, vec![elf]);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "the mana window follows the selection: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        zone(&runner, elf),
        Zone::Battlefield,
        "the selected Elf stays on the battlefield during the mana window"
    );
    activate_for_mana(&mut runner, elf);
    activate_for_mana(&mut runner, forest);
    assert_eq!(
        pool_total(&runner, P0),
        2,
        "{{G}} from the Elf, one from the Forest"
    );
    runner
        .act(GameAction::PassPriority)
        .expect("the payment commits");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast completes: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(mana_spent_on(&runner, card), 2);
    assert_eq!(
        zone(&runner, elf),
        Zone::Hand,
        "returned at the payment commit"
    );
    assert_eq!(pool_total(&runner, P0), 0);
}

/// R10 (CR 601.2f + CR 601.2g + CR 601.2h): the total cost is locked in with
/// Goblin Electromancer's reduction while the Electromancer chosen for the
/// return is still on the battlefield; it leaves only when the locked {1}{U}
/// is paid, so two Islands pay {2}{U} minus {1}.
#[test]
fn beam_me_up_returned_electromancer_keeps_its_cost_reduction() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Open Communications",
        cost(0, &[ManaCostShard::Blue]),
        OPEN_COMMUNICATIONS,
    );
    let electromancer = sc
        .add_creature_from_oracle(P0, "Goblin Electromancer", 2, 2, GOBLIN_ELECTROMANCER)
        .id();
    let islands = [
        sc.add_basic_land(P0, ManaColor::Blue),
        sc.add_basic_land(P0, ManaColor::Blue),
    ];
    let top = sc.add_card_to_library_top(P0, "Library Card");
    let mut runner = sc.build();
    assert!(
        offers_cast_of(&runner, card),
        "REACH: the reduced {{1}}{{U}} is payable with two Islands"
    );

    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("cast Open Communications with beam me up");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::ReturnToHand,
                ..
            }
        ),
        "expected the return prompt, got {:?}",
        runner.state().waiting_for
    );
    select(&mut runner, vec![electromancer]);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "the mana window follows the selection: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        zone(&runner, electromancer),
        Zone::Battlefield,
        "the Electromancer stays until the locked total is paid"
    );
    assert_eq!(
        runner.state().pending_cast.as_ref().map(|p| p.cost.clone()),
        Some(cost(1, &[ManaCostShard::Blue])),
        "the locked total keeps the Electromancer's {{1}} reduction"
    );
    for island in islands {
        activate_for_mana(&mut runner, island);
    }
    runner
        .act(GameAction::PassPriority)
        .expect("the payment commits");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast completes: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(mana_spent_on(&runner, card), 2, "{{2}}{{U}} minus {{1}}");
    assert_eq!(
        zone(&runner, electromancer),
        Zone::Hand,
        "returned at the commit"
    );

    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        set(&hand(&runner, P0)),
        set(&[electromancer, top]),
        "the Electromancer returned, one card drawn"
    );
    assert_eq!(zone(&runner, card), Zone::Exile);
}

/// R11 (CR 601.2g + CR 601.2h): the class — Familiar's Ruse's printed
/// "return a creature you control" additional cost also waits for the mana
/// window, so Birds of Paradise taps for {U} before it is returned.
#[test]
fn familiars_ruse_returned_birds_taps_for_mana_first() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let open_communications = sc
        .add_spell_to_hand_from_oracle(P0, "Open Communications", false, OPEN_COMMUNICATIONS)
        .with_mana_cost(cost(0, &[ManaCostShard::Blue]))
        .id();
    let ruse = sc
        .add_spell_to_hand_from_oracle(P0, "Familiar's Ruse", true, FAMILIARS_RUSE)
        .with_mana_cost(cost(0, &[ManaCostShard::Blue, ManaCostShard::Blue]))
        .id();
    let birds = sc
        .add_creature_from_oracle(P0, "Birds of Paradise", 0, 1, BIRDS_OF_PARADISE)
        .id();
    let island = sc.add_basic_land(P0, ManaColor::Blue);
    let bears = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    sc.add_card_to_library_top(P0, "Library Card");
    sc.with_mana_pool(P0, mana(ManaType::Blue, 1));
    let mut runner = sc.build();

    cast_with(
        &mut runner,
        open_communications,
        CastIntent {
            targets: &[],
            returned: None,
            distribution: None,
            variant: None,
        },
    );
    assert_eq!(pool_total(&runner, P0), 0, "REACH: the pool paid the {{U}}");

    let spell_target = [TargetRef::Object(open_communications)];
    let record = cast_with(
        &mut runner,
        ruse,
        CastIntent::returning(birds).targeting(&spell_target),
    );
    assert_eq!(
        record.return_choices.as_deref().map(set),
        Some(set(&[birds, bears]))
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast completes: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(mana_spent_on(&runner, ruse), 2, "{{U}}{{U}} was paid");
    assert!(runner.state().objects[&island].tapped, "the Island paid");
    assert_eq!(zone(&runner, birds), Zone::Hand, "then the Birds returned");
    assert_eq!(zone(&runner, bears), Zone::Battlefield);

    pass_until_prompt(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        zone(&runner, open_communications),
        Zone::Graveyard,
        "countered"
    );
}

/// R12 (CR 601.2g + CR 601.2h): cancelling the cast in the mana window,
/// before the total is paid, leaves the selected Elf on the battlefield and
/// the card in the graveyard; nothing was returned.
#[test]
fn beam_me_up_cancelled_in_the_mana_window_returns_nothing() {
    let (mut runner, card, elf, _forest) = final_frontier_with_elf_and_forest();
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("cast Final Frontier with beam me up");
    select(&mut runner, vec![elf]);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "REACH: the mana window is open: {:?}",
        runner.state().waiting_for
    );
    activate_for_mana(&mut runner, elf);

    runner
        .act(GameAction::CancelCast)
        .expect("the cast can be cancelled before payment");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        zone(&runner, elf),
        Zone::Battlefield,
        "nothing was returned"
    );
    assert_eq!(zone(&runner, card), Zone::Graveyard);
}

/// R13 (CR 601.2h + CR 903.9b + CR 616.1): the deferred return still meets
/// replacement effects when it is paid. A commander returned for beam me up
/// offers its owner the command zone; the cast completes after the choice.
#[test]
fn beam_me_up_returned_commander_offers_the_command_zone_at_the_commit() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let card = sorcery_in_graveyard(
        &mut sc,
        P0,
        "Final Frontier",
        cost(2, &[ManaCostShard::Green]),
        FINAL_FRONTIER,
    );
    let commander = sc.add_creature(P0, "Commander Bear", 2, 2).id();
    sc.with_commander(commander);
    sc.add_card_to_library_top(P0, "Library Card");
    sc.with_mana_pool(P0, mana(ManaType::Green, 2));
    let mut runner = sc.build();
    runner.state_mut().format_config.command_zone = true;
    // Fixture: the commander starts on the battlefield (it was cast earlier).
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        commander,
        Zone::Battlefield,
        &mut Vec::new(),
    );
    let commander = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].is_commander)
        .expect("REACH: the commander is on the battlefield");

    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Final Frontier with beam me up");
    select(&mut runner, vec![commander]);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the commander's return offers CR 903.9b: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(pool_total(&runner, P0), 0, "the mana was paid first");
    assert_eq!(
        zone(&runner, commander),
        Zone::Battlefield,
        "the return stays proposed until the choice"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the command zone");

    assert_eq!(zone(&runner, commander), Zone::Command);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast completes after the choice: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(mana_spent_on(&runner, card), 2);
}
