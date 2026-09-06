//! Esix, Fractal Bloom — the first-time-each-turn token substitution whose copy
//! source is CHOSEN during resolution.
//!
//! Class, not card: `parse_first_time_token_copy_of_host_replacement` covers a
//! two-axis family. Axis A is the window's turn scope — "each turn" (unscoped,
//! Moonlit Meditation) vs "during each of your turns" (the controller's own
//! turns, CR 102.1). Axis B is where the copy source comes from — a STATIC
//! filter fixed by the card (Moonlit's enchanted host) vs a permanent CHOSEN at
//! resolution (Esix). The chosen arm lowers to `Effect::ChoosePermanent` with a
//! `CopyTokenOf` sub_ability, so answering it is a real mid-resolution
//! round-trip.
//!
//! CR basis: CR 614.1a (a replacement, via "instead"), CR 614.5 (one
//! opportunity per event — the substitute copies must not re-enter),
//! CR 614.12a (the choice is made before the tokens exist), CR 701.7a/b
//! (creating tokens; the replacement applies first), CR 707.1 + CR 707.2 (the
//! tokens are copies and acquire copiable values), CR 115.10a (a choice, not a
//! target), CR 111.2 (the creator owns and controls the tokens).
//!
//! Harness lineage: mirrors the Moonlit Meditation block in `std_longtail_e.rs`
//! (`GameScenario` → install the permanent with its PARSED replacements →
//! `resolve_token_source`). The helpers there are private `fn`s and `support.rs`
//! exports none of them, so the copies below are deliberate rather than an
//! oversight — promoting them would edit a file outside this change's scope.
//!
//! ⚠ Prompt counts are per row, never a blanket claim. Most rows drive a FLOOR
//! of two — the Optional accept, then the `CopyTargetChoice` — where Moonlit
//! drives one; a test that drives only the accept and then asserts on tokens is
//! measuring the wrong seam. That floor is not a ceiling: when the copied
//! permanent carries its own choice-raising entry replacement, each substitute
//! token raises one more, and the Dead Reveler rows below drive five.

use std::sync::Arc;

use engine::database::synthesis::synthesize_unleash;
use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::{AttachTarget, GameObject};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::turns::start_next_turn;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ControllerRef, CopyTargetPurpose, Effect, FilterProp, QuantityExpr, QuantityRef,
    ReplacementCondition, ReplacementDefinition, ReplacementMode, TargetFilter, TargetRef,
    TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// Verbatim printed Oracle text (Scryfall, oracle_id
/// 9d22960b-babc-4cf3-b228-d32e13bc6014). Note the SHORT self-name "other than
/// Esix" — the engine's self-reference normalizer folds it to `~`, and passing
/// the real card name below exercises that fold end-to-end.
const ESIX_ORACLE: &str = "Flying\n\
The first time you would create one or more tokens during each of your turns, you may \
instead choose a creature other than Esix and create that many tokens that are copies \
of that creature.";

fn parse(
    oracle: &str,
    name: &str,
    keywords: &[&str],
    types: &[&str],
    subtypes: &[&str],
) -> engine::parser::oracle::ParsedAbilities {
    let kw: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
    let t: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let s: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle, name, &kw, &t, &s)
}

fn assert_zero_unimplemented(parsed: &engine::parser::oracle::ParsedAbilities, name: &str) {
    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "{name}: expected zero Unimplemented nodes, parse was:\n{dbg}"
    );
}

/// Parse the real card. The name argument is what drives the `~` self-reference
/// fold, so it is load-bearing rather than cosmetic.
fn esix_parsed() -> engine::parser::oracle::ParsedAbilities {
    parse(
        ESIX_ORACLE,
        "Esix, Fractal Bloom",
        // Scryfall reports keywords ["Flying"]; the real loader passes MTGJSON's
        // keyword list here, so the standalone "Flying" line is a keyword rather
        // than an unparsed ability.
        &["Flying"],
        &["Legendary", "Creature"],
        &["Fractal"],
    )
}

/// The parsed `CreateToken` replacement Esix carries.
fn esix_replacement() -> ReplacementDefinition {
    esix_parsed()
        .replacements
        .into_iter()
        .find(|r| r.event == ReplacementEvent::CreateToken)
        .expect("Esix must parse to a CreateToken replacement")
}

// ---------------------------------------------------------------------------
// V1 / V2 / V3 — parse rows
// ---------------------------------------------------------------------------

/// V1 — Esix parses to the full chosen-source replacement with ZERO
/// `Unimplemented` nodes.
///
/// Revert the Axis-B chosen arm and the whole line falls back to a single
/// `Effect::unimplemented("replacement_structure", …)`: `replacements` is empty
/// and every assertion below fails. That is the MEASURED state before this
/// change, so this row is a genuine flip rather than a restatement.
///
/// The `Flying` assertion is the positive reach-guard: it proves the WHOLE card
/// parsed, so a green here cannot come from a card that silently produced no
/// abilities at all.
#[test]
fn esix_parses_to_chosen_copy_source_replacement() {
    let parsed = esix_parsed();
    assert_zero_unimplemented(&parsed, "Esix, Fractal Bloom");
    assert!(
        parsed.extracted_keywords.contains(&Keyword::Flying),
        "the whole card must parse, not just the replacement line; keywords were {:?}",
        parsed.extracted_keywords
    );

    let create_token: Vec<&ReplacementDefinition> = parsed
        .replacements
        .iter()
        .filter(|r| r.event == ReplacementEvent::CreateToken)
        .collect();
    assert_eq!(
        create_token.len(),
        1,
        "exactly one CreateToken replacement, got {:?}",
        parsed.replacements
    );
    let repl = create_token[0];

    // Axis A — the window is scoped to the controller's own turns (CR 102.1).
    assert_eq!(
        repl.condition,
        Some(ReplacementCondition::FirstTokenCreationEachTurn {
            active_player_req: Some(ControllerRef::You),
        }),
        "\"during each of your turns\" must bind active_player_req = You"
    );
    assert_eq!(repl.token_owner_scope, Some(ControllerRef::You));
    assert!(
        matches!(repl.mode, ReplacementMode::Optional { decline: None }),
        "\"you may instead\" is an Optional replacement, got {:?}",
        repl.mode
    );

    // Axis B — the copy source is CHOSEN, so the execute is a ChoosePermanent
    // whose sub_ability carries the copy.
    let execute = repl.execute.as_deref().expect("execute must be present");
    let Effect::ChoosePermanent { filter } = &*execute.effect else {
        panic!("expected a ChoosePermanent root, got {:?}", execute.effect);
    };
    let TargetFilter::Typed(typed) = filter else {
        panic!("expected a Typed choice filter, got {filter:?}");
    };
    assert_eq!(typed.type_filters, vec![TypeFilter::Creature]);
    assert!(
        typed.properties.contains(&FilterProp::Another),
        "CR 201.5: \"other than Esix\" is object identity — the filter must carry \
         FilterProp::Another, got {:?}",
        typed.properties
    );

    let sub = execute
        .sub_ability
        .as_deref()
        .expect("the chosen arm must carry the copy as a sub_ability");
    assert!(
        matches!(
            &*sub.effect,
            Effect::CopyTokenOf {
                target: TargetFilter::Any,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount
                },
                ..
            }
        ),
        "the tail copies \"that many\" tokens of the answered object, got {:?}",
        sub.effect
    );
}

/// V2 — the antecedent did NOT widen.
///
/// The `:742` dispatch slot tries Esix's combinator FIRST, so a loosened Axis-A
/// `alt` would steal one of the two adjacent grammars it sits in front of.
/// Jinnie Fay ("if you would create…") and Doubling Season ("if an effect
/// would…") are the hostile fixtures — they are exactly the cards the
/// dispatch-ordering comment names.
#[test]
fn esix_combinator_does_not_steal_adjacent_token_replacements() {
    // Jinnie Fay — a ChooseOneOf-rooted CreateToken replacement. Verbatim
    // Oracle text (Scryfall); a paraphrase can take a different parser branch.
    let jinnie = parse(
        "If you would create one or more tokens, you may instead create that many 2/2 green \
         Cat creature tokens with haste or that many 3/1 green Dog creature tokens with \
         vigilance.",
        "Jinnie Fay, Jetmir's Second",
        &[],
        &["Legendary", "Creature"],
        &["Elf", "Druid"],
    );
    let jinnie_repl = jinnie
        .replacements
        .iter()
        .find(|r| r.event == ReplacementEvent::CreateToken)
        .expect("Jinnie must still parse to a CreateToken replacement");
    let jinnie_execute = jinnie_repl
        .execute
        .as_deref()
        .expect("Jinnie's replacement must still carry an execute");
    assert!(
        matches!(&*jinnie_execute.effect, Effect::ChooseOneOf { .. }),
        "Jinnie must still be a ChooseOneOf-rooted substitution, got {:?}",
        jinnie_execute.effect
    );

    // Doubling Season — a quantity_modification doubler with NO execute.
    // Verbatim Oracle text (Scryfall), token half only.
    let doubling = parse(
        "If an effect would create one or more tokens under your control, it creates twice \
         that many of those tokens instead.",
        "Doubling Season",
        &[],
        &["Enchantment"],
        &[],
    );
    let doubling_repl = doubling
        .replacements
        .iter()
        .find(|r| r.event == ReplacementEvent::CreateToken)
        .expect("Doubling Season must still parse to a CreateToken replacement");
    assert!(
        doubling_repl.quantity_modification.is_some(),
        "Doubling Season must still carry a quantity_modification, got {doubling_repl:?}"
    );
    assert!(
        doubling_repl.execute.is_none(),
        "Doubling Season substitutes a COUNT, not an ability — it must carry no execute"
    );
}

/// V3 — the turn scope is read from the text, not hardcoded.
///
/// One test over both class members: Esix's "during each of your turns" binds
/// `Some(You)` and Moonlit's bare "each turn" binds `None`. Hardcode either
/// value and exactly one of these two assertions fails, so neither can be
/// satisfied by a constant.
#[test]
fn turn_scope_axis_binds_from_text_for_both_class_members() {
    assert_eq!(
        esix_replacement().condition,
        Some(ReplacementCondition::FirstTokenCreationEachTurn {
            active_player_req: Some(ControllerRef::You),
        }),
        "Esix: \"during each of your turns\" → Some(You)"
    );

    let moonlit = parse(
        "Enchant artifact or creature you control\n\
         The first time you would create one or more tokens each turn, you may instead \
         create that many tokens that are copies of enchanted permanent.",
        "Moonlit Meditation",
        &[],
        &["Enchantment"],
        &["Aura"],
    );
    let moonlit_repl = moonlit
        .replacements
        .iter()
        .find(|r| r.event == ReplacementEvent::CreateToken)
        .expect("Moonlit must still parse to a CreateToken replacement");
    assert_eq!(
        moonlit_repl.condition,
        Some(ReplacementCondition::FirstTokenCreationEachTurn {
            active_player_req: None,
        }),
        "Moonlit: a bare \"each turn\" leaves the window unscoped"
    );
    // The static arm must be untouched by the Axis-B generalization: its copy
    // sits at the ROOT, with no ChoosePermanent and no sub_ability.
    let moonlit_execute = moonlit_repl.execute.as_deref().expect("execute present");
    assert!(
        matches!(
            &*moonlit_execute.effect,
            Effect::CopyTokenOf {
                target: TargetFilter::AttachedTo,
                ..
            }
        ),
        "Moonlit's copy stays at the root against its Aura host, got {:?}",
        moonlit_execute.effect
    );
    assert!(
        moonlit_execute.sub_ability.is_none(),
        "the static arm raises no choice, so it carries no sub_ability"
    );
}

// ---------------------------------------------------------------------------
// Runtime harness
//
// Local copies of the Moonlit block's helpers: those are private `fn`s in
// `std_longtail_e.rs` and `support.rs` exports none of them, so reuse would mean
// editing a file outside this change's scope.
// ---------------------------------------------------------------------------

/// Put Esix on the battlefield under `controller`, carrying its PARSED
/// replacement (never a hand-built one — the parse is half of what is under
/// test). Legendary, per the printed type line.
fn install_esix(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(970),
        controller,
        "Esix, Fractal Bloom".to_string(),
        Zone::Battlefield,
    );
    let reps = vec![esix_replacement()];
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.card_types.supertypes = vec![Supertype::Legendary];
    obj.card_types.subtypes = vec!["Fractal".to_string()];
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
    id
}

/// Resolve a token-creating sorcery controlled by `controller`, driving the real
/// token pipeline (propose → `replace_event`). If Esix's optional replacement
/// applies, the pipeline parks on `WaitingFor::ReplacementChoice`.
fn resolve_token_source(runner: &mut GameRunner, controller: PlayerId, oracle: &str) {
    let parsed = parse_oracle_text(oracle, "Token Source", &[], &["Sorcery".to_string()], &[]);
    let def = parsed
        .abilities
        .first()
        .expect("token source should parse to an ability");
    let src = create_object(
        runner.state_mut(),
        CardId(971),
        controller,
        "Token Source".to_string(),
        Zone::Stack,
    );
    let ability = build_resolved_from_def(def, src, controller);
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("token effect should resolve");
}

/// Give a permanent a distinctive subtype on BOTH the live and base card types —
/// `CopyTokenOf` reads copiable values from `base_card_types`, so a copy
/// inherits the subtype only if the base carries it.
fn set_copiable_subtype(state: &mut GameState, id: ObjectId, subtype: &str) {
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.subtypes = vec![subtype.to_string()];
    obj.base_card_types.subtypes = vec![subtype.to_string()];
}

fn at_replacement_choice(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    )
}

/// Assert the engine is parked on the copy-SOURCE prompt specifically (not the
/// Aura-flavoured `PersistChosenAttribute` one) and return its legal pool.
/// Asserting `purpose` here is what makes the raise-site discriminant testable:
/// a revert to the hardcoded `PersistChosenAttribute` fails right here.
fn copy_token_source_targets(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::CopyTargetChoice {
            valid_targets,
            purpose,
            ..
        } => {
            assert_eq!(
                *purpose,
                CopyTargetPurpose::CopyTokenSource,
                "the chosen-source arm must raise CopyTokenSource, not the \
                 Metamorphic Alteration disposition"
            );
            valid_targets.clone()
        }
        other => panic!("expected a CopyTargetChoice prompt, got {other:?}"),
    }
}

/// Battlefield tokens under `controller` with the given name.
fn tokens_named<'a>(
    runner: &'a GameRunner,
    name: &str,
    controller: PlayerId,
) -> Vec<&'a GameObject> {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| o.is_token && o.controller == controller && o.name == name)
        .collect()
}

fn has_subtype(obj: &GameObject, subtype: &str) -> bool {
    obj.card_types
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case(subtype))
}

/// Three 1/1 Soldier tokens — a multi-token source, so a count that silently
/// collapsed to 1 cannot pass any row vacuously.
const THREE_SOLDIERS: &str = "Create three 1/1 white Soldier creature tokens.";

// ---------------------------------------------------------------------------
// V5 — accept → N tokens copying the CHOSEN creature
// ---------------------------------------------------------------------------

/// V5 — the substitute tokens copy the permanent the player ANSWERED with, not
/// "the first legal candidate" and not the original token spec.
///
/// Two prompts: the Optional accept, then the `CopyTargetChoice`.
///
/// Revert the raise-site discriminant to a hardcoded `PersistChosenAttribute`
/// and this fails twice over: `copy_token_source_targets` rejects the wrong
/// `purpose`, and the answer would route into
/// `handle_persist_chosen_attribute_choice`, whose Aura `debug_assert!` ("the
/// Aura must remain attached after installing the host copy") Esix cannot
/// satisfy — it has no `attached_to`.
///
/// "Wrong Ox" is the multi-authority hostile fixture: an equally legal decoy
/// that must NOT be copied. Without it, a handler that copied any legal
/// candidate would pass.
#[test]
fn accept_creates_copies_of_the_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
    let wrong = scenario.add_creature(P0, "Wrong Ox", 2, 2).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), decoy, "Ox");
    set_copiable_subtype(runner.state_mut(), wrong, "Boar");
    install_esix(runner.state_mut(), P0);

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);

    // Prompt 1 — the Optional accept.
    assert!(
        at_replacement_choice(&runner),
        "Esix's optional substitution must be offered, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Esix's substitution");

    // Prompt 2 — the copy-source choice.
    let pool = copy_token_source_targets(&runner);
    assert!(
        pool.contains(&decoy) && pool.contains(&wrong),
        "both decoys are legal copy sources, pool was {pool:?}"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(decoy)),
        })
        .expect("choose Decoy Ox as the copy source");
    runner.advance_until_stack_empty();

    let copies = tokens_named(&runner, "Decoy Ox", P0);
    assert_eq!(
        copies.len(),
        3,
        "\"that many\" is 3, so accepting yields exactly 3 copies of the chosen \
         creature; battlefield was {:?}",
        runner
            .state()
            .battlefield
            .iter()
            .filter_map(|id| runner.state().objects.get(id))
            .map(|o| (o.name.clone(), o.is_token))
            .collect::<Vec<_>>()
    );
    for copy in &copies {
        assert!(
            has_subtype(copy, "Ox"),
            "each copy carries the chosen creature's copiable subtype, got {:?}",
            copy.card_types.subtypes
        );
        assert_eq!(
            (copy.power, copy.toughness),
            (Some(5), Some(4)),
            "each copy has the chosen creature's P/T, not the 1/1 Soldier spec"
        );
        assert!(
            !has_subtype(copy, "Soldier"),
            "the original token spec was REPLACED, not created alongside"
        );
    }
    assert!(
        tokens_named(&runner, "Wrong Ox", P0).is_empty(),
        "the equally legal decoy that was NOT answered must not be copied"
    );
}

/// Put a Moonlit Meditation Aura on the battlefield attached to `host` — the
/// Axis-A sibling whose window is UNSCOPED (`active_player_req: None`). Used as
/// the negative control proving turn-scope suppression comes from the new
/// binding rather than a globally broken matcher.
fn install_moonlit(state: &mut GameState, host: ObjectId, controller: PlayerId) -> ObjectId {
    let parsed = parse(
        "Enchant artifact or creature you control\n\
         The first time you would create one or more tokens each turn, you may instead \
         create that many tokens that are copies of enchanted permanent.",
        "Moonlit Meditation",
        &[],
        &["Enchantment"],
        &["Aura"],
    );
    let reps: Vec<ReplacementDefinition> = parsed
        .replacements
        .into_iter()
        .filter(|r| r.event == ReplacementEvent::CreateToken)
        .collect();
    assert_eq!(reps.len(), 1, "Moonlit must parse to one CreateToken rep");
    let id = create_object(
        state,
        CardId(972),
        controller,
        "Moonlit Meditation".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.card_types.subtypes = vec!["Aura".to_string()];
    obj.attached_to = Some(AttachTarget::Object(host));
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
    id
}

/// Cross a turn boundary through the real turn machinery. Never poke
/// `state.active_player` by hand — the window reset lives in `start_next_turn`
/// and hand-poking would skip exactly the code under test.
fn cross_turn(runner: &mut GameRunner) {
    let mut events = Vec::<GameEvent>::new();
    start_next_turn(runner.state_mut(), &mut events);
}

// ---------------------------------------------------------------------------
// V6 — turn scope at runtime
// ---------------------------------------------------------------------------

/// V6 — Esix's window does not open on an OPPONENT's turn.
///
/// CR 102.1: `active_player_req: Some(You)` means the controller must be the
/// active player. Hardcode `active_player_req: None` in the parser (phase 1's
/// placeholder) and the first assertion fails: the prompt appears on P1's turn.
///
/// Two guards keep the negative honest:
///   * the positive reach-guard — crossing back to P0's turn DOES prompt, so the
///     silence above is scope rejection rather than a dead Esix; and
///   * the Moonlit sibling — an unscoped `active_player_req: None` replacement
///     under the same controller DOES prompt on the opponent's turn, so the
///     suppression comes from the new binding, not a broken matcher.
#[test]
fn window_does_not_open_on_an_opponents_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), decoy, "Ox");
    install_esix(runner.state_mut(), P0);

    // Cross to P1's turn. P0 still creates the tokens (token_owner_scope You),
    // but P0 is no longer the active player.
    cross_turn(&mut runner);
    assert_eq!(
        runner.state().active_player,
        P1,
        "now on the opponent's turn"
    );

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        !at_replacement_choice(&runner),
        "Esix's window is scoped to its controller's OWN turns, so no prompt on \
         the opponent's turn; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        tokens_named(&runner, "Decoy Ox", P0).len(),
        0,
        "no substitution happened, so no copies exist"
    );

    // Positive reach-guard: back on P0's turn the same board DOES prompt.
    cross_turn(&mut runner);
    assert_eq!(
        runner.state().active_player,
        P0,
        "back on the controller's turn"
    );
    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        at_replacement_choice(&runner),
        "reach-guard: on its controller's own turn the same Esix DOES prompt — \
         so the silence above was scope rejection, not a dead replacement"
    );
}

/// V6b — the negative sibling, isolated so the two windows cannot interfere:
/// Moonlit's UNSCOPED window still prompts on the opponent's turn.
///
/// This is what proves V6's silence is attributable to `active_player_req`
/// rather than to `replacement_active_player_matches` being broken for
/// everything.
#[test]
fn unscoped_sibling_still_prompts_on_an_opponents_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), host, "Ox");
    install_moonlit(runner.state_mut(), host, P0);

    cross_turn(&mut runner);
    assert_eq!(runner.state().active_player, P1);

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        at_replacement_choice(&runner),
        "Moonlit's window carries active_player_req: None, so it is NOT scoped to \
         its controller's turns and must still prompt here; got {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// V7 — the choice pool is by OBJECT identity, not by name
// ---------------------------------------------------------------------------

/// V7 — Esix itself is not a legal copy source, and the exclusion is by object
/// id rather than by name.
///
/// CR 201.5: "other than Esix" means that particular object, not every object
/// with that name. So a token COPY of Esix — same name, different `ObjectId` —
/// IS a legal choice.
///
/// ⚠ The copy must be under an OPPONENT's control. CR 704.5j (the legend rule)
/// is per player, so a second legendary Esix under P0 would be put into the
/// graveyard as a state-based action before the pool could be observed.
///
/// The assertion is on the pool's exact contents and length — never a bare
/// `!contains(esix)`, which an empty pool would satisfy vacuously.
#[test]
fn esix_excludes_itself_by_object_identity_not_by_name() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), decoy, "Ox");
    let esix = install_esix(runner.state_mut(), P0);

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        at_replacement_choice(&runner),
        "reach-guard: the prompt appears"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept");

    let pool = copy_token_source_targets(&runner);
    assert_eq!(
        pool,
        vec![decoy],
        "the only legal copy source is the decoy: Esix excludes ITSELF, and the \
         pool is asserted exactly rather than by absence"
    );
    assert!(!pool.contains(&esix), "Esix is not a legal copy of itself");
}

/// V7b — the hostile half: a token copy of Esix under an OPPONENT's control has
/// the same NAME but a different `ObjectId`, so CR 201.5 makes it a legal
/// choice. If the exclusion were name-based the pool would be length 1.
///
/// Observed in the pool only, never answered: choosing it would create N
/// legendary Esix copies under one controller and re-enter CR 704.5j, which is a
/// different rule's territory.
#[test]
fn an_opponents_esix_token_copy_is_a_legal_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), decoy, "Ox");
    let esix = install_esix(runner.state_mut(), P0);

    // A same-named token copy under the OPPONENT — distinct object identity.
    let impostor = create_object(
        runner.state_mut(),
        CardId(973),
        P1,
        "Esix, Fractal Bloom".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&impostor).unwrap();
        obj.is_token = true;
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types.core_types = vec![CoreType::Creature];
        obj.power = Some(4);
        obj.toughness = Some(4);
    }

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        at_replacement_choice(&runner),
        "reach-guard: the prompt appears"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept");

    let pool = copy_token_source_targets(&runner);
    assert_eq!(
        pool.len(),
        2,
        "CR 201.5: identity is by object, so the opponent's same-named copy IS \
         legal — a name-based exclusion would give a pool of 1. Pool: {pool:?}"
    );
    assert!(
        pool.contains(&impostor),
        "the opponent's Esix token copy must be a legal choice"
    );
    assert!(!pool.contains(&esix), "the real Esix still excludes itself");
}

// ---------------------------------------------------------------------------
// V8 — the "that many" count survives the interactive round-trip
// ---------------------------------------------------------------------------

/// V8 — the replaced event's `count` is latched at the stamp site and is still
/// live when the copy tail resolves, ACROSS the `CopyTargetChoice` round-trip.
///
/// The mid-prompt assertion is a direct probe: with the `CopyTargetChoice` still
/// up, `post_replacement_token_substitution_count` must already read `Some(3)`.
/// Revert the `is_copy_token_substitution` widening and Esix's
/// `ChoosePermanent`-rooted tree stops matching at the stamp site, so the count
/// is never written, the mid-prompt read is `None`, and the copy count collapses.
///
/// Three tokens, not one, so `count == 1` cannot pass vacuously.
#[test]
fn that_many_count_survives_the_copy_source_round_trip() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), decoy, "Ox");
    install_esix(runner.state_mut(), P0);

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        at_replacement_choice(&runner),
        "reach-guard: the prompt appears"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept");

    // MID-PROMPT: taken before the handler retires anything.
    let _ = copy_token_source_targets(&runner);
    assert_eq!(
        runner.state().post_replacement_token_substitution_count,
        Some(3),
        "the replaced event's count must be latched and still live while the \
         copy-source prompt is up — this is the seam the widening feeds"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(decoy)),
        })
        .expect("choose the copy source");
    runner.advance_until_stack_empty();

    assert_eq!(
        tokens_named(&runner, "Decoy Ox", P0).len(),
        3,
        "the latched count drives the copy count after the round-trip"
    );
}

// ---------------------------------------------------------------------------
// V9 / V10 — the once-per-turn window
// ---------------------------------------------------------------------------

/// V9 — declining keeps the ORIGINAL tokens and still consumes the window.
///
/// Ruling: "If you choose not to apply the replacement effect, you will not get
/// the choice to apply it again until your next turn." Declining falls through
/// to the unreplaced `CreateToken`, which records the creator in
/// `players_who_created_token_this_turn` — so the second creation this turn does
/// not prompt.
///
/// Declining must also raise NO `CopyTargetChoice`: the copy source is only
/// chosen when the substitution is actually applied.
#[test]
fn decline_keeps_the_original_tokens_and_consumes_the_window() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), decoy, "Ox");
    install_esix(runner.state_mut(), P0);

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        at_replacement_choice(&runner),
        "reach-guard: the first creation this turn DOES prompt"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline");
    runner.advance_until_stack_empty();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::CopyTargetChoice { .. }
        ),
        "declining never asks for a copy source; got {:?}",
        runner.state().waiting_for
    );
    let soldiers: Vec<&GameObject> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| o.is_token && o.controller == P0 && has_subtype(o, "Soldier"))
        .collect();
    assert_eq!(
        soldiers.len(),
        3,
        "declining falls through to the unreplaced event: the three original \
         Soldier tokens are created"
    );
    assert!(
        tokens_named(&runner, "Decoy Ox", P0).is_empty(),
        "no copies are created on decline"
    );
    assert!(
        runner
            .state()
            .players_who_created_token_this_turn
            .contains(&P0),
        "the original creation records the player, consuming the window"
    );

    // The window is spent for the rest of this turn.
    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        !at_replacement_choice(&runner),
        "\"the first time ... each of your turns\" — a second creation the same \
         turn must not prompt"
    );
}

/// V10 — window mechanics across turn boundaries.
///
/// (a) a second creation on the same turn does not prompt;
/// (b) after the controller's NEXT turn begins, it prompts again — the positive
///     reach-guard, and the assertion that reverts if the turn reset is removed;
/// (c) a creation on the intervening OPPONENT's turn neither prompts nor
///     consumes the controller's next-turn window. That is the multi-authority
///     case: an opponent-turn creation must not be what spends the window.
#[test]
fn window_resets_on_the_controllers_next_turn_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), decoy, "Ox");
    install_esix(runner.state_mut(), P0);

    // Turn 1 (P0): spend the window by declining.
    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(at_replacement_choice(&runner), "turn 1: the window is open");
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline");
    runner.advance_until_stack_empty();

    // (a) same turn again → no prompt.
    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        !at_replacement_choice(&runner),
        "(a) the window is spent for the remainder of this turn"
    );

    // (c) the opponent's turn: no prompt, and it must not spend P0's next window.
    cross_turn(&mut runner);
    assert_eq!(runner.state().active_player, P1);
    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        !at_replacement_choice(&runner),
        "(c) not the controller's turn, so the window does not open"
    );

    // (b) back to P0's turn → the window has reset and the prompt returns.
    cross_turn(&mut runner);
    assert_eq!(runner.state().active_player, P0);
    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        at_replacement_choice(&runner),
        "(b) the controller's next turn reopens the window — this is the \
         assertion that fails if the turn-boundary reset is removed; got {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// V11 — no legal copy source
// ---------------------------------------------------------------------------

/// V11 — with Esix as the only creature there is no legal copy source, and the
/// engine must not strand.
///
/// CR 609.3: an effect does as much as possible. `find_copy_targets` comes back
/// empty, so `apply_post_replacement_effect` returns `None` and NO
/// `CopyTargetChoice` is raised — the empty-pool early return is the seam. The
/// game must settle rather than park on a prompt nobody can answer.
///
/// This is the empty / no-legal-choice hostile row.
#[test]
fn no_legal_copy_source_does_not_strand_the_game() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    install_esix(runner.state_mut(), P0);

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    assert!(
        at_replacement_choice(&runner),
        "the substitution is still OFFERED — the window condition does not \
         consult the copy pool"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept with no legal copy source");
    runner.advance_until_stack_empty();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::CopyTargetChoice { .. }
        ),
        "no legal source → no prompt is raised on an empty pool; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the game must settle at Priority rather than strand; got {:?}",
        runner.state().waiting_for
    );
}

// ---------------------------------------------------------------------------
// M-b — the substitution applies to a NON-CREATURE token
// ---------------------------------------------------------------------------

/// M-b — ruling: "This effect can apply to any token, not just creature tokens."
///
/// `FirstTokenCreationEachTurn` gates on the per-turn window alone; it does not
/// consult `ReplacementCondition`'s sibling `TokenCoreTypeMatches` axis. So a
/// Clue (a non-creature artifact token) must still raise the prompt, and
/// accepting must yield CREATURE copies rather than Clues.
///
/// Add a token-core-type guard to the condition and this row fails while every
/// creature-token row stays green — which is what makes it discriminating rather
/// than a restatement of V5.
#[test]
fn substitution_applies_to_non_creature_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), decoy, "Ox");
    install_esix(runner.state_mut(), P0);

    resolve_token_source(&mut runner, P0, "Create three Clue tokens.");
    assert!(
        at_replacement_choice(&runner),
        "a non-creature token creation must still open the window; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept on a Clue creation");
    let _ = copy_token_source_targets(&runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(decoy)),
        })
        .expect("choose the copy source");
    runner.advance_until_stack_empty();

    assert_eq!(
        tokens_named(&runner, "Decoy Ox", P0).len(),
        3,
        "the Clues are replaced by three copies of the chosen CREATURE"
    );
    let clues: Vec<&GameObject> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| o.is_token && o.controller == P0 && o.name == "Clue")
        .collect();
    assert!(
        clues.is_empty(),
        "the original Clue tokens were REPLACED, not created alongside; got {:?}",
        clues.iter().map(|o| o.name.clone()).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// M-a — CR 614.5 anti-recursion, with a second source as the hostile fixture
// ---------------------------------------------------------------------------

/// Doubling Season's token half — a DIFFERENT source's mandatory `CreateToken`
/// doubler. Verbatim Oracle text (Scryfall), token clause only.
fn install_doubling_season(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let parsed = parse(
        "If an effect would create one or more tokens under your control, it creates twice \
         that many of those tokens instead.",
        "Doubling Season",
        &[],
        &["Enchantment"],
        &[],
    );
    let reps: Vec<ReplacementDefinition> = parsed
        .replacements
        .into_iter()
        .filter(|r| r.event == ReplacementEvent::CreateToken)
        .collect();
    assert_eq!(reps.len(), 1, "Doubling Season's token doubler must parse");
    let id = create_object(
        state,
        CardId(974),
        controller,
        "Doubling Season".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
    id
}

/// M-a — CR 614.5: "a replacement effect doesn't invoke itself repeatedly; it
/// gets only one opportunity to affect an event or any modified events that may
/// replace that event."
///
/// The substitute copies are themselves a token creation, so without the
/// inherited applied set Esix would see them as a fresh `CreateToken` and
/// re-prompt — compounding the copies. `set_replacement_applied_recursive` in
/// `handle_copy_token_source_choice` is the SOLE carrier of that suppression on
/// this route; drop it and the copy-source prompt count below exceeds 1 and/or
/// the copy count compounds.
///
/// Multi-authority hostile fixture: Doubling Season is a DIFFERENT source's
/// mandatory doubler. CR 614.5 suppresses only Esix's own re-entry, so Doubling
/// Season must still apply to the substitute copies — exactly 2, not 1 (which
/// would mean the doubling was wrongly suppressed too) and not 4 (compounding).
#[test]
fn substitute_copies_do_not_re_enter_esixs_own_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), decoy, "Ox");
    install_esix(runner.state_mut(), P0);
    install_doubling_season(runner.state_mut(), P0);

    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );

    // Drive every prompt to completion, COUNTING the copy-source prompts. The
    // count is itself the CR 614.5 assertion: a second one means Esix re-entered.
    let mut copy_source_prompts = 0;
    for _ in 0..12 {
        if at_replacement_choice(&runner) {
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("apply replacement");
            runner.advance_until_stack_empty();
        } else if let WaitingFor::CopyTargetChoice { .. } = runner.state().waiting_for {
            copy_source_prompts += 1;
            let _ = copy_token_source_targets(&runner);
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(decoy)),
                })
                .expect("choose the copy source");
            runner.advance_until_stack_empty();
        } else {
            break;
        }
    }

    assert_eq!(
        copy_source_prompts, 1,
        "CR 614.5: Esix gets ONE opportunity — the substitute copies must not \
         raise a second copy-source prompt"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the flow settles at Priority; got {:?}",
        runner.state().waiting_for
    );
    let copies = tokens_named(&runner, "Decoy Ox", P0);
    assert_eq!(
        copies.len(),
        2,
        "one token, doubled by a DIFFERENT source, substituted into copies → \
         exactly 2. Not 1 (Doubling Season wrongly suppressed) and not 4+ \
         (Esix compounding on its own copies)"
    );
    for c in &copies {
        assert_eq!(
            (c.power, c.toughness),
            (Some(5), Some(4)),
            "each is a copy of the chosen creature, not a copy-of-copy or a Soldier"
        );
    }
}

// ---------------------------------------------------------------------------
// M-c-doc — documentation-grade observation, NOT a verification row
// ---------------------------------------------------------------------------

/// M-c-doc — ⚠ THIS ASSERTION CANNOT FAIL, under a correct implementation or a
/// broken one, and it is recorded here so no later reader mistakes it for
/// evidence.
///
/// `capture_deferred_entry_events_if_mid_entry_choice` clears
/// `state.deferred_entry_events` UNCONDITIONALLY once its `CopyTargetChoice` arm
/// selects, and its capture loop only pushes a `ZoneChanged { to: Battlefield }`
/// for the prompt's own `source_id`. Esix is a resident permanent that is not
/// entering, so the loop matches nothing and the vector is observably empty —
/// but that is a property of THIS fixture, not of the code: the copy batch calls
/// the same dispatcher with an *entering* token as `source_id`, so a fixture
/// whose copies raise a mid-entry choice could populate it.
///
/// There is no revert that flips this. It is kept as a canary that would begin
/// to discriminate the day the capture arm gained a `purpose` guard, or the day
/// an Esix-class source started entering.
#[test]
fn deferred_entry_events_are_observably_empty_for_a_non_entering_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let decoy = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
    let mut runner = scenario.build();
    set_copiable_subtype(runner.state_mut(), decoy, "Ox");
    install_esix(runner.state_mut(), P0);

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept");
    let _ = copy_token_source_targets(&runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(decoy)),
        })
        .expect("choose the copy source");
    runner.advance_until_stack_empty();

    assert!(
        runner.state().deferred_entry_events.is_empty(),
        "observed empty for a non-entering source (see the doc above: this \
         assertion cannot fail, and is not evidence)"
    );
}

// ---------------------------------------------------------------------------
// V15 — a prompt raised DURING the substitute copies' creation must SURVIVE
// ---------------------------------------------------------------------------

/// Give a creature the real Unleash entry replacement, synthesized by the same
/// production function the card database uses (`synthesize_unleash`) rather than
/// hand-built — CR 702.98a: "You may have this permanent enter with an
/// additional +1/+1 counter on it." It is `Optional`, which is what makes it
/// raise a choice per entering token instead of applying silently.
fn grant_unleash(state: &mut GameState, id: ObjectId) {
    let mut face = CardFace {
        name: "Dead Reveler".to_string(),
        keywords: vec![Keyword::Unleash],
        ..CardFace::default()
    };
    synthesize_unleash(&mut face);
    let reps: Vec<ReplacementDefinition> = face.replacements.clone();
    assert!(
        reps.iter().any(|r| {
            r.event == ReplacementEvent::Moved && matches!(r.mode, ReplacementMode::Optional { .. })
        }),
        "reach-guard: Unleash must synthesize an Optional Moved replacement, got {reps:?}"
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
}

fn p1p1_counters(obj: &GameObject) -> u32 {
    obj.counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// V15 reach control — INDEPENDENT of Esix. A Dead Reveler copy token entering
/// via the STATIC arm (Moonlit) raises its own Unleash `ReplacementChoice`.
///
/// This exists so that a V15 failure can be attributed. If this control is red,
/// the fixture is wrong (the copies never acquire Unleash, CR 707.2) and the
/// defect is not in the answer handler; if it is green and V15 is red, the
/// prompt is being raised and then swallowed — which is exactly V15's subject.
#[test]
fn a_copy_of_dead_reveler_raises_its_own_entry_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reveler = scenario.add_creature(P0, "Dead Reveler", 3, 2).id();
    let mut runner = scenario.build();
    grant_unleash(runner.state_mut(), reveler);
    install_moonlit(runner.state_mut(), reveler, P0);

    resolve_token_source(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
    );
    assert!(at_replacement_choice(&runner), "Moonlit prompts");
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the substitution");

    assert!(
        at_replacement_choice(&runner),
        "the entering copy carries Unleash (CR 707.2: the copy acquires the \
         copiable values, which include rules text) and must raise its own \
         entry choice; got {:?}",
        runner.state().waiting_for
    );
}

/// V15 — ⚠ FIVE prompts, and the third is the point of the row.
///
/// Flow: 1 accept · 1 `CopyTargetChoice` · THREE per-token `ReplacementChoice`s.
/// The three are PER TOKEN, not per batch: `token_copy.rs`'s
/// `for index in 0..final_count` loop raises its own `ProposedEvent::TokenEntry`
/// per index, and the `NeedsChoice` arm writes `state.waiting_for` and returns
/// `Paused` immediately, carrying `remaining_count`; resumption re-enters the
/// loop and the next token raises its own choice.
///
/// THE REVERT-FAILING ASSERTION is the one taken immediately after answering the
/// copy-source choice: `state.waiting_for` must be a `ReplacementChoice`, NOT
/// `Priority`. Write the handler's retire step unconditionally
/// (`state.waiting_for = Priority`) and that prompt is destroyed and the parked
/// copy batch stranded behind it. This assertion is what catches that, and no
/// fixture whose copies raise no entry prompt of their own can reach it —
/// which is why the fixture is Dead Reveler rather than a bare decoy.
///
/// The 3-iteration loop count is itself an assertion: a batch that silently
/// collapsed to one prompt fails rather than passing.
#[test]
fn a_prompt_raised_during_the_copy_batch_survives_the_answer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reveler = scenario.add_creature(P0, "Dead Reveler", 3, 2).id();
    let mut runner = scenario.build();
    grant_unleash(runner.state_mut(), reveler);
    install_esix(runner.state_mut(), P0);

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);

    // Prompt 1 — accept.
    assert!(
        at_replacement_choice(&runner),
        "prompt 1: the Optional accept"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept");

    // Prompt 2 — the copy source.
    let pool = copy_token_source_targets(&runner);
    assert!(
        pool.contains(&reveler),
        "Dead Reveler is a legal copy source"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(reveler)),
        })
        .expect("choose Dead Reveler as the copy source");

    // ⚠ THE REVERT: under an unconditional `waiting_for = Priority` this reads
    // Priority and the test fails here, before anything below runs.
    assert!(
        at_replacement_choice(&runner),
        "the copy batch parked and raised its OWN entry prompt; answering Esix's \
         choice must PROPAGATE it, never overwrite it with Priority. Got {:?}",
        runner.state().waiting_for
    );

    // Prompts 3, 4, 5 — one per entering token. The count is the assertion.
    let mut entry_prompts = 0;
    for _ in 0..8 {
        if !at_replacement_choice(&runner) {
            break;
        }
        entry_prompts += 1;
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("accept the entering copy's Unleash counter");
    }
    assert_eq!(
        entry_prompts, 3,
        "one entry prompt PER TOKEN, not per batch — a batch that collapsed to a \
         single prompt fails here rather than passing silently"
    );

    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "once every nested prompt is answered the flow settles at Priority — the \
         other half of the defect is a surviving prompt that nothing retires; \
         got {:?}",
        runner.state().waiting_for
    );

    let copies = tokens_named(&runner, "Dead Reveler", P0);
    assert_eq!(copies.len(), 3, "three substitute copies were created");
    for c in &copies {
        assert_eq!(
            p1p1_counters(c),
            1,
            "each copy accepted its own Unleash counter (CR 702.98a), proving the \
             surviving prompt was genuinely actionable rather than a residual value"
        );
    }
}

/// V15b — the decline branch. Same three per-token prompts, declined: three
/// copies with NO counters. This is the second positive: it proves the prompts
/// that survived were real, answerable choices in both directions rather than a
/// stale value left in `waiting_for`.
#[test]
fn the_surviving_entry_prompts_can_also_be_declined() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reveler = scenario.add_creature(P0, "Dead Reveler", 3, 2).id();
    let mut runner = scenario.build();
    grant_unleash(runner.state_mut(), reveler);
    install_esix(runner.state_mut(), P0);

    resolve_token_source(&mut runner, P0, THREE_SOLDIERS);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept");
    let _ = copy_token_source_targets(&runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(reveler)),
        })
        .expect("choose Dead Reveler");

    let mut entry_prompts = 0;
    for _ in 0..8 {
        if !at_replacement_choice(&runner) {
            break;
        }
        entry_prompts += 1;
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("decline the Unleash counter");
    }
    assert_eq!(entry_prompts, 3, "still one prompt per entering token");

    runner.advance_until_stack_empty();
    let copies = tokens_named(&runner, "Dead Reveler", P0);
    assert_eq!(
        copies.len(),
        3,
        "declining the rider still creates the copies"
    );
    for c in &copies {
        assert_eq!(
            p1p1_counters(c),
            0,
            "declined → no +1/+1 counter, so the prompt genuinely drove the outcome"
        );
    }
}
