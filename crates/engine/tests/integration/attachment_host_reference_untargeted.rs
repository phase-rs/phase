//! "Enchanted creature" / "equipped creature" in an Aura's or Equipment's own
//! ability names its host; it is not a target (phase-rs/phase#9824).
//!
//! CR 115.10a: an object affected by a spell or ability is a target only if the
//! text identifies it with the word "target". CR 303.4b / CR 301.5a + CR 301.5f:
//! "enchanted"/"equipped" names whatever the source is attached to. CR 115.1b: an
//! Aura spell is targeted, but an Aura permanent targets nothing. So such an
//! instruction never makes its host "become the target" (no ward trigger, CR
//! 702.21a), is not stopped by shroud/hexproof (CR 702.18a / CR 702.11b), and
//! reads its host when it resolves. The host is used only when it is provably
//! the object the source is, or (by the source's own last known information,
//! CR 608.2h) was, attached to and hasn't moved since; otherwise the
//! instruction does nothing.
//!
//! Oracle text below is verbatim from Scryfall `cards/named`.

use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityDefinition, Effect, TargetChoiceTiming};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, WardCost};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const MALFUNCTION: &str = "Enchant artifact or creature\nWhen this Aura enters, tap enchanted permanent.\nEnchanted permanent doesn't untap during its controller's untap step.";
const CLAUSTROPHOBIA: &str = "Enchant creature\nWhen this Aura enters, tap enchanted creature.\nEnchanted creature doesn't untap during its controller's untap step.";
const BIND_THE_MONSTER: &str = "Enchant creature\nWhen this Aura enters, tap enchanted creature. It deals damage to you equal to its power.\nEnchanted creature doesn't untap during its controller's untap step.";
const FATAL_ATTRACTION: &str = "Enchant creature\nWhen this Aura enters, it deals 2 damage to enchanted creature.\nAt the beginning of your upkeep, this Aura deals 4 damage to enchanted creature.";
const PEMMINS_AURA: &str = "Enchant creature\n{U}: Untap enchanted creature.\n{U}: Enchanted creature gains flying until end of turn.\n{U}: Enchanted creature gains shroud until end of turn. (It can't be the target of spells or abilities.)\n{1}: Enchanted creature gets +1/-1 or -1/+1 until end of turn.";
const CLOAK_OF_THE_PHILOSOPHER: &str =
    "Equipped creature gets +1/+1.\n{2}: Untap equipped creature.\nEquip {2}";
const PARALLAX_DEMENTIA_LEAVES: &str = "Enchant creature\nEnchanted creature gets +3/+2.\nWhen this Aura leaves the battlefield, destroy enchanted creature. That creature can't be regenerated.";
const NURTURING_LICID: &str = "{G}, {T}: This creature loses this ability and becomes an Aura enchantment with enchant creature. Attach it to target creature. You may pay {G} to end this effect.\n{G}: Regenerate enchanted creature.";
const FRENZIED_FUGUE: &str = "Enchant permanent\nWhen this Aura enters and at the beginning of your upkeep, gain control of enchanted permanent until end of turn. Untap that permanent. It gains haste until end of turn.";
const MAROONED: &str = "Enchant artifact or creature\nWhen this Aura enters, you may pay {3}. If you do, tap enchanted permanent.\nEnchanted permanent doesn't untap during its controller's untap step and can't be beamed up.";
const SWORD_OF_THE_PARUNS: &str = "As long as equipped creature is tapped, tapped creatures you control get +2/+0.\nAs long as equipped creature is untapped, untapped creatures you control get +0/+2.\n{3}: You may tap or untap equipped creature.\nEquip {3}";
const SUN_CLASP: &str =
    "Enchant creature\nEnchanted creature gets +1/+3.\n{W}: Return enchanted creature to its owner's hand.";
const VANISHING: &str = "Enchant creature\n{U}{U}: Enchanted creature phases out. (While it's phased out, it's treated as though it doesn't exist. It phases in before its controller untaps during their next untap step.)";
const SHATTERED_EGO: &str = "Enchant creature\nEnchanted creature gets -3/-0.\n{3}{U}{U}: Put enchanted creature into its owner's library third from the top.";
const JUNK_JET: &str = "When this Equipment enters, create a Junk token. (It's an artifact with \"{T}, Sacrifice this token: Exile the top card of your library. You may play that card this turn. Activate only as a sorcery.\")\n{3}, Sacrifice another artifact: Double equipped creature's power until end of turn.\nEquip {1}";
const FOLLOWED_FOOTSTEPS: &str = "Enchant creature\nAt the beginning of your upkeep, create a token that's a copy of enchanted creature.";
const CONVENIENT_TARGET: &str = "Enchant creature\nWhen this Aura enters, suspect enchanted creature. (It has menace and can't block.)\nEnchanted creature gets +1/+1.\n{2}{R}: Return this card from your graveyard to your hand.";
const ASSAULT_SUIT: &str = "Equipped creature gets +2/+2, has haste, can't attack you or planeswalkers you control, and can't be sacrificed.\nAt the beginning of each opponent's upkeep, you may have that player gain control of equipped creature until end of turn. If you do, untap it.\nEquip {3}";
const OBSERVED_STASIS: &str = "Flash\nEnchant creature an opponent controls\nWhen this Aura enters, remove enchanted creature from combat. Then draw a card for each tapped creature its controller controls.\nEnchanted creature loses all abilities and can't attack or block.";
const LAST_VOYAGE: &str = "When this enchantment enters, you may put a name sticker on it, then it becomes an Aura with enchant creature. Return a creature card from your graveyard to the battlefield and attach this Aura to it.\nEnchanted creature gets +2/+0 for each name sticker on this Aura with seven or fewer letters.\nWhen this Aura leaves the battlefield, sacrifice enchanted creature.";
const PENUMBRA_UMBRA: &str = "Enchant creature you control\nWhen Penumbra Umbra is put into a graveyard from the battlefield, create a token that's a copy of enchanted creature, except the token is black.\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)";
const CAPTIVATING_GLANCE: &str = "Enchant creature\nAt the beginning of your end step, clash with an opponent. If you win, gain control of enchanted creature. Otherwise, that player gains control of enchanted creature. (Each clashing player reveals the top card of their library, then puts that card on their choice of the top or bottom. A player wins if their card had a greater mana value.)";
const STAMINA: &str = "Enchant creature\nEnchanted creature has vigilance.\nSacrifice this Aura: Regenerate enchanted creature.";
const EPHARAS_ENLIGHTENMENT: &str = "Enchant creature\nWhen this Aura enters, put a +1/+1 counter on enchanted creature.\nEnchanted creature has flying.\nWhenever a creature you control enters, you may return this Aura to its owner's hand.";
const HELM_OF_THE_HOST: &str = "At the beginning of combat on your turn, create a token that's a copy of equipped creature, except the token isn't legendary. That token gains haste.\nEquip {5}";
const FALL_FROM_FAVOR: &str = "Enchant creature\nWhen this Aura enters, tap enchanted creature and you become the monarch.\nEnchanted creature doesn't untap during its controller's untap step unless that player is the monarch.";

fn free_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

fn pool(blue: usize, colorless: usize) -> Vec<ManaUnit> {
    let unit = |t| ManaUnit::new(t, ObjectId(0), false, vec![]);
    std::iter::repeat_n(unit(ManaType::Blue), blue)
        .chain(std::iter::repeat_n(unit(ManaType::Colorless), colorless))
        .collect()
}

fn aura_in_hand(sc: &mut GameScenario, player: PlayerId, name: &str, text: &str) -> ObjectId {
    sc.add_spell_to_hand(player, name, false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(free_cost())
        .from_oracle_text_with_keywords(&["enchant"], text)
        .id()
}

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
}

fn set_tapped(runner: &mut GameRunner, id: ObjectId, tapped: bool) {
    runner.state_mut().objects.get_mut(&id).unwrap().tapped = tapped;
}

fn ward_triggers_on_stack(state: &GameState, ward_holder: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == ward_holder)
        })
        .count()
}

/// The index of `source`'s first activated ability whose effect matches `pick`.
fn ability_index(runner: &GameRunner, source: ObjectId, pick: fn(&Effect) -> bool) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| pick(&a.effect))
        .expect("the ability is present")
}

#[derive(Debug, Default)]
struct Drive {
    ward_after_cast: usize,
    /// Every distinct ward trigger the host put on the stack during the drive.
    ward_triggers: std::collections::HashSet<ObjectId>,
}

impl Drive {
    fn ward_triggers(&self) -> usize {
        self.ward_triggers.len()
    }
}

/// Cast the Aura spell at `host` through the shared cast driver, then resolve
/// everything, observing the host's ward triggers. Pays the first ward
/// trigger's cost and declines a second one (the live game had no mana left);
/// accepts every "you may". A trigger asking for a target is a failure here:
/// nothing on these boards should target after the Aura spell.
fn cast_aura_and_resolve(
    runner: &mut GameRunner,
    aura: ObjectId,
    host: ObjectId,
    mode: CastPaymentMode,
) -> Drive {
    let commit = runner
        .cast(aura)
        .payment_mode(mode)
        .target_object(host)
        .commit();
    let mut d = Drive {
        ward_after_cast: ward_triggers_on_stack(commit.state(), host),
        ..Drive::default()
    };
    drop(commit);
    for _ in 0..64 {
        d.ward_triggers.extend(runner.state().stack.iter().filter_map(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == host)
                .then_some(e.id)
        }));
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return d,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::UnlessPayment { .. } => GameAction::PayUnlessCost {
                pay: d.ward_triggers() < 2,
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).expect("prompt accepts");
    }
    panic!("the stack did not resolve");
}

/// P1 controls a Ward {2} creature; P0 casts `aura_text` at it with exactly
/// enough mana for ONE ward payment.
fn ward_host_case(aura_name: &str, aura_text: &str, mode: CastPaymentMode) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P1, "Patchwork Automaton", 1, 1)
        .with_keyword(Keyword::Ward(WardCost::Mana(ManaCost::generic(2))))
        .id();
    let aura = aura_in_hand(&mut sc, P0, aura_name, aura_text);
    // A second legal enchant target, so the Aura spell's target is a real choice.
    sc.add_creature(P0, "Grizzly Bears", 2, 2);
    sc.with_mana_pool(P0, pool(0, 2));
    let mut runner = sc.build();

    let d = cast_aura_and_resolve(&mut runner, aura, host, mode);
    let s = runner.state();

    // CR 702.21a + CR 115.1b: the Aura spell's one targeting triggers ward once.
    assert_eq!(
        d.ward_after_cast, 1,
        "reach ({aura_name}, {mode:?}): the Aura spell targeted the ward creature"
    );
    assert_eq!(
        s.objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "reach ({aura_name}, {mode:?}): ward was paid, so the Aura resolved and attached"
    );
    // CR 115.10a: "tap enchanted permanent/creature" has no "target", so the ETB
    // never makes the host become the target and ward asks exactly once.
    assert_eq!(
        d.ward_triggers(),
        1,
        "({aura_name}, {mode:?}) ward triggers exactly once"
    );
    assert!(
        s.objects[&host].tapped,
        "({aura_name}, {mode:?}) the ETB taps the enchanted permanent"
    );
}

#[test]
fn malfunction_ward_asks_once_and_etb_taps_host_auto_payment() {
    ward_host_case("Malfunction", MALFUNCTION, CastPaymentMode::Auto);
}

#[test]
fn malfunction_ward_asks_once_and_etb_taps_host_interactive_payment() {
    ward_host_case("Malfunction", MALFUNCTION, CastPaymentMode::Manual);
}

#[test]
fn claustrophobia_ward_asks_once_and_etb_taps_host() {
    ward_host_case("Claustrophobia", CLAUSTROPHOBIA, CastPaymentMode::Auto);
}

/// DealDamage sibling: "it deals 2 damage to enchanted creature" never targets.
#[test]
fn fatal_attraction_etb_damage_does_not_retrigger_ward() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P1, "Warded Brute", 4, 4)
        .with_keyword(Keyword::Ward(WardCost::Mana(ManaCost::generic(2))))
        .id();
    let aura = aura_in_hand(&mut sc, P0, "Fatal Attraction", FATAL_ATTRACTION);
    sc.add_creature(P0, "Grizzly Bears", 2, 2);
    sc.with_mana_pool(P0, pool(0, 2));
    let mut runner = sc.build();

    let d = cast_aura_and_resolve(&mut runner, aura, host, CastPaymentMode::Auto);
    let s = runner.state();
    assert_eq!(
        s.objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "reach: Fatal Attraction resolved and attached"
    );
    assert_eq!(d.ward_triggers(), 1, "ward triggers exactly once");
    assert_eq!(
        s.objects[&host].damage_marked, 2,
        "the ETB deals 2 damage to the enchanted creature"
    );
}

/// The "It" in "It deals damage to you equal to its power" names the host the
/// tap instruction acted on, so the chained damage follows the bound host.
#[test]
fn bind_the_monster_taps_host_and_host_deals_its_power_to_you() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc.add_creature(P1, "Craw Wurm", 6, 4).id();
    let aura = aura_in_hand(&mut sc, P0, "Bind the Monster", BIND_THE_MONSTER);
    sc.add_creature(P0, "Grizzly Bears", 2, 2);
    let mut runner = sc.build();
    let life_before = runner.state().players[P0.0 as usize].life;

    let d = cast_aura_and_resolve(&mut runner, aura, host, CastPaymentMode::Auto);
    let s = runner.state();
    assert_eq!(d.ward_triggers(), 0, "reach: no ward on this board");
    assert_eq!(
        s.objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "reach: Bind the Monster resolved and attached"
    );
    assert!(
        s.objects[&host].tapped,
        "the ETB taps the enchanted creature"
    );
    assert_eq!(
        s.players[P0.0 as usize].life,
        life_before - 6,
        "the enchanted creature deals damage equal to its power to Bind the Monster's controller"
    );
}

/// Puts the Aura `text` on the battlefield attached to a shrouded creature of
/// P0's (tapped), with `mana` floating, and activates the ability `pick`
/// selects. CR 115.10a: the host is not a target, so shroud can't stop it.
fn activate_on_shrouded_host(
    text: &str,
    mana: Vec<ManaUnit>,
    pick: fn(&Effect) -> bool,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P0, "Shrouded Bear", 2, 2)
        .with_keyword(Keyword::Shroud)
        .id();
    let source = aura_on_battlefield(&mut sc, P0, "Test Aura", text);
    sc.with_mana_pool(P0, mana);
    let mut runner = sc.build();
    attach(&mut runner, source, host);
    let index = ability_index(&runner, source, pick);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CR 115.10a: an ability naming the host without 'target' can be activated");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the ability resolved"
    );
    (runner, source, host)
}

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    std::iter::repeat_n(ManaUnit::new(kind, ObjectId(0), false, vec![]), n).collect()
}

/// Bounce sibling: Sun Clasp, "{W}: Return enchanted creature to its owner's hand."
#[test]
fn sun_clasp_returns_its_shrouded_host() {
    let (runner, _, host) = activate_on_shrouded_host(SUN_CLASP, mana(ManaType::White, 1), |e| {
        matches!(e, Effect::Bounce { .. })
    });
    assert_eq!(runner.state().objects[&host].zone, Zone::Hand);
}

/// PhaseOut sibling: Vanishing, "{U}{U}: Enchanted creature phases out."
#[test]
fn vanishing_phases_out_its_shrouded_host() {
    let (runner, _, host) = activate_on_shrouded_host(VANISHING, mana(ManaType::Blue, 2), |e| {
        matches!(e, Effect::PhaseOut { .. })
    });
    assert!(
        !runner.state().objects[&host].is_phased_in(),
        "the host phased out"
    );
}

/// PutAtLibraryPosition sibling: Shattered Ego, "{3}{U}{U}: Put enchanted
/// creature into its owner's library third from the top."
#[test]
fn shattered_ego_puts_its_shrouded_host_into_the_library() {
    let mut pool = mana(ManaType::Blue, 2);
    pool.extend(mana(ManaType::Colorless, 3));
    let (runner, _, host) = activate_on_shrouded_host(SHATTERED_EGO, pool, |e| {
        matches!(e, Effect::PutAtLibraryPosition { .. })
    });
    assert_eq!(runner.state().objects[&host].zone, Zone::Library);
}

/// DoublePT sibling: Junk Jet, "{3}, Sacrifice another artifact: Double equipped
/// creature's power until end of turn."
#[test]
fn junk_jet_doubles_its_shrouded_equipped_creatures_power() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P0, "Shrouded Bear", 3, 3)
        .with_keyword(Keyword::Shroud)
        .id();
    let jet = sc
        .add_creature(P0, "Junk Jet", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(JUNK_JET)
        .id();
    let fodder = sc.add_creature(P0, "Spare Part", 0, 0).as_artifact().id();
    sc.with_mana_pool(P0, mana(ManaType::Colorless, 3));
    let mut runner = sc.build();
    attach(&mut runner, jet, host);
    let index = ability_index(&runner, jet, |e| matches!(e, Effect::DoublePT { .. }));
    runner
        .act(GameAction::ActivateAbility {
            source_id: jet,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CR 115.10a: 'double equipped creature's power' does not target");
    for _ in 0..8 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::PayCost { choices, .. } => {
                assert_eq!(
                    choices,
                    &vec![fodder],
                    "reach: the other artifact is the fodder"
                );
                GameAction::SelectCards {
                    cards: vec![fodder],
                }
            }
            other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).expect("prompt accepts");
    }
    let s = runner.state();
    assert_eq!(
        s.objects[&fodder].zone,
        Zone::Graveyard,
        "reach: the artifact was sacrificed"
    );
    assert_eq!(
        s.objects[&host].power,
        Some(6),
        "the equipped creature's power doubled"
    );
}

/// CopyTokenOf sibling: Followed Footsteps, "At the beginning of your upkeep,
/// create a token that's a copy of enchanted creature." on a shrouded creature.
#[test]
fn followed_footsteps_copies_its_shrouded_host() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P1, "Shrouded Bear", 2, 2)
        .with_keyword(Keyword::Shroud)
        .id();
    let aura = aura_on_battlefield(&mut sc, P0, "Followed Footsteps", FOLLOWED_FOOTSTEPS);
    sc.with_library_top(P0, &["Card A", "Card B"]);
    sc.with_library_top(P1, &["Card C", "Card D"]);
    let mut runner = sc.build();
    attach(&mut runner, aura, host);
    // From P1's end step to P0's upkeep.
    {
        let state = runner.state_mut();
        state.phase = Phase::End;
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_until_stack_empty();
    let s = runner.state();
    assert_eq!(s.active_player, P0, "reach: it is P0's upkeep");
    let copies = s
        .battlefield
        .iter()
        .filter(|id| {
            let o = &s.objects[*id];
            o.is_token && o.name == "Shrouded Bear" && o.controller == P0
        })
        .count();
    assert_eq!(
        copies, 1,
        "P0 creates one token copy of the enchanted creature"
    );
}

/// Suspect sibling: Convenient Target, "When this Aura enters, suspect
/// enchanted creature." cast on an opponent's ward creature.
#[test]
fn convenient_target_suspects_ward_host_with_one_ward_trigger() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P1, "Patchwork Automaton", 1, 1)
        .with_keyword(Keyword::Ward(WardCost::Mana(ManaCost::generic(2))))
        .id();
    let aura = aura_in_hand(&mut sc, P0, "Convenient Target", CONVENIENT_TARGET);
    sc.add_creature(P0, "Grizzly Bears", 2, 2);
    sc.with_mana_pool(P0, pool(0, 2));
    let mut runner = sc.build();

    let d = cast_aura_and_resolve(&mut runner, aura, host, CastPaymentMode::Auto);
    let s = runner.state();
    assert_eq!(
        s.objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "reach: Convenient Target resolved and attached"
    );
    assert_eq!(d.ward_triggers(), 1, "ward triggers exactly once");
    assert!(
        s.objects[&host].is_suspected,
        "the enchanted creature is suspected"
    );
}

/// Whether the board also has a P1 bystander wearing an unrelated Aura.
#[derive(Clone, Copy)]
enum Bystander {
    Absent,
    WearingAnotherAura,
}

/// Puts Claustrophobia onto the battlefield attached to `host` (as a resolved
/// Aura spell would) and leaves its ETB trigger waiting on the stack.
fn claustrophobia_etb_on_stack(
    bystander: Bystander,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    aura_etb_on_stack("Claustrophobia", CLAUSTROPHOBIA, 2, bystander)
}

/// Casts the Aura `text` at a P1 creature of power `host_power` and stops with
/// the Aura attached and its ETB trigger waiting on the stack.
fn aura_etb_on_stack(
    name: &str,
    text: &str,
    host_power: i32,
    bystander: Bystander,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc.add_creature(P1, "Grizzly Bears", host_power, 2).id();
    let bystander = match bystander {
        Bystander::Absent => None,
        Bystander::WearingAnotherAura => Some(sc.add_creature(P1, "Bystander", 2, 2).id()),
    };
    let aura = aura_in_hand(&mut sc, P0, name, text);
    let other_aura =
        bystander.map(|_| aura_on_battlefield(&mut sc, P1, "Rancor Dummy", "Enchant creature"));
    let mut runner = sc.build();
    if let (Some(other), Some(bystander)) = (other_aura, bystander) {
        attach(&mut runner, other, bystander);
    }
    runner.cast(aura).target_object(host).commit();
    // Resolve the Aura spell only; its ETB trigger goes on the stack.
    runner.act(GameAction::PassPriority).expect("pass");
    for _ in 0..8 {
        if runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }),
        "reach: {name}'s ETB trigger is on the stack"
    );
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "reach: {name} is attached to the host"
    );
    (runner, aura, host, bystander)
}

/// Exile `id` and return it to the battlefield: a new object (CR 400.7).
fn blink(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), id, Zone::Exile, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), id, Zone::Battlefield, &mut events);
}

/// Review probe P2 (CR 400.7): Claustrophobia's ETB is pending on P1's creature
/// and P1 blinks the creature (Cloudshift). The returned creature is a new
/// object, not the enchanted creature, so it stays untapped.
#[test]
fn host_blinked_before_etb_resolves_returned_creature_stays_untapped() {
    let (mut runner, aura, host, _) = claustrophobia_etb_on_stack(Bystander::Absent);
    let incarnation_before = runner.state().objects[&host].incarnation;
    blink(&mut runner, host);
    assert_ne!(
        runner.state().objects[&host].incarnation,
        incarnation_before,
        "reach: the creature returned as a new object"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the pending ability resolved"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: resolution returned to priority"
    );
    let s = runner.state();
    assert_eq!(
        s.objects[&host].zone,
        Zone::Battlefield,
        "reach: it returned"
    );
    assert_ne!(
        s.objects[&aura].zone,
        Zone::Battlefield,
        "reach: CR 303.4c put the unattached Aura into the graveyard"
    );
    assert!(
        !s.objects[&host].tapped,
        "the returned creature is a new object, not the enchanted creature"
    );
}

/// Review probe P3, disclosed limit (fail closed): Bind the Monster's ETB is
/// pending on P1's 6-power creature and P1 returns that creature to hand
/// (Unsummon). The Aura, now unattached, goes to the graveyard, so there is no
/// provable host: nothing is tapped and no damage is dealt (base: the same —
/// the targeted ETB lost its only target).
#[test]
fn bind_the_monster_host_bounced_deals_nothing() {
    let (mut runner, aura, host, _) =
        aura_etb_on_stack("Bind the Monster", BIND_THE_MONSTER, 6, Bystander::Absent);
    let life_before = runner.state().players[P0.0 as usize].life;
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Hand, &mut events);
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the pending ability resolved"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: resolution returned to priority"
    );
    let s = runner.state();
    assert_eq!(s.objects[&host].zone, Zone::Hand, "reach: the host left");
    assert_ne!(
        s.objects[&aura].zone,
        Zone::Battlefield,
        "reach: the Aura left too"
    );
    assert!(
        !s.battlefield.iter().any(|id| s.objects[id].tapped),
        "nothing on the battlefield is tapped"
    );
    assert_eq!(
        s.players[P0.0 as usize].life, life_before,
        "no provable host, so no damage"
    );
}

/// Review probe P1 (CR 400.7 + CR 608.2h): Pemmin's Aura on A, "{U}: Untap
/// enchanted creature" activated; in response the Aura is flickered and returns
/// attached to B. The ability's source is the Aura as it was, so its enchanted
/// creature is A: A untaps and B stays tapped.
#[test]
fn pemmins_aura_flickered_onto_another_creature_untaps_its_original_host() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let a = sc.add_creature(P0, "Craw Wurm A", 6, 4).id();
    let b = sc.add_creature(P0, "Craw Wurm B", 6, 4).id();
    let aura = aura_on_battlefield(&mut sc, P0, "Pemmin's Aura", PEMMINS_AURA);
    sc.with_mana_pool(P0, pool(1, 0));
    let mut runner = sc.build();
    attach(&mut runner, aura, a);
    set_tapped(&mut runner, a, true);
    set_tapped(&mut runner, b, true);
    let index = ability_index(&runner, aura, |e| matches!(e, Effect::SetTapState { .. }));
    runner
        .act(GameAction::ActivateAbility {
            source_id: aura,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the untap ability is activated");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: the ability is on the stack"
    );
    // Flicker of Fate on the Aura: it returns as a new object, attached to B.
    blink(&mut runner, aura);
    runner
        .state_mut()
        .objects
        .get_mut(&a)
        .unwrap()
        .attachments
        .retain(|id| *id != aura);
    attach(&mut runner, aura, b);
    runner.advance_until_stack_empty();
    let s = runner.state();
    assert_eq!(
        s.objects[&aura].attached_to,
        Some(AttachTarget::Object(b)),
        "reach: the returned Aura enchants B"
    );
    assert!(
        !s.objects[&a].tapped,
        "A, the Aura's host as the ability saw it, untaps"
    );
    assert!(s.objects[&b].tapped, "B stays tapped");
}

/// Disclosed limit (fail closed): Penumbra Umbra, "When Penumbra Umbra is put
/// into a graveyard from the battlefield, create a token that's a copy of
/// enchanted creature, except the token is black." — the creature was exiled
/// first, so the Aura was attached to nothing as it left and there is no
/// provable host to copy (base: no token either).
#[test]
fn penumbra_umbra_with_its_host_exiled_copies_nothing() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc.add_creature(P0, "Exiled Beast", 4, 4).id();
    let aura = aura_on_battlefield(&mut sc, P0, "Penumbra Umbra", PENUMBRA_UMBRA);
    let mut runner = sc.build();
    attach(&mut runner, aura, host);
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Exile, &mut events);
    // CR 303.4c: the now-unattached Aura goes to the graveyard; its trigger fires.
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    assert!(
        runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }),
        "reach: Penumbra Umbra's put-into-a-graveyard trigger went on the stack"
    );
    runner.advance_until_stack_empty();
    let s = runner.state();
    assert!(s.stack.is_empty(), "reach: the trigger resolved");
    assert_eq!(
        s.objects[&aura].zone,
        Zone::Graveyard,
        "reach: the Aura was put into the graveyard"
    );
    let copies = s
        .battlefield
        .iter()
        .filter(|id| s.objects[id].is_token)
        .count();
    assert_eq!(copies, 0, "no token is created");
}

/// Review probe F1, disclosed limit (fail closed): Stamina, "Sacrifice this
/// Aura: Regenerate enchanted creature." The cost moves the Aura to the
/// graveyard before the ability is put on the stack, so the incarnation the
/// ability records is the sacrificed card's, which never left the
/// battlefield; there is no record proving the host, and nothing is
/// regenerated (base: no shield either).
#[test]
fn stamina_sacrificed_as_cost_regenerates_nothing() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = aura_on_battlefield(&mut sc, P0, "Stamina", STAMINA);
    let mut runner = sc.build();
    attach(&mut runner, aura, host);
    let index = ability_index(&runner, aura, |e| matches!(e, Effect::Regenerate { .. }));
    runner
        .act(GameAction::ActivateAbility {
            source_id: aura,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Stamina's ability is activated");
    if let WaitingFor::PayCost { choices, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::SelectCards { cards: choices })
            .expect("the Aura is sacrificed");
    }
    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Graveyard,
        "reach: the cost sacrificed the Aura"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: the regeneration ability is on the stack"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the ability resolved"
    );
    assert!(
        !has_regeneration_shield(&runner, host),
        "no provable host, so no shield"
    );
}

/// Equips Cloak of the Philosopher to `bears` through its Equip ability.
fn equip(runner: &mut GameRunner, cloak: ObjectId, bears: ObjectId) {
    let index = ability_index(runner, cloak, |e| matches!(e, Effect::Attach { .. }));
    runner.activate(cloak, index).target_object(bears).resolve();
    assert_eq!(
        runner.state().objects[&cloak].attached_to,
        Some(AttachTarget::Object(bears)),
        "reach: Cloak is equipped to the creature"
    );
}

/// What happened to the equipped creature before the Cloak's ability is
/// activated.
#[derive(Clone, Copy)]
enum CloakHistory {
    EquippedOnce,
    /// Earlier in the turn the creature was blinked (a new object) and the
    /// Cloak re-equipped to it.
    BlinkedThenReequipped,
    /// As `BlinkedThenReequipped`, then the creature transformed (CR 613.7g: a
    /// new timestamp; still the same object, still equipped).
    BlinkedReequippedThenTransformed,
}

/// Review probe F2 (CR 400.7 + CR 608.2h): Cloak of the Philosopher's "{2}:
/// Untap equipped creature." is activated, and the Cloak is destroyed in
/// response. The Cloak's departure says what it was attached to, and that
/// creature hasn't moved since, so it untaps — including when an earlier blink
/// made it a new object the Cloak was then re-equipped to (base: nothing).
fn cloak_destroyed_in_response(history: CloakHistory) -> bool {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let bears = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let cloak = sc
        .add_creature(P0, "Cloak of the Philosopher", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(CLOAK_OF_THE_PHILOSOPHER)
        .id();
    sc.with_mana_pool(P0, pool(0, 6));
    let mut runner = sc.build();
    equip(&mut runner, cloak, bears);
    match history {
        CloakHistory::EquippedOnce => {}
        CloakHistory::BlinkedThenReequipped | CloakHistory::BlinkedReequippedThenTransformed => {
            blink(&mut runner, bears);
            let mut events = Vec::new();
            engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
            equip(&mut runner, cloak, bears);
        }
    }
    if let CloakHistory::BlinkedReequippedThenTransformed = history {
        let timestamp_before = runner.state().objects[&bears].timestamp;
        let obj = runner.state_mut().objects.get_mut(&bears).unwrap();
        let mut back = engine::game::printed_cards::snapshot_object_face(obj);
        back.name = "Insectile Aberration".to_string();
        obj.back_face = Some(back);
        let mut events = Vec::new();
        engine::game::transform::transform_permanent(runner.state_mut(), bears, &mut events)
            .expect("the creature transforms");
        let s = runner.state();
        assert_eq!(s.objects[&bears].name, "Insectile Aberration", "reach");
        assert!(
            s.objects[&bears].timestamp > timestamp_before,
            "reach: CR 613.7g, transforming gave the creature a new timestamp"
        );
        assert_eq!(
            s.objects[&cloak].attached_to,
            Some(AttachTarget::Object(bears)),
            "reach: the Cloak is still attached"
        );
    }
    set_tapped(&mut runner, bears, true);
    let index = ability_index(&runner, cloak, |e| matches!(e, Effect::SetTapState { .. }));
    runner
        .act(GameAction::ActivateAbility {
            source_id: cloak,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the untap ability is activated");
    // Disenchant on the Cloak, in response.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), cloak, Zone::Graveyard, &mut events);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: the ability is still on the stack"
    );
    runner.advance_until_stack_empty();
    !runner.state().objects[&bears].tapped
}

#[test]
fn cloak_destroyed_in_response_still_untaps_its_equipped_creature() {
    assert!(cloak_destroyed_in_response(CloakHistory::EquippedOnce));
}

#[test]
fn cloak_reequipped_after_a_blink_then_destroyed_untaps_the_returned_creature() {
    assert!(cloak_destroyed_in_response(
        CloakHistory::BlinkedThenReequipped
    ));
}

/// Review probe N1: a re-attached host that then transformed is still provably
/// the equipped creature — the attachment edge, not a timestamp, is the proof.
#[test]
fn cloak_reequipped_host_transformed_then_cloak_destroyed_untaps_it() {
    assert!(cloak_destroyed_in_response(
        CloakHistory::BlinkedReequippedThenTransformed
    ));
}

/// CR 608.2h (PutCounter on the host): Ephara's Enlightenment, "When this Aura
/// enters, put a +1/+1 counter on enchanted creature." The Aura is returned to
/// hand in response; its departure record says what it was attached to, and
/// that creature never moved, so it gets the counter.
#[test]
fn ephara_s_enlightenment_bounced_in_response_still_counters_its_host() {
    let (mut runner, aura, host, _) = aura_etb_on_stack(
        "Ephara's Enlightenment",
        EPHARAS_ENLIGHTENMENT,
        2,
        Bystander::Absent,
    );
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), aura, Zone::Hand, &mut events);
    runner.advance_until_stack_empty();
    let s = runner.state();
    assert_eq!(s.objects[&aura].zone, Zone::Hand, "reach: the Aura left");
    assert_eq!(
        s.objects[&host]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the creature the Aura was attached to gets the counter"
    );
}

/// When the host moved, relative to the source's departure.
#[derive(Clone, Copy)]
enum HostBlink {
    BeforeTheAuraLeft,
    AfterTheAuraLeft,
}

/// Negative controls for the departed-source admission: Last Voyage of the
/// _____'s leaves-the-battlefield trigger, with the host blinked before or after
/// the Aura left. Either way the returned creature is a new object the Aura was
/// never attached to, so nothing is sacrificed.
fn last_voyage_with_host_blinked(blink_at: HostBlink) -> (GameRunner, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = aura_on_battlefield(&mut sc, P0, "Last Voyage of the _____", LAST_VOYAGE);
    let mut runner = sc.build();
    attach(&mut runner, aura, host);
    let mut events = Vec::new();
    if let HostBlink::BeforeTheAuraLeft = blink_at {
        blink(&mut runner, host);
    }
    engine::game::zones::move_to_zone(runner.state_mut(), aura, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    assert!(
        runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }),
        "reach: the leaves-the-battlefield trigger went on the stack"
    );
    if let HostBlink::AfterTheAuraLeft = blink_at {
        blink(&mut runner, host);
    }
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the trigger resolved"
    );
    (runner, host)
}

#[test]
fn last_voyage_host_blinked_before_the_aura_left_sacrifices_nothing() {
    let (runner, host) = last_voyage_with_host_blinked(HostBlink::BeforeTheAuraLeft);
    assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
}

#[test]
fn last_voyage_host_blinked_after_the_aura_left_sacrifices_nothing() {
    let (runner, host) = last_voyage_with_host_blinked(HostBlink::AfterTheAuraLeft);
    assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
}

/// Base-untargeted, cost-moved source, disclosed limit: "{3}{W}{W}, Sacrifice
/// this Aura: Exile enchanted creature." The cost moves the Aura before the
/// ability is put on the stack, so no record proves its host and nothing is
/// exiled — the same as base, whose filter scan found nothing either.
#[test]
fn choking_restraints_sacrificed_as_cost_exiles_nothing() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let aura = aura_on_battlefield(
        &mut sc,
        P0,
        "Choking Restraints",
        "Enchant creature\nEnchanted creature can't attack or block.\n{3}{W}{W}, Sacrifice this Aura: Exile enchanted creature.",
    );
    sc.with_mana_pool(P0, mana(ManaType::White, 5));
    let mut runner = sc.build();
    attach(&mut runner, aura, host);
    let index = ability_index(&runner, aura, |e| matches!(e, Effect::ChangeZone { .. }));
    runner
        .act(GameAction::ActivateAbility {
            source_id: aura,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the ability is activated");
    for _ in 0..4 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, .. } => {
                runner
                    .act(GameAction::SelectCards { cards: choices })
                    .expect("the Aura is sacrificed");
            }
            _ => break,
        }
    }
    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Graveyard,
        "reach: the cost sacrificed the Aura"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the pending ability resolved"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: resolution returned to priority"
    );
    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Battlefield,
        "no provable host, so nothing is exiled (as at base)"
    );
}

/// Base-untargeted, cost-moved source, disclosed limit: Krasis Incubation,
/// "{1}{G}{U}, Return this Aura to its owner's hand: Put two +1/+1 counters on
/// enchanted creature." No record proves the host, so no counters — the same
/// as base.
#[test]
fn krasis_incubation_returned_as_cost_counters_nothing() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = aura_on_battlefield(
        &mut sc,
        P0,
        "Krasis Incubation",
        "Enchant creature\nEnchanted creature can't attack or block, and its activated abilities can't be activated.\n{1}{G}{U}, Return this Aura to its owner's hand: Put two +1/+1 counters on enchanted creature.",
    );
    sc.with_mana_pool(P0, mana(ManaType::Colorless, 3));
    let mut runner = sc.build();
    attach(&mut runner, aura, host);
    for unit in mana(ManaType::Green, 1)
        .into_iter()
        .chain(mana(ManaType::Blue, 1))
    {
        let _ = runner.state_mut().add_mana_to_pool(P0, unit);
    }
    let index = ability_index(&runner, aura, |e| matches!(e, Effect::PutCounter { .. }));
    runner
        .act(GameAction::ActivateAbility {
            source_id: aura,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the ability is activated");
    for _ in 0..4 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, .. } => {
                runner
                    .act(GameAction::SelectCards { cards: choices })
                    .expect("the Aura is returned");
            }
            _ => break,
        }
    }
    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Hand,
        "reach: the cost returned the Aura"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the pending ability resolved"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: resolution returned to priority"
    );
    assert_eq!(
        runner.state().objects[&host]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "no provable host, so no counters (as at base)"
    );
}

/// Whether the Helm was attached to anything as it left.
#[derive(Clone, Copy)]
enum HelmFate {
    StaysUnattached,
    DestroyedUnattached,
}

/// Review probe N2: Helm of the Host's beginning-of-combat trigger is pending;
/// the equipped creature is blinked (the Helm is now unattached). An explicit
/// "attached to nothing" is final: no earlier host is read, so no token is
/// created, whether the Helm then stays on the battlefield or is destroyed.
fn helm_of_the_host_with_its_host_blinked(fate: HelmFate) -> GameRunner {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let bears = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let helm = sc
        .add_creature(P0, "Helm of the Host", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(HELM_OF_THE_HOST)
        .id();
    let mut runner = sc.build();
    attach(&mut runner, helm, bears);
    runner.advance_to_phase(Phase::BeginCombat);
    assert!(
        runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == helm)
        }),
        "reach: the Helm's combat trigger is on the stack"
    );
    blink(&mut runner, bears);
    assert_eq!(
        runner.state().objects[&helm].attached_to,
        None,
        "reach: the Helm is unattached"
    );
    if let HelmFate::DestroyedUnattached = fate {
        let mut events = Vec::new();
        engine::game::zones::move_to_zone(runner.state_mut(), helm, Zone::Graveyard, &mut events);
        assert!(
            runner
                .state()
                .zone_changes_this_turn
                .iter()
                .rev()
                .find(|record| record.object_id == helm)
                .is_some_and(|record| record.attached_to.is_none()),
            "reach: the Helm's departure records no attachment"
        );
    }
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the pending ability resolved"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: resolution returned to priority"
    );
    runner
}

#[test]
fn helm_of_the_host_destroyed_unattached_copies_nothing() {
    let runner = helm_of_the_host_with_its_host_blinked(HelmFate::DestroyedUnattached);
    let s = runner.state();
    assert!(
        !s.battlefield.iter().any(|id| s.objects[id].is_token),
        "no token is created"
    );
}

#[test]
fn helm_of_the_host_left_unattached_copies_nothing() {
    let runner = helm_of_the_host_with_its_host_blinked(HelmFate::StaysUnattached);
    let s = runner.state();
    assert!(
        !s.battlefield.iter().any(|id| s.objects[id].is_token),
        "no token is created"
    );
}

/// Review probe F3, disclosed limit (fail closed): Bind the Monster's ETB is
/// pending on P1's 2/2 grown to 5/5 with three +1/+1 counters, and P1 blinks the
/// creature (a new 2/2). The Aura, now unattached, has no provable host:
/// nothing is tapped and no damage is dealt — in particular the returned 2/2 is
/// never read as "it" (base: the same; the targeted ETB lost its target).
#[test]
fn bind_the_monster_host_blinked_deals_nothing() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    sc.with_counter(host, CounterType::Plus1Plus1, 3);
    let aura = aura_in_hand(&mut sc, P0, "Bind the Monster", BIND_THE_MONSTER);
    let mut runner = sc.build();
    runner.cast(aura).target_object(host).commit();
    runner.act(GameAction::PassPriority).expect("pass");
    for _ in 0..8 {
        if runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }),
        "reach: the ETB trigger is on the stack"
    );
    assert_eq!(
        runner.state().objects[&host].power,
        Some(5),
        "reach: the host is 5/5 as the trigger waits"
    );
    let life_before = runner.state().players[P0.0 as usize].life;
    blink(&mut runner, host);
    let mut events = Vec::new();
    while !runner.state().stack.is_empty() {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("priority passes")
                .events,
        );
    }
    let s = runner.state();
    assert_eq!(
        s.objects[&host].power,
        Some(2),
        "reach: the returned creature is a new 2/2"
    );
    assert!(
        !s.objects[&host].tapped,
        "the returned creature is not tapped"
    );
    assert_eq!(
        s.players[P0.0 as usize].life, life_before,
        "no provable host, so neither the departed nor the returned creature deals damage"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::DamageDealt { .. })),
        "no damage event at all"
    );
}

/// HOSTILE: the Aura leaves the battlefield while its ETB is on the stack. The
/// instruction uses the Aura's last known information (CR 608.2h), so the
/// creature it was attached to is still tapped.
#[test]
fn aura_bounced_in_response_still_taps_its_last_known_host() {
    let (mut runner, aura, host, _) = claustrophobia_etb_on_stack(Bystander::Absent);
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), aura, Zone::Hand, &mut events);
    runner.advance_until_stack_empty();
    let s = runner.state();
    assert_eq!(
        s.objects[&aura].zone,
        Zone::Hand,
        "reach: the Aura left the battlefield"
    );
    assert!(
        s.objects[&host].tapped,
        "CR 608.2h: 'enchanted creature' is the creature the Aura was last attached to"
    );
}

/// HOSTILE: the host leaves the battlefield while the ETB is on the stack, and
/// another creature on that side has an unrelated Aura attached. The ETB taps
/// nothing; in particular it never re-resolves "enchanted creature" to some
/// other creature that happens to wear an Aura.
#[test]
fn host_gone_before_etb_resolves_taps_nothing() {
    let (mut runner, aura, host, bystander) =
        claustrophobia_etb_on_stack(Bystander::WearingAnotherAura);
    let bystander = bystander.expect("bystander staged");
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Hand, &mut events);
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the pending ability resolved"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: resolution returned to priority"
    );
    let s = runner.state();
    assert_eq!(
        s.objects[&host].zone,
        Zone::Hand,
        "reach: the host left the battlefield"
    );
    assert!(
        !s.objects[&host].tapped,
        "the departed host is not tapped in its new zone"
    );
    assert!(
        !s.objects[&bystander].tapped,
        "an unrelated enchanted creature is not 'enchanted creature' for Claustrophobia"
    );
    assert_ne!(
        s.objects[&aura].zone,
        Zone::Battlefield,
        "reach: CR 303.4c put the now-unattached Aura into the graveyard"
    );
}

/// Activated sibling: "{U}: Untap enchanted creature." works on a host with
/// shroud — Pemmin's Aura's own third ability grants exactly that.
#[test]
fn pemmins_aura_untaps_a_shrouded_host() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P0, "Shrouded Bear", 2, 2)
        .with_keyword(Keyword::Shroud)
        .id();
    let aura = aura_on_battlefield(&mut sc, P0, "Pemmin's Aura", PEMMINS_AURA);
    sc.with_mana_pool(P0, pool(1, 0));
    let mut runner = sc.build();
    attach(&mut runner, aura, host);
    set_tapped(&mut runner, host, true);

    let index = ability_index(&runner, aura, |e| matches!(e, Effect::SetTapState { .. }));
    runner
        .act(GameAction::ActivateAbility {
            source_id: aura,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CR 115.10a: 'untap enchanted creature' does not target, so shroud cannot stop it");
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().objects[&host].tapped,
        "the shrouded enchanted creature is untapped"
    );
}

/// Whether the source starts attached to the board's host.
#[derive(Clone, Copy)]
enum AttachmentStart {
    AttachedToHost,
    Unattached,
}

fn cloak_board(start: AttachmentStart) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P0, "Shrouded Bear", 2, 2)
        .with_keyword(Keyword::Shroud)
        .id();
    let other = sc.add_creature(P0, "Equipped Elsewhere", 2, 2).id();
    let cloak = sc
        .add_creature(P0, "Cloak of the Philosopher", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(CLOAK_OF_THE_PHILOSOPHER)
        .id();
    let other_equipment = sc
        .add_creature(P0, "Other Blade", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text("Equipped creature gets +1/+1.\nEquip {1}")
        .id();
    sc.with_mana_pool(P0, pool(0, 2));
    let mut runner = sc.build();
    match start {
        AttachmentStart::AttachedToHost => attach(&mut runner, cloak, host),
        AttachmentStart::Unattached => {}
    }
    attach(&mut runner, other_equipment, other);
    set_tapped(&mut runner, host, true);
    set_tapped(&mut runner, other, true);
    (runner, cloak, host, other)
}

/// Equipment sibling: "{2}: Untap equipped creature." on a shrouded host.
#[test]
fn cloak_of_the_philosopher_untaps_a_shrouded_equipped_creature() {
    let (mut runner, cloak, host, other) = cloak_board(AttachmentStart::AttachedToHost);
    let index = ability_index(&runner, cloak, |e| matches!(e, Effect::SetTapState { .. }));
    runner
        .act(GameAction::ActivateAbility {
            source_id: cloak,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CR 115.10a: 'untap equipped creature' does not target");
    runner.advance_until_stack_empty();
    let s = runner.state();
    assert!(
        !s.objects[&host].tapped,
        "the equipped creature is untapped"
    );
    assert!(
        s.objects[&other].tapped,
        "a creature equipped by a different Equipment is untouched"
    );
}

/// CR 301.5c + CR 301.5f: an unattached Equipment has no equipped creature, so
/// the ability resolves and untaps nothing (it never falls back to "some
/// creature with an Equipment attached").
#[test]
fn unattached_cloak_untaps_nothing() {
    let (mut runner, cloak, host, other) = cloak_board(AttachmentStart::Unattached);
    let index = ability_index(&runner, cloak, |e| matches!(e, Effect::SetTapState { .. }));
    runner
        .act(GameAction::ActivateAbility {
            source_id: cloak,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the ability names no target, so it can be activated");
    runner.advance_until_stack_empty();
    let s = runner.state();
    assert!(s.stack.is_empty(), "reach: the ability resolved");
    assert!(
        s.objects[&host].tapped,
        "no creature is equipped by the Cloak"
    );
    assert!(
        s.objects[&other].tapped,
        "another Equipment's creature is untouched"
    );
}

/// LKI sibling (Destroy): "When this Aura leaves the battlefield, destroy
/// enchanted creature." — the Aura is already gone when the trigger resolves.
#[test]
fn parallax_dementia_leaves_trigger_destroys_last_known_host() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P1, "Shrouded Bear", 2, 2)
        .with_keyword(Keyword::Shroud)
        .id();
    let aura = aura_on_battlefield(&mut sc, P0, "Parallax Dementia", PARALLAX_DEMENTIA_LEAVES);
    let mut runner = sc.build();
    attach(&mut runner, aura, host);
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), aura, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    assert!(
        runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }),
        "reach: the leaves-the-battlefield trigger went on the stack"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Graveyard,
        "CR 608.2h + CR 115.10a: the shrouded creature the Aura was attached to is destroyed"
    );
}

fn has_regeneration_shield(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .replacement_definitions
        .iter_unchecked()
        .any(|r| r.description.as_deref() == Some("Regenerate"))
}

/// Nurturing Licid's "{G}: Regenerate enchanted creature." in each of its forms.
/// As an Aura it shields the creature it is attached to. As a creature it is
/// attached to nothing, so there is no enchanted creature: the ability must not
/// fall back to "every creature that has some Aura attached".
fn nurturing_licid_case(start: AttachmentStart) -> (bool, bool) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc.add_creature(P0, "Licid Host", 2, 2).id();
    let bystander = sc.add_creature(P1, "Enchanted Bystander", 2, 2).id();
    let other_aura = aura_on_battlefield(&mut sc, P1, "Unrelated Aura", "Enchant creature");
    let licid = sc
        .add_creature_from_oracle(P0, "Nurturing Licid", 1, 1, NURTURING_LICID)
        .id();
    sc.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])],
    );
    let mut runner = sc.build();
    attach(&mut runner, other_aura, bystander);
    if let AttachmentStart::AttachedToHost = start {
        // The state its first ability leaves it in: an Aura enchantment, not a
        // creature (written to the base types so the layer pass keeps it).
        let obj = runner.state_mut().objects.get_mut(&licid).unwrap();
        obj.card_types
            .core_types
            .retain(|t| *t != engine::types::card_type::CoreType::Creature);
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Enchantment);
        obj.card_types.subtypes = vec!["Aura".to_string()];
        obj.base_card_types = obj.card_types.clone();
        attach(&mut runner, licid, host);
    }
    let index = ability_index(&runner, licid, |e| matches!(e, Effect::Regenerate { .. }));
    runner
        .act(GameAction::ActivateAbility {
            source_id: licid,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("'regenerate enchanted creature' names no target, so it can be activated");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the ability resolved"
    );
    (
        has_regeneration_shield(&runner, host),
        has_regeneration_shield(&runner, bystander),
    )
}

#[test]
fn nurturing_licid_as_aura_regenerates_its_host() {
    let (host, bystander) = nurturing_licid_case(AttachmentStart::AttachedToHost);
    assert!(
        host,
        "the creature the Licid enchants gets the regeneration shield"
    );
    assert!(
        !bystander,
        "a creature wearing a different Aura gets nothing"
    );
}

#[test]
fn nurturing_licid_as_creature_regenerates_nothing() {
    let (host, bystander) = nurturing_licid_case(AttachmentStart::Unattached);
    assert!(!host, "reach: the Licid enchants nothing");
    assert!(
        !bystander,
        "CR 303.4b: an unattached Licid has no enchanted creature, so a creature wearing an \
         unrelated Aura is not regenerated"
    );
}

/// RemoveFromCombat sibling with a controller anaphor: Observed Stasis (Flash),
/// "When this Aura enters, remove enchanted creature from combat. Then draw a
/// card for each tapped creature its controller controls." — flashed in on an
/// attacking ward creature. The Aura's own "loses all abilities" strips ward
/// before its ETB goes on the stack, so this pins the bound host (removed from
/// combat) and the "its controller" count rather than the ward count.
#[test]
fn observed_stasis_removes_attacking_host_from_combat_and_counts_its_controller() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P1, "Patchwork Automaton", 1, 1)
        .with_keyword(Keyword::Ward(WardCost::Mana(ManaCost::generic(2))))
        .id();
    sc.add_creature(P1, "Second Creature", 2, 2);
    let aura = aura_in_hand(&mut sc, P0, "Observed Stasis", OBSERVED_STASIS);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let mut runner = sc.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(host, AttackTarget::Player(P0))])
        .expect("P1 attacks with the ward creature");
    let attacking = |runner: &GameRunner| {
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == host))
    };
    assert!(attacking(&runner), "reach: the host is attacking");
    assert!(
        runner.state().objects[&host].tapped,
        "reach: attacking tapped it"
    );
    while runner.state().priority_player != P0 {
        runner
            .act(GameAction::PassPriority)
            .expect("P1 passes priority");
    }
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    // CR 500.5: the pool emptied as the steps changed; float ward's {2} now.
    for unit in pool(0, 2) {
        let _ = runner.state_mut().add_mana_to_pool(P0, unit);
    }

    let d = cast_aura_and_resolve(&mut runner, aura, host, CastPaymentMode::Auto);
    let s = runner.state();
    assert_eq!(
        s.objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "reach: Observed Stasis resolved and attached"
    );
    assert_eq!(
        d.ward_triggers(),
        1,
        "ward triggers once, for the Aura spell"
    );
    assert!(
        !attacking(&runner),
        "the enchanted creature is removed from combat"
    );
    // The Aura left the hand (-1); one tapped creature under P1 draws one (+1).
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before,
        "one card is drawn for the one tapped creature the host's controller controls"
    );
}

/// Disclosed limit (fail closed): Observed Stasis's ETB is pending on a
/// creature P1 controls but P0 owns, and the creature is blinked, returning as
/// a new object under its owner's control. The Aura, now unattached, has no
/// provable host, so nothing is removed from combat and "its controller" names
/// no one: P0 draws nothing — neither for P1's tapped creatures nor for the
/// returned creature's new controller's (base: the same).
#[test]
fn observed_stasis_host_blinked_draws_nothing() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P0, "Borrowed Bears", 2, 2)
        .controlled_by(P1)
        .id();
    let tapped = [
        sc.add_creature(P1, "Tapped One", 1, 1).id(),
        sc.add_creature(P1, "Tapped Two", 1, 1).id(),
        sc.add_creature(P0, "Tapped Mine", 1, 1).id(),
    ];
    let aura = aura_in_hand(&mut sc, P0, "Observed Stasis", OBSERVED_STASIS);
    sc.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let mut runner = sc.build();
    for id in tapped {
        set_tapped(&mut runner, id, true);
    }
    runner.cast(aura).target_object(host).commit();
    runner.act(GameAction::PassPriority).expect("pass");
    for _ in 0..8 {
        if runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }),
        "reach: the ETB trigger is on the stack"
    );
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    blink(&mut runner, host);
    assert_eq!(
        runner.state().objects[&host].controller,
        P0,
        "reach: the returned creature is a new object under its owner's control"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "reach: the pending ability resolved"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach: resolution returned to priority"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before,
        "no provable host, so no card is drawn"
    );
}

/// Sacrifice sibling (LKI): Last Voyage of the _____, "When this Aura leaves the
/// battlefield, sacrifice enchanted creature." Regression guard: `Sacrifice`
/// never announced a slot for this shape, so it pins that the resolution-time
/// binding keeps reading the departed Aura's last known host.
#[test]
fn last_voyage_leaves_trigger_sacrifices_last_known_host() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let host = sc
        .add_creature(P0, "Shrouded Bear", 2, 2)
        .with_keyword(Keyword::Shroud)
        .id();
    let aura = aura_on_battlefield(&mut sc, P0, "Last Voyage of the _____", LAST_VOYAGE);
    let mut runner = sc.build();
    attach(&mut runner, aura, host);
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), aura, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    assert!(
        runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }),
        "reach: the leaves-the-battlefield trigger went on the stack"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Graveyard,
        "CR 608.2h: the creature the Aura was attached to is sacrificed"
    );
}

/// SYNTHETIC (fail-closed guard): a non-Aura source that is attached to nothing
/// names no enchanted creature, so "Enchanted creature phases out" affects
/// nothing. PhaseOut's resolver falls back to a filter scan when it has no
/// targets; without the no-host skip it would phase out an unrelated creature
/// that happens to wear an Aura.
#[test]
fn hostless_phase_out_affects_no_unrelated_enchanted_creature() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let source = sc
        .add_creature_from_oracle(
            P0,
            "Unattached Phaser",
            1,
            1,
            "{U}{U}: Enchanted creature phases out.",
        )
        .id();
    let bystander = sc.add_creature(P1, "Enchanted Bystander", 2, 2).id();
    let other_aura = aura_on_battlefield(&mut sc, P1, "Unrelated Aura", "Enchant creature");
    sc.with_mana_pool(P0, mana(ManaType::Blue, 2));
    let mut runner = sc.build();
    attach(&mut runner, other_aura, bystander);
    let index = ability_index(&runner, source, |e| matches!(e, Effect::PhaseOut { .. }));
    assert_eq!(
        runner.state().objects[&source].abilities[index].target_choice_timing,
        TargetChoiceTiming::Resolution,
        "reach: the host reference is resolution-timed"
    );
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the ability names no target, so it can be activated");
    runner.advance_until_stack_empty();
    let s = runner.state();
    assert!(s.stack.is_empty(), "reach: the ability resolved");
    assert!(
        s.objects[&bystander].is_phased_in(),
        "a creature wearing an unrelated Aura is not the source's enchanted creature"
    );
}

/// SHAPE (building block): the literal word "target" is the only thing that
/// makes an Aura's own instruction stack-targeted (CR 115.10a).
#[test]
fn host_reference_is_resolution_timed_and_literal_target_stays_stack() {
    let mut sc = GameScenario::new();
    let described = aura_on_battlefield(
        &mut sc,
        P0,
        "Described",
        "Enchant creature\n{1}: Tap enchanted creature.",
    );
    let targeted = aura_on_battlefield(
        &mut sc,
        P0,
        "Targeted",
        "Enchant creature\n{1}: Tap target creature.",
    );
    let runner = sc.build();
    let timing = |id: ObjectId| runner.state().objects[&id].abilities[0].target_choice_timing;
    assert_eq!(timing(described), TargetChoiceTiming::Resolution);
    assert_eq!(timing(targeted), TargetChoiceTiming::Stack);
}

/// The kind of permanent a host-referring source is.
#[derive(Clone, Copy)]
enum SourceKind {
    Aura,
    Equipment,
}

fn source_from_text(sc: &mut GameScenario, name: &str, text: &str, kind: SourceKind) -> ObjectId {
    match kind {
        SourceKind::Aura => aura_on_battlefield(sc, P0, name, text),
        SourceKind::Equipment => sc
            .add_creature(P0, name, 0, 0)
            .as_artifact()
            .with_subtypes(vec!["Equipment"])
            .from_oracle_text(text)
            .id(),
    }
}

fn add_sources(
    sc: &mut GameScenario,
    cards: &[(&'static str, &'static str, SourceKind)],
) -> Vec<(&'static str, ObjectId)> {
    cards
        .iter()
        .map(|(name, text, kind)| (*name, source_from_text(sc, name, text, *kind)))
        .collect()
}

/// The target-choice timing of every node of `source`'s abilities and triggers
/// that names its "enchanted/equipped X".
fn host_reference_timings(runner: &GameRunner, source: ObjectId) -> Vec<TargetChoiceTiming> {
    let object = &runner.state().objects[&source];
    let mut definitions: Vec<&AbilityDefinition> = object.abilities.iter().collect();
    definitions.extend(
        object
            .trigger_definitions
            .iter_unchecked()
            .filter_map(|entry| entry.definition.execute.as_deref()),
    );
    let mut timings = Vec::new();
    let mut pending = definitions;
    while let Some(current) = pending.pop() {
        if current
            .effect
            .target_filter()
            .is_some_and(|filter| filter.contains_source_attachment_host())
        {
            timings.push(current.target_choice_timing);
        }
        pending.extend(current.sub_ability.as_deref());
        pending.extend(current.else_ability.as_deref());
    }
    timings
}

/// SHAPE (scope): only a host instruction that is the whole ability stops
/// targeting its host. A chain where other instructions precede or follow it,
/// and a donor shape, keep base's stack-time target; an instruction that was
/// already untargeted at base keeps that.
#[test]
fn only_a_lone_host_instruction_stops_targeting() {
    let lone = [
        ("Malfunction", MALFUNCTION, SourceKind::Aura),
        ("Claustrophobia", CLAUSTROPHOBIA, SourceKind::Aura),
        (
            "Parallax Dementia",
            PARALLAX_DEMENTIA_LEAVES,
            SourceKind::Aura,
        ),
        ("Pemmin's Aura", PEMMINS_AURA, SourceKind::Aura),
        ("Stamina", STAMINA, SourceKind::Aura),
        (
            "Cloak of the Philosopher",
            CLOAK_OF_THE_PHILOSOPHER,
            SourceKind::Equipment,
        ),
    ];
    let kept = [
        ("Bind the Monster", BIND_THE_MONSTER, SourceKind::Aura),
        ("Frenzied Fugue", FRENZIED_FUGUE, SourceKind::Aura),
        ("Marooned", MAROONED, SourceKind::Aura),
        ("Observed Stasis", OBSERVED_STASIS, SourceKind::Aura),
        ("Captivating Glance", CAPTIVATING_GLANCE, SourceKind::Aura),
        ("Fall from Favor", FALL_FROM_FAVOR, SourceKind::Aura),
        ("Assault Suit", ASSAULT_SUIT, SourceKind::Equipment),
        ("Sword of the Paruns", SWORD_OF_THE_PARUNS, SourceKind::Equipment),
        ("Helm of the Host", HELM_OF_THE_HOST, SourceKind::Equipment),
        (
            "Copy Rig",
            "{1}: This Equipment becomes a copy of equipped creature until end of turn.\nEquip {1}",
            SourceKind::Equipment,
        ),
        (
            "Ability Rig",
            "{1}: This Equipment gains all activated abilities of equipped creature until end of turn.\nEquip {1}",
            SourceKind::Equipment,
        ),
    ];
    let base_untargeted = [(
        "Ephara's Enlightenment",
        EPHARAS_ENLIGHTENMENT,
        SourceKind::Aura,
    )];
    let mut sc = GameScenario::new();
    let lone = add_sources(&mut sc, &lone);
    let kept = add_sources(&mut sc, &kept);
    let base_untargeted = add_sources(&mut sc, &base_untargeted);
    let runner = sc.build();
    let expect = |cards: &[(&str, ObjectId)], timing: TargetChoiceTiming| {
        for (name, id) in cards {
            let timings = host_reference_timings(&runner, *id);
            assert!(!timings.is_empty(), "reach: {name} names its host");
            assert!(
                timings.iter().all(|t| *t == timing),
                "{name}: expected {timing:?}, got {timings:?}"
            );
        }
    };
    expect(&lone, TargetChoiceTiming::Resolution);
    expect(&kept, TargetChoiceTiming::Stack);
    expect(&base_untargeted, TargetChoiceTiming::Resolution);
}

/// The target-choice timing of every node in `value` (parsed abilities as JSON)
/// whose slot filter names the source's host; an absent field is the
/// serialized default, `Stack`.
fn host_reference_timings_in_json(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            let names_host = map
                .get("effect")
                .and_then(|effect| effect.get("target"))
                .is_some_and(|target| {
                    let text = target.to_string();
                    text.contains("EnchantedBy") || text.contains("EquippedBy")
                });
            if names_host {
                out.push(
                    map.get("target_choice_timing")
                        .and_then(|t| t.as_str())
                        .unwrap_or("Stack")
                        .to_string(),
                );
            }
            map.values()
                .for_each(|v| host_reference_timings_in_json(v, out));
        }
        serde_json::Value::Array(items) => items
            .iter()
            .for_each(|v| host_reference_timings_in_json(v, out)),
        _ => {}
    }
}

fn parsed_host_timings<T: serde::Serialize>(parsed: &T) -> Vec<String> {
    let mut out = Vec::new();
    host_reference_timings_in_json(&serde_json::to_value(parsed).unwrap(), &mut out);
    out
}

/// Converted GainControl control: Wellspring, "When this Aura enters, gain
/// control of enchanted land until end of turn." — the lone instruction binds
/// the host at resolution and P0 controls the land.
#[test]
fn wellspring_gains_control_of_its_land() {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let land = sc.add_basic_land(P1, engine::types::mana::ManaColor::Green);
    let aura = aura_in_hand(
        &mut sc,
        P0,
        "Wellspring",
        "Enchant land\nWhen this Aura enters, gain control of enchanted land until end of turn.\nAt the beginning of your upkeep, untap enchanted land. You gain control of that land until end of turn.",
    );
    let mut runner = sc.build();
    runner.cast(aura).target_object(land).commit();
    runner.act(GameAction::PassPriority).expect("pass");
    for _ in 0..8 {
        if runner.state().stack.iter().any(|e| {
            matches!(&e.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == aura)
        }) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        runner.state().stack.iter().any(|e| matches!(
            &e.kind,
            StackEntryKind::TriggeredAbility { source_id, ability, .. }
                if *source_id == aura
                    && ability.target_choice_timing == TargetChoiceTiming::Resolution
        )),
        "reach: the converted ETB is on the stack"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&land].controller,
        P0,
        "P0 gains control of the enchanted land"
    );
}

/// SHAPE (scope, R6-1): a document-level modal spell's modes are parts of one
/// modal ability, never a whole ability, so a lone host instruction in a mode
/// keeps its stack-time target. The same instruction as a whole spell converts.
#[test]
fn a_modal_spell_mode_keeps_its_host_target() {
    let sorcery = ["Sorcery".to_string()];
    let modal = engine::parser::oracle::parse_oracle_text(
        "Choose one —\n• Tap enchanted creature.\n• Draw a card.",
        "Modal Probe",
        &[],
        &sorcery,
        &[],
    );
    assert!(modal.modal.is_some(), "reach: parsed as a modal spell");
    let timings = parsed_host_timings(&modal.abilities);
    assert!(!timings.is_empty(), "reach: a mode names the host");
    assert!(
        timings.iter().all(|t| t == "Stack"),
        "modal modes keep their target: {timings:?}"
    );
    let lone = engine::parser::oracle::parse_oracle_text(
        "Tap enchanted creature.",
        "Lone Probe",
        &[],
        &sorcery,
        &[],
    );
    assert_eq!(
        parsed_host_timings(&lone.abilities),
        vec!["Resolution".to_string()],
        "control: the same instruction as the whole ability converts"
    );
}

/// SHAPE (scope, R6-1): an ability quoted inside a grant is not the card's own
/// ability; only the card's own abilities and triggers convert, so a granted
/// trigger or activated ability keeps its stack-time target.
#[test]
fn a_granted_ability_keeps_its_host_target() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        "Enchant creature\nEnchanted creature has \"Whenever this creature attacks, tap enchanted creature.\" and \"{T}: Untap equipped creature.\"",
        "Grant Probe",
        &["Enchant creature".to_string()],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    let timings = parsed_host_timings(&parsed.statics);
    assert_eq!(
        timings.len(),
        2,
        "reach: both the granted trigger and the granted activated ability name a host: {timings:?}"
    );
    assert!(
        timings.iter().all(|t| t == "Stack"),
        "granted abilities keep their target: {timings:?}"
    );
    assert!(
        parsed_host_timings(&parsed.triggers).is_empty()
            && parsed_host_timings(&parsed.abilities).is_empty(),
        "reach: the host references live only inside the grant"
    );
}

/// SHAPE (scope, R7-1): a CR 614.15 self-replacement paragraph ("Fateful hour —
/// If …, … instead.") is lowered as its own document item and only later folded
/// into the preceding ability as a `ConditionInstead` follower. Conversion is
/// judged on the composed ability, so neither host instruction converts; the
/// same instruction alone still does.
#[test]
fn a_host_instruction_with_a_self_replacement_paragraph_keeps_its_target() {
    let sorcery = ["Sorcery".to_string()];
    let composed = engine::parser::oracle::parse_oracle_text(
        "Tap enchanted creature.\nFateful hour — If you have 5 or less life, untap enchanted creature instead.",
        "Override Probe",
        &[],
        &sorcery,
        &[],
    );
    assert_eq!(
        composed.abilities.len(),
        1,
        "reach: the paragraph folds into one ability"
    );
    let root = &composed.abilities[0];
    let follower = root
        .sub_ability
        .as_deref()
        .expect("reach: the override is the root's follower");
    assert!(
        matches!(
            follower.condition,
            Some(engine::types::ability::AbilityCondition::ConditionInstead { .. })
        ),
        "reach: the follower is the self-replacement override"
    );
    let json = serde_json::to_string(&composed.abilities).unwrap();
    assert!(
        !json.contains("Unimplemented"),
        "reach: fully parsed: {json}"
    );
    assert_eq!(
        parsed_host_timings(&composed.abilities),
        vec!["Stack".to_string(), "Stack".to_string()],
        "both host instructions of the composed ability keep their target"
    );
    let lone = engine::parser::oracle::parse_oracle_text(
        "Tap enchanted creature.",
        "Lone Probe",
        &[],
        &sorcery,
        &[],
    );
    assert_eq!(
        parsed_host_timings(&lone.abilities),
        vec!["Resolution".to_string()],
        "control: the same instruction as the whole ability converts"
    );
}

/// The Reviewer's r5 probes, and two neighbours, as ETBs whose host instruction
/// is followed by other instructions: not converted, so they keep base's
/// stack-time target.
#[derive(Clone, Copy)]
enum ChainedEtb {
    /// P5-1: "tap enchanted creature. You may put a +1/+1 counter on it.
    /// Create a Treasure token."
    OptionalCounterOnIt,
    /// P5-2: "tap enchanted creature. You gain life equal to its mana value.
    /// Create a Treasure token."
    LifeEqualToItsManaValue,
    /// P5-3: "tap enchanted creature. If you control an artifact, you gain 1
    /// life. Put a +1/+1 counter on it. Create a Treasure token."
    ConditionalLifeThenCounterOnIt,
    /// "tap enchanted creature. It deals 2 damage to you. Create a Treasure
    /// token."
    DependentDamage,
    /// Already untargeted at base (PutCounter): "put a +1/+1 counter on
    /// enchanted creature. Create a Treasure token."
    BaseUntargetedCounter,
}

/// What happens to the enchanted creature before the ETB resolves.
#[derive(Clone, Copy)]
enum HostFate {
    StaysPut,
    BlinkedInResponse,
}

#[derive(Debug, PartialEq, Eq)]
struct EtbOutcome {
    life_change: i32,
    treasures: usize,
    counters: u32,
    host_tapped: bool,
    optional_prompts: usize,
}

/// SYNTHETIC (Oracle-parsed) Aura with `shape`'s ETB, cast on P1's creature;
/// the host is worth MV 3 and the Aura MV 2, so a fallback read of the wrong
/// object shows in the life change.
fn chained_etb(shape: ChainedEtb, fate: HostFate) -> EtbOutcome {
    let body = match shape {
        ChainedEtb::OptionalCounterOnIt => {
            "tap enchanted creature. You may put a +1/+1 counter on it. Create a Treasure token."
        }
        ChainedEtb::LifeEqualToItsManaValue => {
            "tap enchanted creature. You gain life equal to its mana value. Create a Treasure token."
        }
        ChainedEtb::ConditionalLifeThenCounterOnIt => {
            "tap enchanted creature. If you control an artifact, you gain 1 life. Put a +1/+1 counter on it. Create a Treasure token."
        }
        ChainedEtb::DependentDamage => {
            "tap enchanted creature. It deals 2 damage to you. Create a Treasure token."
        }
        ChainedEtb::BaseUntargetedCounter => {
            "put a +1/+1 counter on enchanted creature. Create a Treasure token."
        }
    };
    let text = format!("Enchant creature\nWhen this Aura enters, {body}");
    let (mut runner, aura, host, _) = aura_etb_on_stack("Probe Charm", &text, 2, Bystander::Absent);
    runner.state_mut().objects.get_mut(&host).unwrap().mana_cost = ManaCost::generic(3);
    runner.state_mut().objects.get_mut(&aura).unwrap().mana_cost = ManaCost::generic(2);
    let expected_timing = match shape {
        ChainedEtb::BaseUntargetedCounter => TargetChoiceTiming::Resolution,
        ChainedEtb::OptionalCounterOnIt
        | ChainedEtb::LifeEqualToItsManaValue
        | ChainedEtb::ConditionalLifeThenCounterOnIt
        | ChainedEtb::DependentDamage => TargetChoiceTiming::Stack,
    };
    assert!(
        runner.state().stack.iter().any(|e| matches!(
            &e.kind,
            StackEntryKind::TriggeredAbility { source_id, ability, .. }
                if *source_id == aura && ability.target_choice_timing == expected_timing
        )),
        "reach: the ETB is on the stack with its head at {expected_timing:?}"
    );
    let life_before = runner.state().players[P0.0 as usize].life;
    if let HostFate::BlinkedInResponse = fate {
        blink(&mut runner, host);
    }
    let mut optional_prompts = 0;
    let mut settled = false;
    for _ in 0..64 {
        let action = match runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                settled = true;
                break;
            }
            // A paused optional instruction is answered even once the stack
            // is empty: the ETB is still resolving.
            WaitingFor::OptionalEffectChoice { .. } => {
                optional_prompts += 1;
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            ref other => panic!("unexpected prompt: {other:?}"),
        };
        runner.act(action).expect("the ETB resolves");
    }
    let s = runner.state();
    assert!(settled, "reach: the ETB finished resolving");
    assert!(s.stack.is_empty(), "reach: the stack is empty");
    assert!(
        matches!(s.waiting_for, WaitingFor::Priority { .. }),
        "reach: the game is back at priority, not paused mid-resolution: {:?}",
        s.waiting_for
    );
    EtbOutcome {
        life_change: s.players[P0.0 as usize].life - life_before,
        treasures: s
            .battlefield
            .iter()
            .filter(|id| s.objects[id].is_token && s.objects[id].name == "Treasure")
            .count(),
        counters: s.objects[&host]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        host_tapped: s.objects[&host].tapped,
        optional_prompts,
    }
}

/// P5-1, positive control: with the creature in place the targeted chain
/// resolves fully against it, as at base: tapped, the optional counter is
/// offered once and accepted, and the Treasure is created.
#[test]
fn optional_counter_chain_with_its_host_in_place() {
    assert_eq!(
        chained_etb(ChainedEtb::OptionalCounterOnIt, HostFate::StaysPut),
        EtbOutcome {
            life_change: 0,
            treasures: 1,
            counters: 1,
            host_tapped: true,
            optional_prompts: 1,
        }
    );
}

/// P5-1: the creature is blinked in response. The chain keeps base's target, so
/// with that target gone the ETB does nothing at all — no prompt, no counter on
/// the returned creature, no Treasure (CR 608.2b, as at base).
#[test]
fn optional_counter_chain_with_its_host_blinked_does_nothing() {
    assert_eq!(
        chained_etb(ChainedEtb::OptionalCounterOnIt, HostFate::BlinkedInResponse),
        EtbOutcome {
            life_change: 0,
            treasures: 0,
            counters: 0,
            host_tapped: false,
            optional_prompts: 0,
        }
    );
}

/// P5-2, positive control: "its mana value" is the creature's, 3.
#[test]
fn life_equal_to_its_mana_value_with_its_host_in_place() {
    let outcome = chained_etb(ChainedEtb::LifeEqualToItsManaValue, HostFate::StaysPut);
    assert_eq!(
        outcome.life_change, 3,
        "the enchanted creature's mana value"
    );
    assert_eq!(outcome.treasures, 1);
    assert!(outcome.host_tapped);
}

/// P5-2: blinked in response, nothing happens; in particular no life from the
/// Aura's or the returned creature's mana value.
#[test]
fn life_equal_to_its_mana_value_with_its_host_blinked_does_nothing() {
    let outcome = chained_etb(
        ChainedEtb::LifeEqualToItsManaValue,
        HostFate::BlinkedInResponse,
    );
    assert_eq!(outcome.life_change, 0);
    assert_eq!(outcome.treasures, 0);
    assert!(!outcome.host_tapped);
}

/// P5-3, positive control: with the creature in place the targeted chain
/// resolves against it; no artifact, so no life, and the Treasure is created.
#[test]
fn conditional_then_counter_chain_with_its_host_in_place() {
    let outcome = chained_etb(
        ChainedEtb::ConditionalLifeThenCounterOnIt,
        HostFate::StaysPut,
    );
    assert!(outcome.host_tapped, "the enchanted creature is tapped");
    assert_eq!(outcome.life_change, 0, "no artifact, so no life");
    assert_eq!(outcome.treasures, 1);
    assert_eq!(outcome.optional_prompts, 0);
}

/// P5-3: blinked in response, nothing happens.
#[test]
fn conditional_then_counter_chain_with_its_host_blinked_does_nothing() {
    assert_eq!(
        chained_etb(
            ChainedEtb::ConditionalLifeThenCounterOnIt,
            HostFate::BlinkedInResponse
        ),
        EtbOutcome {
            life_change: 0,
            treasures: 0,
            counters: 0,
            host_tapped: false,
            optional_prompts: 0,
        }
    );
}

/// "It deals 2 damage to you": blinked in response, nothing happens.
#[test]
fn dependent_damage_chain_with_its_host_blinked_does_nothing() {
    let outcome = chained_etb(ChainedEtb::DependentDamage, HostFate::BlinkedInResponse);
    assert_eq!(outcome.life_change, 0);
    assert_eq!(outcome.treasures, 0);
}

/// Base-untargeted (PutCounter) with the creature in place: same as base.
#[test]
fn base_untargeted_counter_then_treasure_with_its_host_in_place() {
    let outcome = chained_etb(ChainedEtb::BaseUntargetedCounter, HostFate::StaysPut);
    assert_eq!(
        outcome.counters, 1,
        "the enchanted creature gets the counter"
    );
    assert_eq!(outcome.treasures, 1);
}

/// Base-untargeted (PutCounter), creature blinked in response. Base put the
/// counter on the returned creature, a new object the Aura was never attached
/// to; now there is no provable host and the counter goes nowhere (the accepted
/// difference from base). The Treasure is created, as at base.
#[test]
fn base_untargeted_counter_then_treasure_with_its_host_blinked_counters_nothing() {
    let outcome = chained_etb(
        ChainedEtb::BaseUntargetedCounter,
        HostFate::BlinkedInResponse,
    );
    assert_eq!(outcome.counters, 0, "the returned creature gets no counter");
    assert_eq!(outcome.treasures, 1, "the Treasure is created, as at base");
}
