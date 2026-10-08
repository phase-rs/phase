//! CR 608.2d + CR 613.4c: "This creature gets +1/-1 or -1/+1 until end of turn"
//! — a disjunction of two alternative Layer 7c P/T modifications, one of which
//! the controller picks AS THE ABILITY RESOLVES.
//!
//! Brightling's own Oracle ruling states the timing: "you don't choose whether
//! Brightling gets +1/-1 or -1/+1 until that ability resolves."
//!
//! EIGHT cards print this shape — seven with literal inverse alternatives
//! (Brightling, Endling, Greater Morphling, Shorecrasher Elemental, Multiform
//! Wonder, Pemmin's Aura, Shaper Parasite) and Liliana of the Dark Realms with
//! the variable pair "+X/+X or -X/-X" under a where-clause.
//!
//! Before the fix `oracle_static::grammar::parse_pt_mod` discarded the nom
//! remainder, so " or -1/+1" evaporated and the ability lowered to a single,
//! silent, confident `Effect::Pump { +1, -1 }` — no choice offered, and wrong
//! half the time by construction because the two alternatives are inverses.
//!
//! This drives the REAL activate -> pay -> resolve pipeline and asserts BOTH
//! branches, so a fix that offers a choice but wires both branches to the same
//! modification cannot pass. Liliana adds the second failure mode the parse-only
//! tests cannot see: a branch whose X is not bound resolves as +0/+0, so her two
//! cases assert the MAGNITUDE the Swamp count supplies, not just the shape.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Verbatim Scryfall Oracle text.
const BRIGHTLING: &str = "{W}: This creature gains vigilance until end of turn.\n\
{W}: This creature gains lifelink until end of turn.\n\
{W}: Return this creature to its owner's hand.\n\
{1}: This creature gets +1/-1 or -1/+1 until end of turn.";

const PEMMINS_AURA: &str = "Enchant creature\n\
{U}: Untap enchanted creature.\n\
{U}: Enchanted creature gains flying until end of turn.\n\
{U}: Enchanted creature gains shroud until end of turn.\n\
{1}: Enchanted creature gets +1/-1 or -1/+1 until end of turn.";

fn pool(colored: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    colored
        .iter()
        .flat_map(|(kind, n)| vec![ManaUnit::new(*kind, ObjectId(0), false, vec![]); *n])
        .collect()
}

/// Pass priority / finalize mana until the branch prompt (or a terminal state)
/// is reached.
fn advance_to_choice(runner: &mut GameRunner) {
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseOneOfBranch { .. } => return,
            WaitingFor::ManaPayment { .. } | WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Index of the `ChooseOneOf` activated ability on a parsed permanent. Located
/// by EFFECT SHAPE, not by a hardcoded ordinal, so the assertion below cannot
/// pass by accidentally activating one of the sibling keyword-grant abilities.
fn pt_choice_ability_index(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|a| matches!(&*a.effect, Effect::ChooseOneOf { .. }))
        .expect("the P/T disjunction must lower to a ChooseOneOf activated ability")
}

/// Activate the P/T-disjunction ability on `id` and return the branch prompt's
/// descriptions, having asserted the controller is the chooser.
fn activate_and_read_branches(runner: &mut GameRunner, id: ObjectId) -> Vec<String> {
    let index = pt_choice_ability_index(runner, id);
    runner
        .act(GameAction::ActivateAbility {
            source_id: id,
            ability_index: index,
        })
        .expect("activating the P/T disjunction must succeed");
    advance_to_choice(runner);
    match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            player,
            branch_descriptions,
            ..
        } => {
            assert_eq!(
                *player, P0,
                "CR 608.2d: the ability's controller announces the choice"
            );
            assert_eq!(
                branch_descriptions.len(),
                2,
                "a two-alternative disjunction must offer exactly two branches, got \
                 {branch_descriptions:?}"
            );
            branch_descriptions.clone()
        }
        other => panic!(
            "CR 608.2d: the controller must be offered the +1/-1 or -1/+1 choice as the \
             ability resolves; got {other:?}"
        ),
    }
}

/// Brightling (SelfRef subject). Each branch is resolved in its own game so the
/// two Layer 7c modifications cannot stack and mask each other.
///
/// `branch_phrase` picks the branch by its printed modification, so the test
/// fails loudly if the branch order changes rather than silently checking the
/// wrong one.
fn brightling_branch_outcome(branch_phrase: &str, seed: u64) -> (Option<i32>, Option<i32>) {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(&[(ManaType::White, 1)]));
    let brightling = scenario
        .add_creature_from_oracle(P0, "Brightling", 3, 3, BRIGHTLING)
        .id();
    let mut runner = scenario.build();

    assert_eq!(
        (
            runner.state().objects[&brightling].power,
            runner.state().objects[&brightling].toughness
        ),
        (Some(3), Some(3)),
        "precondition: Brightling is a 3/3 before the ability resolves"
    );

    let descriptions = activate_and_read_branches(&mut runner, brightling);
    let index = descriptions
        .iter()
        .position(|d| d.contains(branch_phrase))
        .unwrap_or_else(|| panic!("no {branch_phrase} branch among {descriptions:?}"));
    runner
        .act(GameAction::ChooseBranch { index })
        .expect("resolving the chosen branch must succeed");

    let obj = &runner.state().objects[&brightling];
    (obj.power, obj.toughness)
}

/// The +1/-1 alternative applies — and ONLY it.
#[test]
fn brightling_plus_one_minus_one_branch_makes_it_four_two() {
    assert_eq!(
        brightling_branch_outcome("+1/-1", 42),
        (Some(4), Some(2)),
        "CR 613.4c: choosing +1/-1 must make the 3/3 a 4/2"
    );
}

/// The -1/+1 alternative applies — the branch the pre-fix parser threw away.
/// This is the half the collapsed `Effect::Pump { +1, -1 }` got wrong by
/// construction.
#[test]
fn brightling_minus_one_plus_one_branch_makes_it_two_four() {
    assert_eq!(
        brightling_branch_outcome("-1/+1", 7),
        (Some(2), Some(4)),
        "CR 613.4c: choosing -1/+1 must make the 3/3 a 2/4 — the alternative the \
         discarded parser remainder used to lose"
    );
}

/// Pemmin's Aura (`EnchantedBy` subject, not SelfRef). The Aura's ability must
/// pump the ENCHANTED creature, proving the arm carries the subject through
/// rather than defaulting to the source.
#[test]
fn pemmins_aura_pumps_the_enchanted_creature_on_the_chosen_branch() {
    let mut scenario = GameScenario::new_n_player(2, 99);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(&[(ManaType::Blue, 1)]));
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Pemmin's Aura", PEMMINS_AURA)
        .with_subtypes(vec!["Aura"])
        .id();
    let mut runner = scenario.build();
    // CR 303.4b + CR 704.5p: the Aura subtype is load-bearing here, MEASURED —
    // without it the permanent is "neither an Aura, an Equipment, nor a
    // Fortification", so the state-based action unattaches it (and it stays on
    // the battlefield) before the ability can resolve, and the "enchanted
    // creature" of CR 303.4b is then nobody. Both halves of the link are wired
    // for the same reason the real attach path wires both.
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .expect("the Aura exists")
        .attached_to = Some(AttachTarget::Object(bears));
    runner
        .state_mut()
        .objects
        .get_mut(&bears)
        .expect("the host exists")
        .attachments
        .push(aura);

    let descriptions = activate_and_read_branches(&mut runner, aura);
    let index = descriptions
        .iter()
        .position(|d| d.contains("-1/+1"))
        .unwrap_or_else(|| panic!("no -1/+1 branch among {descriptions:?}"));
    runner
        .act(GameAction::ChooseBranch { index })
        .expect("resolving the chosen branch must succeed");

    let obj = &runner.state().objects[&bears];
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(1), Some(3)),
        "CR 613.4c: the chosen -1/+1 must land on the ENCHANTED creature, making the 2/2 a 1/3"
    );
}

/// Verbatim Scryfall Oracle text. The EIGHTH card in the class and the only one
/// whose alternatives are variable.
const LILIANA_OF_THE_DARK_REALMS: &str =
    "[+1]: Search your library for a Swamp card, reveal it, put it into your hand, then shuffle.\n\
[\u{2212}3]: Target creature gets +X/+X or -X/-X until end of turn, where X is the number of \
Swamps you control.\n\
[\u{2212}6]: You get an emblem with \"Swamps you control have '{T}: Add {B}{B}{B}{B}.'\"";

/// Liliana of the Dark Realms' [-3], driven through the real
/// activate -> announce target -> resolve -> choose branch pipeline with THREE
/// Swamps on the battlefield, so X is 3 and each alternative is a concrete
/// ±3/±3 on the 4/4 target.
///
/// This is the runtime half of the where-X binding. The parse-level test
/// (`liliana_pt_disjunction_keeps_both_alternatives_and_binds_x_to_swamps`)
/// proves the branch holds the Swamp count instead of a bare `Variable("X")`;
/// this proves that count is what the board actually applies. With the binding
/// missing, X resolves to 0 and BOTH branches are a silent +0/+0 no-op — a 4/4
/// that stays a 4/4 while the ability reads as fully supported.
fn liliana_branch_outcome(branch_phrase: &str, seed: u64) -> (Option<i32>, Option<i32>) {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);
    let liliana = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Liliana of the Dark Realms",
            "Liliana",
            3,
            LILIANA_OF_THE_DARK_REALMS,
        )
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let target = scenario.add_creature(P0, "Recipient", 4, 4).id();
    let mut runner = scenario.build();

    let index = runner.state().objects[&liliana]
        .abilities
        .iter()
        .position(|a| matches!(&*a.effect, Effect::TargetOnly { .. }))
        .expect("the [-3] must lower to a TargetOnly head carrying the choice");
    runner
        .act(GameAction::ActivateAbility {
            source_id: liliana,
            ability_index: index,
        })
        .expect("the [-3] is activatable at 3 loyalty");
    // CR 601.2c via CR 602.2b: the TARGET is announced as the ability goes on
    // the stack, before the modification choice exists.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            })
            .expect("the 4/4 is a legal target");
    }
    advance_to_choice(&mut runner);

    let WaitingFor::ChooseOneOfBranch {
        player,
        branch_descriptions,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "CR 608.2d: the controller must be offered the +X/+X or -X/-X choice on \
             resolution; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let descriptions = branch_descriptions.clone();
    let index = descriptions
        .iter()
        .position(|d| d.contains(branch_phrase))
        .unwrap_or_else(|| panic!("no {branch_phrase} branch among {descriptions:?}"));
    runner
        .act(GameAction::ChooseBranch { index })
        .expect("resolving the chosen branch must succeed");

    let obj = &runner.state().objects[&target];
    (obj.power, obj.toughness)
}

/// X = 3 Swamps, "+X/+X" chosen: the 4/4 becomes a 7/7.
#[test]
fn liliana_plus_x_branch_reads_the_swamp_count() {
    assert_eq!(
        liliana_branch_outcome("+x/+x", 1234),
        (Some(7), Some(7)),
        "CR 107.3i + CR 613.4c: with three Swamps, +X/+X is +3/+3 on the 4/4 — an \
         unbound X would leave it a 4/4"
    );
}

/// X = 3 Swamps, "-X/-X" chosen: the 4/4 becomes a 1/1. This is the alternative
/// the pre-fix parser threw away entirely.
#[test]
fn liliana_minus_x_branch_reads_the_swamp_count() {
    assert_eq!(
        liliana_branch_outcome("-x/-x", 4321),
        (Some(1), Some(1)),
        "CR 107.3i + CR 613.4c: with three Swamps, -X/-X is -3/-3 on the 4/4 — the \
         alternative that used to be dropped, at the magnitude that used to be lost"
    );
}
