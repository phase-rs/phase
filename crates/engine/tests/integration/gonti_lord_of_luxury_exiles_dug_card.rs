//! Discriminating regression test for **issue #1146**: Gonti, Lord of Luxury's
//! ETB ("look at the top four cards … exile one of them face down, then you may
//! look at and play that card …") must exile the player-CHOSEN dug card — never
//! Gonti himself.
//!
//! Root cause (pre-fix): the parser lowered "exile one of them face down" as a
//! `Dig { keep_count: 0 }` pure-peek (CR 701.20e) plus a SEPARATE sibling
//! `ChangeZone { target: ParentTarget → Exile }`. At runtime the keep_count:0
//! Dig short-circuited WITHOUT surfacing a `WaitingFor::DigChoice` (no card was
//! ever selected), and the chained `ChangeZone { ParentTarget }` resolved with an
//! empty object-target set → the `ParentTarget && targets.is_empty()` fallback in
//! `effect_object_targets` used `ability.source_id` = Gonti, so GONTI was exiled.
//!
//! Fix (parser): when "exile one of them face down" follows a private "look at
//! top N" `Dig`, fuse it into `Dig { keep_count: Some(1), destination: Exile }`
//! (the Hideaway model, CR 702.75a) plus a chained `HideawayConceal` that flips
//! the dug card face down (CR 406.3) and links it to the source. The dug card is
//! now player-selected through the real `DigChoice` flow and routed to exile by
//! the Dig itself — no sibling `ChangeZone` exists.
//!
//! This test drives the full cast pipeline: it casts Gonti from hand, lets the
//! ETB trigger fire, and answers the `DigChoice` that the fix introduces.
//! Pre-fix, no `DigChoice` ever surfaces (the assertion `saw_dig_choice` fails)
//! AND Gonti ends in exile — both flip with the fix.
//!
//! Note on the Oracle text: `GONTI_ORACLE` below is Gonti's REAL printed text
//! as published by MTGJSON, verbatim. It previously held a hand-written
//! paraphrase saying "an opponent's library" (and "you may look at and play
//! that card"), which matches no printing of this card. That paraphrase reached
//! `parse_dig_library_owner`'s old `TargetFilter::Controller` fallthrough, so
//! the dig read the CONTROLLER's library and the fixture below was stacked on
//! `P0` to suit. #8498 made that recognizer fail-closed, which is what exposed
//! the paraphrase: an owner phrase the table cannot bind now declines the dig
//! arm instead of silently guessing the controller.
//!
//! With the real text the dig binds the announced target ("target opponent's
//! library" → `Typed{controller: Opponent}`, CR 115.1), so the fixture is
//! stacked on `P1` and the trigger's target is answered below. The #1146
//! property under test — WHICH card is exiled — is unchanged by whose library
//! is read.
//!
//! CR 701.20e: looking at cards is private. CR 406.3 / CR 708.2: a card exiled
//! face down has no characteristics and can't be examined. CR 702.75a: Hideaway
//! is the structural analog this lowering mirrors.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const GONTI_ORACLE: &str = "Deathtouch\n\
When Gonti enters, look at the top four cards of target opponent's library, exile one of them face down, then put the rest on the bottom of that library in a random order. You may cast that card for as long as it remains exiled, and mana of any type can be spent to cast that spell.";

#[test]
fn gonti_exiles_the_dug_card_not_himself() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Stack the DUG player's library — P1, the opponent Gonti targets (CR 115.1)
    // — so the top four are the looked-at cards and a fifth deeper card must NOT
    // be seen (proves the dig is bounded to 4). `add_card_to_library_top` inserts
    // at the top (index 0), so add the deepest card first and the top-of-four
    // last.
    let lib_deep = scenario.add_card_to_library_top(P1, "Lib Deep Card");
    let lib4 = scenario.add_card_to_library_top(P1, "Lib Card 4");
    let lib3 = scenario.add_card_to_library_top(P1, "Lib Card 3");
    let lib2 = scenario.add_card_to_library_top(P1, "Lib Card 2");
    let lib1 = scenario.add_card_to_library_top(P1, "Lib Card 1");

    // Gonti in P0's hand, free to cast.
    let gonti = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Gonti, Lord of Luxury",
            2,
            3,
            GONTI_ORACLE,
        );
        b.as_legendary();
        b.with_mana_cost(ManaCost::default());
        b.id()
    };

    let mut runner = scenario.build();
    let dig = cast_gonti_and_dig(&mut runner, gonti);

    // DISCRIMINATOR (#1146), part 1: the keep_count:1 fix surfaces a DigChoice.
    // Pre-fix the keep_count:0 pure-peek short-circuited and this never fired.
    let (_, looked_at, dug_card) = dig.expect(
        "Gonti's ETB must surface a DigChoice so the controller selects the card to exile; \
         pre-fix the keep_count:0 peek short-circuited and no choice was offered",
    );

    // The dig looked at exactly the top four cards (CR 701.20e) — not the deeper
    // card — proving the keep_count:1 dig is bounded to the four looked-at cards.
    assert_eq!(looked_at.len(), 4, "Gonti looks at the top FOUR cards");
    assert!(
        !looked_at.contains(&lib_deep),
        "the deeper (5th) card must not be looked at"
    );

    let state = runner.state();

    // DISCRIMINATOR (#1146), part 2 — the regression direction: Gonti is NOT
    // exiled. Pre-fix the sibling ChangeZone{ParentTarget} exiled the trigger
    // source (Gonti) because no object target had been selected.
    assert_eq!(
        state.objects[&gonti].zone,
        Zone::Battlefield,
        "Gonti must remain on the battlefield — the dug card is exiled, not Gonti"
    );
    assert!(
        !state.objects[&gonti].face_down,
        "Gonti must not be turned face down"
    );

    // The player-chosen dug card is the exiled, face-down object (CR 406.3).
    assert_eq!(
        state.objects[&dug_card].zone,
        Zone::Exile,
        "the chosen dug card must be in exile"
    );
    assert!(
        state.objects[&dug_card].face_down,
        "the exiled dug card must be face down (CR 406.3)"
    );

    // The other three looked-at cards were not exiled (they go to the bottom of
    // the library in a random order).
    for &id in &looked_at {
        if id == dug_card {
            continue;
        }
        assert_ne!(
            state.objects[&id].zone,
            Zone::Exile,
            "only the chosen card is exiled; the other looked-at cards are not"
        );
    }
    let _ = (lib1, lib2, lib3, lib4);
}

/// Casts Gonti targeting P1 and answers its ETB `DigChoice` with the first card; returns the
/// choosing player, the looked-at cards and the dug card, or `None` if no `DigChoice` surfaced.
fn cast_gonti_and_dig(
    runner: &mut GameRunner,
    gonti: ObjectId,
) -> Option<(PlayerId, Vec<ObjectId>, ObjectId)> {
    let card_id = runner.state().objects[&gonti].card_id;

    // Cast Gonti (free — auto-pays from an empty pool). Resolving it puts Gonti
    // onto the battlefield and fires its ETB trigger.
    runner
        .act(GameAction::CastSpell {
            object_id: gonti,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell accepted");

    // Drive the pipeline by hand: pass priority to resolve the spell + the ETB
    // trigger, accept the optional "you may play" rider, and answer the
    // DigChoice the fix introduces.
    let mut dig = None;

    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            // CR 115.1 + CR 603.3d: "target opponent's library" announces a
            // player target when the ETB trigger goes on the stack. That target
            // is what `Dig`'s library-owner filter resolves against at
            // resolution (`Typed{controller: Opponent}` is not a context ref, so
            // `resolve_player_for_context_ref` reads `ability.targets`).
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let choice = slot
                    .legal_targets
                    .iter()
                    .find(|t| **t == TargetRef::Player(P1))
                    .cloned();
                assert!(
                    choice.is_some(),
                    "the opponent P1 must be a legal target for Gonti's ETB; \
                     legal targets were {:?}",
                    slot.legal_targets
                );
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("ChooseTarget accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                // Accept the "you may look at and play that card" rider so the
                // resolution proceeds to the dig selection.
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("DecideOptionalEffect accepted");
            }
            WaitingFor::DigChoice { player, cards, .. } => {
                let chosen = cards[0];
                dig = Some((player, cards.clone(), chosen));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![chosen],
                    })
                    .expect("SelectCards (dig keep) accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && dig.is_some() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while driving Gonti ETB: {other:?}"),
        }
    }
    dig
}

const WORD_OF_SEIZING: &str = "Split second (As long as this spell is on the stack, players can't cast spells or activate abilities that aren't mana abilities.)\nUntap target permanent and gain control of it until end of turn. It gains haste until end of turn.";

/// CR 406.3 + CR 613.1b: the player who looked at Gonti's dug card keeps the look after a third
/// player gains control of Gonti; that player and the card's owner may not look.
#[test]
fn gonti_dug_card_look_stays_with_the_player_who_looked() {
    const P2: PlayerId = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        for i in 0..6 {
            scenario.add_card_to_library_top(player, &format!("Filler {i}"));
        }
    }
    let gonti = scenario
        .add_creature_to_hand(P0, "Gonti, Lord of Luxury", 2, 3)
        .from_oracle_text_with_keywords(&["Deathtouch"], GONTI_ORACLE)
        .as_legendary()
        .with_mana_cost(ManaCost::zero())
        .id();
    let seize = scenario
        .add_spell_to_hand(P2, "Word of Seizing", true)
        .from_oracle_text_with_keywords(&["Split second"], WORD_OF_SEIZING)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let (looker, _, dug) = cast_gonti_and_dig(&mut runner, gonti).expect("Gonti's ETB digs");
    let state = runner.state();
    assert_eq!(looker, P0);
    assert_eq!(state.objects[&dug].owner, P1);
    assert_eq!(state.objects[&dug].zone, Zone::Exile);
    assert!(state.objects[&dug].face_down);
    let name = state.objects[&dug].name.clone();

    for _ in 0..8 {
        if runner.state().priority_player == P2 {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority");
    }
    runner.cast(seize).target_objects(&[gonti]).resolve();
    let state = runner.state();
    let seen_as = |viewer: PlayerId| {
        filter_state_for_viewer(state, viewer).objects[&dug]
            .name
            .clone()
    };
    assert_eq!(state.objects[&gonti].controller, P2);
    assert_eq!(state.objects[&dug].zone, Zone::Exile);
    assert_eq!(seen_as(P2), "Hidden Card");
    assert_eq!(seen_as(P1), "Hidden Card");
    assert_eq!(seen_as(P0), name);
    assert!(spell_objects_available_to_cast(state, P0).contains(&dug));
}
