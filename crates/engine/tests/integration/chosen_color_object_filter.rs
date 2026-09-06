//! Runtime cast-pipeline coverage for the PRINTED chosen-colour object-filter
//! qualifier — `<type-phrase> of the color of your choice` (CR 105.4).
//!
//! Before the fix, `parse_type_phrase_with_ctx` had a trailing chosen-TYPE
//! qualifier arm but no chosen-COLOUR sibling, so the colour restriction was
//! silently dropped: Wash Out returned EVERY permanent and Root Greevil
//! destroyed EVERY enchantment. The fix adds the arm (gated on
//! `ChosenColorQualifierScope::ChainBound`) plus an assembly-time
//! `Effect::Choose { ChoiceType::Color, persist: true }` injection driven by
//! declared per-clause provenance (`ClauseIr.printed_color_choice`).
//!
//! Built via the `/card-test` recipe: `GameScenario` +
//! `GameRunner::cast(..)/activate(..).resolve()` + `CastOutcome`/`Outcome`
//! zone deltas, on VERBATIM Oracle text. Every negative assertion is paired
//! with a positive reach-guard in the same test, so an upstream parse failure
//! cannot satisfy it vacuously.
//!
//! REVERT DISCRIMINATORS:
//! * `wash_out_returns_only_the_chosen_colors_permanents` (V1) — the
//!   `assert_zone(&[red_bear, colorless_relic, decoy], Zone::Battlefield)`
//!   assertion. Revert the parser arm and all four bounce.
//! * `root_greevil_destroys_only_the_chosen_colors_enchantments` (V3) — the
//!   `assert_zone(&[red_aura], Zone::Battlefield)` assertion. Revert and both
//!   enchantments are destroyed.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{ChoiceType, ChosenAttribute, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Wash Out {3}{U} Sorcery — verbatim Oracle text (MTGJSON `AtomicCards.json`).
const WASH_OUT: &str = "Return all permanents of the color of your choice to their owners' hands.";

/// Root Greevil {3}{G} Creature — Beast 2/3 — verbatim Oracle text.
const ROOT_GREEVIL: &str =
    "{2}{G}, {T}, Sacrifice this creature: Destroy all enchantments of the color of your choice.";

/// Evacuation {3}{U}{U} Instant — verbatim Oracle text. The no-qualifier
/// sibling: the arm must not fire where no qualifier is printed.
const EVACUATION: &str = "Return all creatures to their owners' hands.";

/// `n` units of one mana type, with no producing source and no spend
/// restrictions — the plainest pool contents that can pay a printed cost, so a
/// cast in this suite can never fail for a mana reason the test did not intend.
fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); n]
}

/// A mana pool assembled from `(type, count)` pairs. Each test seeds exactly the
/// cost it intends to pay rather than an unbounded pool, so a cast that should
/// have been unaffordable cannot pass unnoticed.
fn pool(colored: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    colored
        .iter()
        .flat_map(|(kind, n)| mana(*kind, *n))
        .collect()
}

/// A one-colour mana cost, so `with_mana_cost` derives exactly that colour
/// (CR 202.2 / CR 105.2).
fn one_color(shard: ManaCostShard) -> ManaCost {
    ManaCost::Cost {
        shards: vec![shard],
        generic: 1,
    }
}

/// Count `Effect::Choose { ChoiceType::Color, .. }` nodes anywhere in a
/// definition tree (effect head, sub-ability chain, else branch, modes).
fn count_color_choices(text: &str) -> usize {
    let parsed = parse_oracle_text(text, "Probe", &[], &[], &[]);
    let mut n = 0;
    for ability in &parsed.abilities {
        n += walk_color_choices(ability);
    }
    n
}

/// Recursive worker for [`count_color_choices`]: counts colour choosers in one
/// definition tree, following every branch a chooser can be displaced into
/// (`sub_ability` — where the injected wrap puts the displaced effect —
/// `else_ability`, and each modal branch). Walking only the head would report 0
/// for exactly the shape this suite exists to assert.
fn walk_color_choices(def: &engine::types::ability::AbilityDefinition) -> usize {
    let mut n = usize::from(matches!(
        &*def.effect,
        Effect::Choose {
            choice_type: ChoiceType::Color { .. },
            ..
        }
    ));
    if let Some(sub) = def.sub_ability.as_ref() {
        n += walk_color_choices(sub);
    }
    if let Some(alt) = def.else_ability.as_ref() {
        n += walk_color_choices(alt);
    }
    for mode in &def.mode_abilities {
        n += walk_color_choices(mode);
    }
    n
}

// ---------------------------------------------------------------------------
// V1 — Wash Out: the primary runtime discriminator.
// ---------------------------------------------------------------------------

/// V1 (RUNTIME) — CR 105.4 + CR 105.2 + CR 109.2. Wash Out returns only the
/// permanents of the chosen colour.
///
/// Multi-authority hostile fixture (a) is folded in: `decoy` already carries
/// `ChosenAttribute::Color(Red)` before the cast. If the runtime read "any
/// chosen colour on the board" instead of binding to `ability.source_id`, the
/// red permanents would bounce too.
#[test]
fn wash_out_returns_only_the_chosen_colors_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blue_bear = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    let red_bear = scenario
        .add_creature(P0, "Red Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Red))
        .id();
    let blue_relic = scenario
        .add_artifact_from_oracle(P0, "Blue Relic", "")
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    // CR 105.4: "colorless" is not a colour, so a colourless permanent can
    // never be the chosen colour.
    let colorless_relic = scenario
        .add_artifact_from_oracle(P0, "Colorless Relic", "")
        .id();
    // Wash Out is not controller-scoped: an opponent's blue permanent is
    // returned as well (to its own owner's hand).
    let opposing_drake = scenario
        .add_creature(P1, "Opposing Drake", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    // Multi-authority decoy: a GREEN permanent that already chose Red.
    let decoy = scenario
        .add_creature(P0, "Decoy Bear", 1, 1)
        .with_mana_cost(one_color(ManaCostShard::Green))
        .id();

    let wash_out = scenario
        .add_spell_to_hand_from_oracle(P0, "Wash Out", false, WASH_OUT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    scenario.with_mana_pool(P0, pool(&[(ManaType::Blue, 1), (ManaType::Colorless, 3)]));

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&decoy)
        .expect("decoy exists")
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Red));

    let outcome = runner.cast(wash_out).choose_option("Blue").resolve();

    // POSITIVE REACH-GUARD: the effect genuinely ran and moved objects.
    outcome.assert_zone(&[blue_bear, blue_relic, opposing_drake], Zone::Hand);
    // THE REVERT-FAILING ASSERTION: today (pre-fix) all six bounce.
    outcome.assert_zone(&[red_bear, colorless_relic, decoy], Zone::Battlefield);
}

/// V1 sibling (a) — CR 105.4. Choosing a colour no permanent has moves nothing
/// and raises no further prompt. The positive reach-guard is the paired
/// blue-choice run above; here the guard is that the spell itself resolved
/// (CR 608.2n puts it into the graveyard) rather than being stuck.
#[test]
fn wash_out_with_no_permanent_of_the_chosen_color_moves_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blue_bear = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    let red_bear = scenario
        .add_creature(P0, "Red Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Red))
        .id();

    let wash_out = scenario
        .add_spell_to_hand_from_oracle(P0, "Wash Out", false, WASH_OUT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    scenario.with_mana_pool(P0, pool(&[(ManaType::Blue, 1), (ManaType::Colorless, 3)]));

    let mut runner = scenario.build();

    // POSITIVE REACH-GUARD on the RUNTIME CHOICE PATH, in the same
    // `runner.state().waiting_for` idiom as
    // `wash_out_raises_one_five_option_color_prompt_before_the_bounce`.
    //
    // Load-bearing: `drive_resolution` (`game/scenario.rs`) answers a
    // `NamedChoice` window only when a choice was declared and otherwise BREAKS,
    // so a declared `choose_option` that is never consumed is SILENT. Without
    // this guard the zone assertions below would hold just as well on a build
    // that raised no chooser at all and simply bounced nothing — the exact
    // failure this suite exists to catch. Resolving with NO colour declared
    // halts AT the injected prompt and proves it was reached.
    let halted = runner.cast(wash_out).resolve();
    assert!(
        matches!(
            halted.final_waiting_for(),
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::Color { .. },
                ..
            }
        ),
        "the resolution must reach the injected colour prompt, got {:?}",
        halted.final_waiting_for()
    );
    // CR 608.2c: the choice resolves before the bounce — nothing has moved yet.
    assert_eq!(
        runner.state().objects[&blue_bear].zone,
        Zone::Battlefield,
        "the bounce must not precede the colour choice (CR 608.2c)"
    );

    // Answer with a colour NO permanent has, then let the spell finish.
    runner
        .act(GameAction::ChooseOption {
            choice: "White".to_string(),
        })
        .expect("answer the colour prompt");
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority toward resolution");
    }

    // REACH-GUARD: the spell resolved (CR 608.2n) — it did not hang on a prompt.
    assert_eq!(runner.state().objects[&wash_out].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&blue_bear].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&red_bear].zone, Zone::Battlefield);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no further prompt may be raised, got {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// V2 — the injected prompt itself.
// ---------------------------------------------------------------------------

/// V2 (RUNTIME) — CR 105.4 + CR 608.2c. Exactly one colour prompt is raised,
/// with the five colours, and it resolves BEFORE the bounce.
///
/// Driven through `apply()` via `runner.act`, so the prompt is observed in the
/// real pipeline. Reverting the injection removes the prompt entirely and the
/// `NamedChoice` window is never reached.
#[test]
fn wash_out_raises_one_five_option_color_prompt_before_the_bounce() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blue_bear = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    let red_bear = scenario
        .add_creature(P0, "Red Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Red))
        .id();

    let wash_out = scenario
        .add_spell_to_hand_from_oracle(P0, "Wash Out", false, WASH_OUT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    scenario.with_mana_pool(P0, pool(&[(ManaType::Blue, 1), (ManaType::Colorless, 3)]));

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&wash_out].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: wash_out,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Wash Out");

    let mut prompts = 0;
    let mut options = Vec::new();
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice {
                choice_type,
                options: opts,
                ..
            } => {
                assert!(
                    matches!(choice_type, ChoiceType::Color { .. }),
                    "the injected prompt must be a colour choice, got {choice_type:?}"
                );
                prompts += 1;
                options = opts;
                // CR 105.4: the bounce has NOT happened yet — the choice
                // resolves first (CR 608.2c).
                assert_eq!(
                    runner.state().objects[&blue_bear].zone,
                    Zone::Battlefield,
                    "the bounce must not precede the colour choice (CR 608.2c)"
                );
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Blue".to_string(),
                    })
                    .expect("answer the colour prompt");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward resolution");
            }
            other => panic!("unexpected window: {other:?}"),
        }
    }

    assert_eq!(prompts, 1, "exactly one colour prompt (CR 105.4)");
    // CR 105.4: five colours, never "colorless" or "multicolored".
    assert_eq!(options.len(), 5, "five colour options, got {options:?}");

    // POSITIVE REACH-GUARD: the post-answer bounce delta.
    assert_eq!(runner.state().objects[&blue_bear].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&red_bear].zone, Zone::Battlefield);
}

/// V2 multi-authority (c) — CR 105.4. Wash Out cast twice in one turn raises
/// two independent prompts, and each choice governs only its own resolution.
///
/// SCOPE: the two casts here are two DISTINCT spell objects, so this test covers
/// the two-objects case. The SOURCE-REUSE case — the SAME storage id resolving a
/// colour choice twice, as a recursion effect (Regrowth) or flashback (Prismatic
/// Strands) produces — is covered by
/// `chosen_color_rechoose_same_source::wash_out_recast_on_the_same_object_uses_its_own_color`,
/// which moves the resolved sorcery back to hand and recasts the same object.
///
/// That seam was the former follow-up **F7** and is now closed — but NOT by
/// replace-on-rechoose, which this PR deleted. `apply_choice_attributes`
/// (`game/effects/choose.rs`) ACCUMULATES `ChosenAttribute::Color`, so a source
/// can hold several answers. F7 is closed by the ACCESSOR SPLIT instead: each
/// reader takes the end of the list its own rule entitles it to — CR 608.2d the
/// newest via `current_chosen_color()`, CR 607.2d the linked ability's via
/// `chosen_color()` (oldest-since-entry). Both `game/filter.rs` `IsChosenColor`
/// arms read the NEWEST, so a recast binds its own answer. This matches
/// `docs/parser-misparse-backlog.md`'s F7 entry; an earlier revision of this
/// comment asserted the opposite and was wrong.
#[test]
fn two_wash_outs_bind_their_own_choices_independently() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blue_bear = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    let red_bear = scenario
        .add_creature(P0, "Red Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Red))
        .id();
    let green_bear = scenario
        .add_creature(P0, "Green Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Green))
        .id();

    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Wash Out", false, WASH_OUT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Wash Out", false, WASH_OUT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let out1 = runner.cast(first).choose_option("Blue").resolve();
    out1.assert_zone(&[blue_bear], Zone::Hand);
    out1.assert_zone(&[red_bear, green_bear], Zone::Battlefield);

    let out2 = runner.cast(second).choose_option("Red").resolve();
    out2.assert_zone(&[red_bear], Zone::Hand);
    out2.assert_zone(&[green_bear], Zone::Battlefield);
}

// ---------------------------------------------------------------------------
// V3 — Root Greevil: the CR 400.7j sacrificed-source case.
// ---------------------------------------------------------------------------

/// V3 (RUNTIME) — CR 105.4 + CR 400.7j. Root Greevil destroys only the chosen
/// colour's enchantments, even though its own source was sacrificed as part of
/// the activation COST and is in the graveyard when the ability resolves.
///
/// Multi-authority hostile fixture (b): a second Greevil-like source already
/// carrying `ChosenAttribute::Color(Red)` must not govern this activation.
#[test]
fn root_greevil_destroys_only_the_chosen_colors_enchantments() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blue_aura = scenario
        .add_enchantment_from_oracle(P0, "Blue Enchantment", "")
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    let red_aura = scenario
        .add_enchantment_from_oracle(P0, "Red Enchantment", "")
        .with_mana_cost(one_color(ManaCostShard::Red))
        .id();
    // The type filter still applies alongside the colour prop: a BLUE creature
    // is not an enchantment and must survive.
    let blue_bear = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();

    let greevil = scenario
        .add_creature_from_oracle(P0, "Root Greevil", 2, 3, ROOT_GREEVIL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 3,
        })
        .id();
    // Multi-authority decoy: another source that chose Red on an earlier turn.
    let stale_chooser = scenario
        .add_creature(P0, "Stale Chooser", 1, 1)
        .with_mana_cost(one_color(ManaCostShard::Green))
        .id();
    scenario.with_mana_pool(P0, pool(&[(ManaType::Green, 1), (ManaType::Colorless, 2)]));

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&stale_chooser)
        .expect("decoy exists")
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Red));

    let outcome = runner.activate(greevil, 0).choose_option("Blue").resolve();

    // POSITIVE REACH-GUARD: the destroy genuinely ran.
    outcome.assert_zone(&[blue_aura], Zone::Graveyard);
    // THE REVERT-FAILING ASSERTION: today (pre-fix) both enchantments die.
    outcome.assert_zone(&[red_aura], Zone::Battlefield);
    outcome.assert_zone(&[blue_bear], Zone::Battlefield);
    // CR 400.7j: the source moved to a public zone as a COST, and the ability's
    // own effect still found it to read the chosen colour.
    outcome.assert_zone(&[greevil], Zone::Graveyard);
}

// ---------------------------------------------------------------------------
// V-NOQUAL — the arm is not over-broad.
// ---------------------------------------------------------------------------

/// V-NOQUAL (RUNTIME) — a mass bounce with NO printed qualifier is unchanged:
/// no colour choice is injected, no `IsChosenColor` is stamped, and both
/// creatures bounce.
#[test]
fn evacuation_without_a_qualifier_is_unchanged() {
    assert_eq!(
        count_color_choices(EVACUATION),
        0,
        "Evacuation prints no colour choice, so none may be injected"
    );
    // POSITIVE REACH-GUARD for the count helper itself: Wash Out DOES inject one.
    assert_eq!(
        count_color_choices(WASH_OUT),
        1,
        "reach-guard: the helper must be able to see an injected colour choice"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let blue_bear = scenario
        .add_creature(P0, "Blue Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Blue))
        .id();
    let red_bear = scenario
        .add_creature(P0, "Red Bear", 2, 2)
        .with_mana_cost(one_color(ManaCostShard::Red))
        .id();

    let evacuation = scenario
        .add_spell_to_hand_from_oracle(P0, "Evacuation", true, EVACUATION)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(evacuation).resolve();

    // POSITIVE DELTA: both creatures moved — the effect really ran.
    outcome.assert_zone(&[blue_bear, red_bear], Zone::Hand);
    outcome.assert_zone(&[evacuation], Zone::Graveyard);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no colour prompt may be raised, got {:?}",
        outcome.final_waiting_for()
    );
}
