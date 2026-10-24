//! CR 109.5 + CR 601.2a: a spell cast by a player who does not own it reads
//! "you" / "your" in its additional and alternative costs as the caster.
//! Each test runs an owner control row (P0 casts its own card) and a non-owner
//! row (P0 casts P1's card from exile); both must pay with P0's objects.

use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{CardPlayMode, CastingPermission, Duration, PlayFromExileProvenance};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, CounterCostChoice, PayCostKind, StackEntry, StackEntryKind,
    WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::CastFrequency;
use engine::types::zones::{EtbTapState, ExileCostSourceZone, Zone};
use engine::types::TargetRef;

const CORPSE_LUNGE: &str = "As an additional cost to cast this spell, exile a creature card from your graveyard.\nCorpse Lunge deals damage equal to the exiled card's power to target creature.";
const FORCE_OF_WILL: &str = "You may pay 1 life and exile a blue card from your hand rather than pay this spell's mana cost.\nCounter target spell.";
const DAZE: &str = "You may return an Island you control to its owner's hand rather than pay this spell's mana cost.\nCounter target spell unless its controller pays {1}.";
const DEPRIVE: &str = "As an additional cost to cast this spell, return a land you control to its owner's hand.\nCounter target spell.";
const ANGELIC_FAVOR: &str = "If you control a Plains, you may tap an untapped creature you control rather than pay this spell's mana cost.\nCast this spell only during combat.\nCreate a 4/4 white Angel creature token with flying. Exile it at the beginning of the next end step.";
const HIEROPHANT_BIO_TITAN: &str = "Frenzied Metabolism — As an additional cost to cast this spell, you may remove any number of +1/+1 counters from among creatures you control. This spell costs {2} less to cast for each counter removed this way.\nReach, vigilance, ward {2}\nTitanic — This creature can't be blocked by creatures with power 2 or less.";
const SOULBLAST: &str = "As an additional cost to cast this spell, sacrifice all creatures you control.\nSoulblast deals damage to any target equal to the total power of the sacrificed creatures.";

enum Aim {
    Creature,
    Spell,
    Opponent,
}

struct Board {
    runner: GameRunner,
    card: ObjectId,
    mine: ObjectId,
    theirs: ObjectId,
    target: TargetRef,
}

struct Spec<'a> {
    name: &'a str,
    text: &'a str,
    mana_cost: ManaCost,
    phase: Phase,
    tap_lands: bool,
    aim: Aim,
}

fn mana(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// P0 holds `mine`, P1 holds `theirs`; the `owner`-owned card sits in exile,
/// castable by P0 under an impulse permission.
fn board(
    spec: Spec<'_>,
    owner: PlayerId,
    build: impl FnOnce(&mut GameScenario) -> (ObjectId, ObjectId),
) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(spec.phase);
    let (mine, theirs) = build(&mut scenario);
    let victim = scenario.add_vanilla(P1, 1, 9);
    let card = {
        let mut b = scenario.add_spell_to_exile(owner, spec.name, true);
        b.from_oracle_text(spec.text);
        b.with_mana_cost(spec.mana_cost);
        b.id()
    };
    let mut runner = scenario.build();
    if spec.tap_lands {
        let lands: Vec<ObjectId> = runner
            .state()
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                runner.state().objects[id]
                    .card_types
                    .core_types
                    .contains(&CoreType::Land)
            })
            .collect();
        for id in lands {
            runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
        }
    }
    runner
        .state_mut()
        .objects
        .get_mut(&card)
        .unwrap()
        .casting_permissions
        .push(CastingPermission::PlayFromExile {
            provenance: PlayFromExileProvenance::Impulse,
            duration: Duration::UntilEndOfTurn,
            granted_to: P0,
            mode: CardPlayMode::Play,
            frequency: CastFrequency::Unlimited,
            source_id: None,
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_modifier: None,
            alt_ability_cost: None,
            without_paying_mana_cost: false,
            land_enter_tapped: EtbTapState::Unspecified,
        });
    let target = match spec.aim {
        Aim::Creature => TargetRef::Object(victim),
        Aim::Opponent => TargetRef::Player(P1),
        Aim::Spell => TargetRef::Object(push_opponent_spell(&mut runner)),
    };
    Board {
        runner,
        card,
        mine,
        theirs,
        target,
    }
}

fn push_opponent_spell(runner: &mut GameRunner) -> ObjectId {
    let card_id = CardId(900_500);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P1,
        "Opponent Spell".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    runner.state_mut().stack.push_back(StackEntry {
        id,
        source_id: id,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id,
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    id
}

impl Board {
    /// Announce the cast, then accept every optional cost and choose the target
    /// until the cast waits on something else. Returns whether an optional
    /// cost was offered.
    fn cast(&mut self) -> Result<bool, EngineError> {
        let card_id = self.runner.state().objects[&self.card].card_id;
        self.runner.act(GameAction::CastSpell {
            object_id: self.card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })?;
        self.advance()
    }

    fn advance(&mut self) -> Result<bool, EngineError> {
        let mut offered = false;
        loop {
            let action = match self.runner.state().waiting_for {
                WaitingFor::OptionalCostChoice { .. } => {
                    offered = true;
                    GameAction::DecideOptionalCost { pay: true }
                }
                WaitingFor::TargetSelection { .. } => GameAction::ChooseTarget {
                    target: Some(self.target.clone()),
                },
                _ => return Ok(offered),
            };
            self.runner.act(action)?;
        }
    }

    fn pay_cost(&self) -> (PayCostKind, Vec<ObjectId>) {
        match &self.runner.state().waiting_for {
            WaitingFor::PayCost { kind, choices, .. } => (kind.clone(), choices.clone()),
            other => panic!("expected PayCost, got {other:?}"),
        }
    }

    fn select_mine_and_finish(&mut self) {
        self.runner
            .act(GameAction::SelectCards {
                cards: vec![self.mine],
            })
            .unwrap();
        self.advance().unwrap();
        self.assert_cast_on_stack();
    }

    fn assert_cast_on_stack(&self) {
        assert!(matches!(
            self.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(
            self.runner.state().stack.back().map(|e| e.id),
            Some(self.card)
        );
    }

    fn zone(&self, id: ObjectId) -> Zone {
        self.runner.state().objects[&id].zone
    }
}

#[test]
fn graveyard_exile_additional_cost_reads_the_caster() {
    for owner in [P0, P1] {
        let spec = Spec {
            name: "Corpse Lunge",
            text: CORPSE_LUNGE,
            mana_cost: ManaCost::default(),
            phase: Phase::PreCombatMain,
            tap_lands: false,
            aim: Aim::Creature,
        };
        let mut b = board(spec, owner, |sc| {
            (
                sc.add_creature_to_graveyard(P0, "Mine", 3, 3).id(),
                sc.add_creature_to_graveyard(P1, "Theirs", 4, 4).id(),
            )
        });
        b.cast().unwrap();
        assert_eq!(
            b.pay_cost(),
            (
                PayCostKind::ExileFromZone {
                    zone: ExileCostSourceZone::Graveyard
                },
                vec![b.mine]
            ),
            "owner {owner:?}"
        );
        b.select_mine_and_finish();
        assert_eq!(b.zone(b.mine), Zone::Exile);
        assert_eq!(b.zone(b.theirs), Zone::Graveyard);
    }
}

#[test]
fn hand_exile_alternative_cost_reads_the_caster() {
    for owner in [P0, P1] {
        let spec = Spec {
            name: "Force of Will",
            text: FORCE_OF_WILL,
            mana_cost: mana(3, vec![ManaCostShard::Blue, ManaCostShard::Blue]),
            phase: Phase::PreCombatMain,
            tap_lands: false,
            aim: Aim::Spell,
        };
        let mut b = board(spec, owner, |sc| {
            let blue = || mana(0, vec![ManaCostShard::Blue]);
            (
                sc.add_creature_to_hand(P0, "Mine", 1, 1)
                    .with_mana_cost(blue())
                    .id(),
                sc.add_creature_to_hand(P1, "Theirs", 1, 1)
                    .with_mana_cost(blue())
                    .id(),
            )
        });
        assert!(b.cast().unwrap(), "owner {owner:?}: alternative offered");
        assert_eq!(
            b.pay_cost(),
            (
                PayCostKind::ExileFromZone {
                    zone: ExileCostSourceZone::Hand
                },
                vec![b.mine]
            ),
            "owner {owner:?}"
        );
        b.select_mine_and_finish();
        assert_eq!(b.zone(b.mine), Zone::Exile);
        assert_eq!(b.zone(b.theirs), Zone::Hand);
        assert_eq!(b.runner.life(P0), 19);
    }
}

#[test]
fn return_to_hand_alternative_cost_reads_the_caster() {
    for owner in [P0, P1] {
        let spec = Spec {
            name: "Daze",
            text: DAZE,
            mana_cost: mana(1, vec![ManaCostShard::Blue]),
            phase: Phase::PreCombatMain,
            tap_lands: true,
            aim: Aim::Spell,
        };
        let mut b = board(spec, owner, |sc| {
            (
                sc.add_basic_land(P0, ManaColor::Blue),
                sc.add_basic_land(P1, ManaColor::Blue),
            )
        });
        assert!(b.cast().unwrap(), "owner {owner:?}: alternative offered");
        assert_eq!(
            b.pay_cost(),
            (PayCostKind::ReturnToHand, vec![b.mine]),
            "owner {owner:?}"
        );
        b.select_mine_and_finish();
        assert_eq!(b.zone(b.mine), Zone::Hand);
        assert_eq!(b.zone(b.theirs), Zone::Battlefield);
    }
}

#[test]
fn return_to_hand_additional_cost_reads_the_caster() {
    for owner in [P0, P1] {
        let spec = Spec {
            name: "Deprive",
            text: DEPRIVE,
            mana_cost: ManaCost::default(),
            phase: Phase::PreCombatMain,
            tap_lands: false,
            aim: Aim::Spell,
        };
        let mut b = board(spec, owner, |sc| {
            (
                sc.add_basic_land(P0, ManaColor::Blue),
                sc.add_basic_land(P1, ManaColor::Blue),
            )
        });
        b.cast().unwrap();
        assert_eq!(
            b.pay_cost(),
            (PayCostKind::ReturnToHand, vec![b.mine]),
            "owner {owner:?}"
        );
        b.select_mine_and_finish();
        assert_eq!(b.zone(b.mine), Zone::Hand);
        assert_eq!(b.zone(b.theirs), Zone::Battlefield);
    }
}

#[test]
fn tap_creature_alternative_cost_reads_the_caster() {
    for owner in [P0, P1] {
        let spec = Spec {
            name: "Angelic Favor",
            text: ANGELIC_FAVOR,
            mana_cost: mana(3, vec![ManaCostShard::White]),
            phase: Phase::BeginCombat,
            tap_lands: true,
            aim: Aim::Creature,
        };
        let mut b = board(spec, owner, |sc| {
            sc.add_basic_land(P0, ManaColor::White);
            sc.add_basic_land(P1, ManaColor::White);
            (sc.add_vanilla(P0, 2, 2), sc.add_vanilla(P1, 2, 2))
        });
        assert!(b.cast().unwrap(), "owner {owner:?}: alternative offered");
        let (kind, choices) = b.pay_cost();
        assert!(
            matches!(kind, PayCostKind::TapCreatures { .. }),
            "owner {owner:?}: {kind:?}"
        );
        assert_eq!(choices, vec![b.mine], "owner {owner:?}");
        b.select_mine_and_finish();
        assert!(b.runner.state().objects[&b.mine].tapped);
        assert!(!b.runner.state().objects[&b.theirs].tapped);
    }
}

#[test]
fn remove_counter_x_cost_reads_the_caster() {
    for owner in [P0, P1] {
        let spec = Spec {
            name: "Hierophant Bio-Titan",
            text: HIEROPHANT_BIO_TITAN,
            mana_cost: ManaCost::default(),
            phase: Phase::PreCombatMain,
            tap_lands: false,
            aim: Aim::Creature,
        };
        let mut b = board(spec, owner, |sc| {
            let mine = sc.add_vanilla(P0, 2, 2);
            let theirs = sc.add_vanilla(P1, 2, 2);
            sc.with_counter(mine, CounterType::Plus1Plus1, 2);
            sc.with_counter(theirs, CounterType::Plus1Plus1, 3);
            (mine, theirs)
        });
        assert!(b.cast().unwrap(), "owner {owner:?}: optional cost offered");
        let WaitingFor::ChooseXValue { max, .. } = b.runner.state().waiting_for else {
            panic!("owner {owner:?}: expected ChooseXValue");
        };
        assert_eq!(max, 2, "owner {owner:?}");
        b.runner.act(GameAction::ChooseX { value: 2 }).unwrap();
        b.runner
            .act(GameAction::ChooseRemoveCounterCostDistribution {
                distribution: vec![CounterCostChoice {
                    object_id: b.mine,
                    counter_type: CounterType::Plus1Plus1,
                    count: 2,
                }],
            })
            .unwrap();
        b.advance().unwrap();
        b.assert_cast_on_stack();
        let counters = |id: ObjectId| {
            b.runner.state().objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        };
        assert_eq!(counters(b.mine), 0);
        assert_eq!(counters(b.theirs), 3);
    }
}

#[test]
fn deferred_sacrifice_additional_cost_reads_the_caster() {
    for owner in [P0, P1] {
        let spec = Spec {
            name: "Soulblast",
            text: SOULBLAST,
            mana_cost: ManaCost::default(),
            phase: Phase::PreCombatMain,
            tap_lands: false,
            aim: Aim::Opponent,
        };
        // One creature each: the parsed "all" cost carries a count of one.
        let mut b = board(spec, owner, |sc| {
            (sc.add_vanilla(P0, 2, 2), sc.add_vanilla(P1, 2, 2))
        });
        b.cast().unwrap();
        assert_eq!(
            b.pay_cost(),
            (PayCostKind::Sacrifice, vec![b.mine]),
            "owner {owner:?}"
        );
        b.select_mine_and_finish();
        assert_eq!(b.zone(b.mine), Zone::Graveyard);
        assert_eq!(b.zone(b.theirs), Zone::Battlefield);
    }
}
