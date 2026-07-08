//! Greymond, Avacyn's Stalwart ({2}{W}{W}, Creature — Human Soldier, 3/4):
//!   1. "As Greymond, Avacyn's Stalwart enters, choose two abilities from among
//!      first strike, vigilance, and lifelink."
//!   2. "Humans you control have each of the chosen abilities."
//!   3. "As long as you control four or more Humans, Humans you control get
//!      +2/+2."
//!
//! These tests drive the real parse → synthesis → layer pipeline. The
//! `ChoiceType::Keyword { count: 2 }` choice is answered through the actual cast
//! pipeline (V2), and the two Humans-you-control statics are exercised by
//! pushing the chosen keywords onto the source (the same indirection the cast
//! pipeline produces) and reading the post-`evaluate_layers` view.
//!
//! CR 608.2d (choosing two abilities), CR 613.1f (Layer 6 keyword grant),
//! CR 611.3a (continuous effect not locked in — the +2/+2 anthem tracks the live
//! Human count), CR 400.7 (chosen attributes clear on zone change).

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{ChoiceType, ChosenAttribute};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const GREYMOND: &str = "As Greymond, Avacyn's Stalwart enters, choose two abilities from among \
    first strike, vigilance, and lifelink.\n\
    Humans you control have each of the chosen abilities.\n\
    As long as you control four or more Humans, Humans you control get +2/+2.";

/// Recompute layers and read an object's effective (post-layer) power/toughness.
fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

/// True iff `id` currently has `keyword` after a fresh layer evaluation.
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

/// Push the two chosen keywords onto Greymond — the same persisted state the
/// as-enters `Choose { count: 2 }` produces via `bind_named_choice` (two
/// independent `ChosenAttribute::Keyword` entries).
fn set_chosen(runner: &mut GameRunner, greymond: ObjectId, kws: &[Keyword]) {
    let obj = runner.state_mut().objects.get_mut(&greymond).unwrap();
    obj.chosen_attributes
        .retain(|a| !matches!(a, ChosenAttribute::Keyword(_)));
    for kw in kws {
        obj.chosen_attributes
            .push(ChosenAttribute::Keyword(kw.clone()));
    }
}

/// Build Greymond (Human Soldier) on the battlefield under P0 from Oracle text,
/// running the real parse + synthesis pipeline that installs both statics.
fn add_greymond(scenario: &mut GameScenario) -> ObjectId {
    let mut b =
        scenario.add_creature_from_oracle(P0, "Greymond, Avacyn's Stalwart", 3, 4, GREYMOND);
    b.with_subtypes(vec!["Human", "Soldier"]);
    b.id()
}

fn add_human(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    let mut b = scenario.add_creature(player, name, 2, 2);
    b.with_subtypes(vec!["Human"]);
    b.id()
}

// ===== V2: cast Greymond and answer the count-2 choice =====

/// V2 — Cast Greymond through the real pipeline; the as-enters replacement pauses
/// on the keyword choice, which offers the three count-2 pairs. Answering a pair
/// must persist TWO `ChosenAttribute::Keyword` on Greymond (CR 608.2d).
#[test]
fn cast_greymond_answers_two_keyword_choice() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let greymond = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Greymond, Avacyn's Stalwart",
            3,
            4,
            GREYMOND,
        );
        b.with_subtypes(vec!["Human", "Soldier"]);
        b.id()
    };

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&greymond).unwrap().card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: greymond,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Greymond");
    runner.advance_until_stack_empty();

    // The as-enters choice pauses on a NamedChoice whose options are the three
    // count-2 pairs (CR 608.2d).
    let WaitingFor::NamedChoice { options, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Greymond must pause on the keyword choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        options.len(),
        3,
        "C(3,2) = 3 pairs offered, got {options:?}"
    );
    let pair = options
        .iter()
        .find(|o| o.contains("First Strike") && o.contains("Vigilance"))
        .expect("a First Strike + Vigilance pair option")
        .clone();

    runner
        .act(GameAction::ChooseOption { choice: pair })
        .expect("answer the keyword pair");
    runner.advance_until_stack_empty();

    // Two independent ChosenAttribute::Keyword persist on Greymond.
    let chosen = runner.state().objects[&greymond].chosen_keywords();
    assert!(
        chosen.contains(&&Keyword::FirstStrike) && chosen.contains(&&Keyword::Vigilance),
        "both chosen abilities must persist, got {chosen:?}"
    );
    assert_eq!(
        chosen.len(),
        2,
        "exactly two keywords chosen, got {chosen:?}"
    );
}

// ===== V4: each Human gains BOTH chosen keywords; non-Human none =====

/// V4 — With Greymond's chosen pair = (First Strike, Lifelink), every Human you
/// control gains BOTH keywords (CR 613.1f); a non-Human gains neither; an
/// unchosen keyword (Vigilance) is never granted; the chosen Lifelink is
/// installed on each recipient (CR 702.15b).
#[test]
fn each_human_gains_both_chosen_keywords_non_human_none() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let greymond = add_greymond(&mut scenario);
    let other_human = add_human(&mut scenario, P0, "Soldier Ally");
    let non_human = scenario.add_creature(P0, "Llanowar Elves", 1, 1).id();
    let opp_human = add_human(&mut scenario, P1, "Enemy Conscript");

    let mut runner = scenario.build();
    set_chosen(
        &mut runner,
        greymond,
        &[Keyword::FirstStrike, Keyword::Lifelink],
    );

    // Greymond itself (a Human you control) and the other Human gain BOTH.
    for id in [greymond, other_human] {
        assert!(
            has_kw(&mut runner, id, &Keyword::FirstStrike),
            "Human {id:?} must gain the chosen First Strike"
        );
        assert!(
            has_kw(&mut runner, id, &Keyword::Lifelink),
            "Human {id:?} must gain the chosen Lifelink"
        );
        assert!(
            !has_kw(&mut runner, id, &Keyword::Vigilance),
            "the unchosen Vigilance must NOT be granted to {id:?}"
        );
    }

    // The non-Human gains nothing.
    assert!(
        !has_kw(&mut runner, non_human, &Keyword::FirstStrike)
            && !has_kw(&mut runner, non_human, &Keyword::Lifelink),
        "a non-Human you control gains none of the chosen abilities"
    );

    // The opponent's Human is not "a Human you control".
    assert!(
        !has_kw(&mut runner, opp_human, &Keyword::FirstStrike)
            && !has_kw(&mut runner, opp_human, &Keyword::Lifelink),
        "the opponent's Human is excluded (controller = You)"
    );

    // The chosen Lifelink is actually installed (present in the recipient's
    // post-layer keyword set), which is what drives lifegain on damage
    // (CR 702.15b — damage from a lifelink source gains its controller life;
    // lifelink is a damage-application static, not a trigger).
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert!(
        runner.state().objects[&other_human]
            .keywords
            .contains(&Keyword::Lifelink),
        "the chosen Lifelink must be installed on the recipient's effective keyword set"
    );
}

// ===== V5: two Greymonds with different pairs — union, source-scoped =====

/// V5 — Two Greymonds choosing different pairs each grant their own keywords;
/// a Human you control receives the UNION, and each grant stays scoped to its
/// own source (CR 613.1f, source-scoped read in `apply_continuous_effect`).
#[test]
fn two_greymonds_union_of_keywords_source_scoped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let greymond_a = add_greymond(&mut scenario);
    let greymond_b = add_greymond(&mut scenario);
    let human = add_human(&mut scenario, P0, "Shared Recipient");

    let mut runner = scenario.build();
    set_chosen(
        &mut runner,
        greymond_a,
        &[Keyword::FirstStrike, Keyword::Vigilance],
    );
    set_chosen(
        &mut runner,
        greymond_b,
        &[Keyword::Vigilance, Keyword::Lifelink],
    );

    // The Human gains the UNION: First Strike (A) + Vigilance (both) + Lifelink (B).
    assert!(
        has_kw(&mut runner, human, &Keyword::FirstStrike),
        "First Strike from Greymond A"
    );
    assert!(
        has_kw(&mut runner, human, &Keyword::Vigilance),
        "Vigilance from both Greymonds"
    );
    assert!(
        has_kw(&mut runner, human, &Keyword::Lifelink),
        "Lifelink from Greymond B"
    );

    // Source-scoped: clearing B's choice drops ONLY B's exclusive keyword.
    set_chosen(&mut runner, greymond_b, &[]);
    assert!(
        has_kw(&mut runner, human, &Keyword::FirstStrike)
            && has_kw(&mut runner, human, &Keyword::Vigilance),
        "Greymond A's grants survive clearing Greymond B (source-scoped)"
    );
    assert!(
        !has_kw(&mut runner, human, &Keyword::Lifelink),
        "Lifelink (B-exclusive) is gone once B's choice is cleared"
    );
}

// ===== V7: live count gate for the +2/+2 anthem =====

/// V7 — The +2/+2 anthem is active only while you control four or more Humans
/// (CR 611.3a: the continuous effect tracks the live count). 3 Humans → off,
/// 4 → on, back to 3 → off.
#[test]
fn anthem_tracks_live_human_count_threshold() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Greymond + 2 other Humans = 3 Humans you control (below threshold).
    let greymond = add_greymond(&mut scenario);
    let h2 = add_human(&mut scenario, P0, "Human Two");
    let _h3 = add_human(&mut scenario, P0, "Human Three");

    let mut runner = scenario.build();
    set_chosen(
        &mut runner,
        greymond,
        &[Keyword::FirstStrike, Keyword::Vigilance],
    );

    // 3 Humans → no +2/+2 (Greymond stays base 3/4; h2 stays 2/2).
    assert_eq!(
        effective_pt(&mut runner, greymond),
        (3, 4),
        "3 Humans: anthem OFF, Greymond base 3/4"
    );
    assert_eq!(
        effective_pt(&mut runner, h2),
        (2, 2),
        "3 Humans: anthem OFF, other Human base 2/2"
    );

    // Add a 4th Human → anthem ON.
    let h4 = add_human_after_build(&mut runner, P0, "Human Four");
    assert_eq!(
        effective_pt(&mut runner, greymond),
        (5, 6),
        "4 Humans: anthem ON, Greymond 3/4 + 2/2"
    );
    assert_eq!(
        effective_pt(&mut runner, h2),
        (4, 4),
        "4 Humans: anthem ON, other Human 2/2 + 2/2"
    );

    // Remove the 4th Human → back to 3 → anthem OFF (CR 611.3a live tracking).
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != h4);
        state.objects.remove(&h4);
    }
    assert_eq!(
        effective_pt(&mut runner, greymond),
        (3, 4),
        "back to 3 Humans: anthem OFF again"
    );
}

/// Add a Human to the battlefield after the runner is built (for the threshold
/// flip in V7). Mirrors `add_human` but operates on live state.
fn add_human_after_build(runner: &mut GameRunner, player: PlayerId, name: &str) -> ObjectId {
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    let id = engine::game::zones::create_object(
        state,
        card_id,
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.card_types.subtypes = vec!["Human".to_string()];
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    id
}

// V8 (off-zone plural read) lives as a unit test inside the engine crate
// (`off_zone_characteristics.rs`) because that module is `pub(crate)`.

// V10 (matthewevans regression for PR #4638 — repeated single-keyword choice
// REPLACES the prior answer) lives as an inline unit test inside the engine crate
// (`game/effects/choose.rs`), because it parses Angelic Skirmisher's real
// choose+grant chain via the `pub(crate)` `parser::oracle_trigger::parse_trigger_line`
// to drive the production `ChooseOption` / `bind_named_choice` path twice.

// ===== V9: serde round-trip =====

/// V9 — Serde compatibility: legacy `Keyword { options }` JSON (no `count`)
/// deserializes to `count == 1`; a new `count: 2` value round-trips; and the
/// `count == 1` form serializes WITHOUT the `count` field (byte-stable with old
/// card-data).
#[test]
fn choice_type_keyword_count_serde_roundtrip() {
    // Legacy JSON (no count) → count == 1.
    let legacy: ChoiceType =
        serde_json::from_str(r#"{"Keyword":{"options":["FirstStrike","Vigilance"]}}"#)
            .expect("legacy Keyword JSON parses");
    assert_eq!(
        legacy,
        ChoiceType::Keyword {
            options: vec![Keyword::FirstStrike, Keyword::Vigilance],
            count: 1,
        },
        "legacy Keyword JSON must default count to 1"
    );

    // count == 1 serializes WITHOUT the count field (byte-stable).
    let single = ChoiceType::Keyword {
        options: vec![Keyword::FirstStrike],
        count: 1,
    };
    let single_json = serde_json::to_string(&single).unwrap();
    assert!(
        !single_json.contains("count"),
        "count == 1 must be omitted for byte-stability, got {single_json}"
    );

    // count == 2 round-trips and carries the field.
    let pair = ChoiceType::Keyword {
        options: vec![Keyword::FirstStrike, Keyword::Vigilance, Keyword::Lifelink],
        count: 2,
    };
    let pair_json = serde_json::to_string(&pair).unwrap();
    assert!(
        pair_json.contains("count"),
        "count == 2 must serialize the field, got {pair_json}"
    );
    let back: ChoiceType = serde_json::from_str(&pair_json).unwrap();
    assert_eq!(back, pair, "count == 2 must round-trip");
}

// ===== F4: AI candidate generation =====

/// F4 — AI legal-action generation for Greymond's count-2 keyword choice yields
/// exactly the three pair candidates (each a `GameAction::ChooseOption` whose
/// choice is a comma-joined keyword pair).
#[test]
fn ai_candidates_three_pairs_for_greymond_choice() {
    use engine::ai_support::legal_actions;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let greymond = add_greymond(&mut scenario);
    let mut runner = scenario.build();

    // Drive state into the keyword NamedChoice for Greymond's choice.
    runner.state_mut().waiting_for = WaitingFor::NamedChoice {
        player: P0,
        choice_type: ChoiceType::Keyword {
            options: vec![Keyword::FirstStrike, Keyword::Vigilance, Keyword::Lifelink],
            count: 2,
        },
        options: vec![
            "First Strike, Vigilance".to_string(),
            "First Strike, Lifelink".to_string(),
            "Vigilance, Lifelink".to_string(),
        ],
        source_id: Some(greymond),
        persist_player: None,
    };

    let actions = legal_actions(runner.state());
    let choose: Vec<String> = actions
        .into_iter()
        .filter_map(|a| match a {
            GameAction::ChooseOption { choice } => Some(choice),
            _ => None,
        })
        .collect();
    assert_eq!(
        choose.len(),
        3,
        "exactly three pair candidates for a count-2 keyword choice, got {choose:?}"
    );
    for expected in [
        "First Strike, Vigilance",
        "First Strike, Lifelink",
        "Vigilance, Lifelink",
    ] {
        assert!(
            choose.iter().any(|c| c == expected),
            "missing pair candidate {expected:?} in {choose:?}"
        );
    }
}
