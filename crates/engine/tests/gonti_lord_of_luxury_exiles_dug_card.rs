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
//! Note on scope: the body parser lowers "an opponent's library" to the
//! controller's-library `Dig` (`parse_dig_library_owner` returns
//! `TargetFilter::Controller`), so this test exercises the dig against the
//! controller's own library. The "an opponent's library" → opponent-targeting
//! is a separate latent gap, out of scope for #1146 (which concerns WHICH card
//! is exiled, not WHOSE library is read).
//!
//! CR 701.20e: looking at cards is private. CR 406.3 / CR 708.2: a card exiled
//! face down has no characteristics and can't be examined. CR 702.75a: Hideaway
//! is the structural analog this lowering mirrors.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const GONTI_ORACLE: &str = "Deathtouch\n\
When Gonti, Lord of Luxury enters the battlefield, look at the top four cards of an opponent's library, exile one of them face down, then you may look at and play that card for as long as it remains exiled, and you may spend mana as though it were mana of any color to cast that spell.";

#[test]
fn gonti_exiles_the_dug_card_not_himself() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Stack the dug library so the top four are the looked-at cards and a fifth
    // deeper card must NOT be seen (proves the dig is bounded to 4).
    // `add_card_to_library_top` inserts at the top (index 0), so add the deepest
    // card first and the top-of-four last.
    let lib_deep = scenario.add_card_to_library_top(P0, "Lib Deep Card");
    let lib4 = scenario.add_card_to_library_top(P0, "Lib Card 4");
    let lib3 = scenario.add_card_to_library_top(P0, "Lib Card 3");
    let lib2 = scenario.add_card_to_library_top(P0, "Lib Card 2");
    let lib1 = scenario.add_card_to_library_top(P0, "Lib Card 1");

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
    let mut saw_dig_choice = false;
    let mut dug_card: Option<ObjectId> = None;
    let mut looked_at: Vec<ObjectId> = Vec::new();

    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                // Accept the "you may look at and play that card" rider so the
                // resolution proceeds to the dig selection.
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("DecideOptionalEffect accepted");
            }
            WaitingFor::DigChoice { cards, .. } => {
                saw_dig_choice = true;
                looked_at = cards.clone();
                let chosen = cards[0];
                dug_card = Some(chosen);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![chosen],
                    })
                    .expect("SelectCards (dig keep) accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && saw_dig_choice {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while driving Gonti ETB: {other:?}"),
        }
    }

    // DISCRIMINATOR (#1146), part 1: the keep_count:1 fix surfaces a DigChoice.
    // Pre-fix the keep_count:0 pure-peek short-circuited and this never fired.
    assert!(
        saw_dig_choice,
        "Gonti's ETB must surface a DigChoice so the controller selects the card to exile; \
         pre-fix the keep_count:0 peek short-circuited and no choice was offered"
    );

    // The dig looked at exactly the top four cards (CR 701.20e) — not the deeper
    // card — proving the keep_count:1 dig is bounded to the four looked-at cards.
    assert_eq!(looked_at.len(), 4, "Gonti looks at the top FOUR cards");
    assert!(
        !looked_at.contains(&lib_deep),
        "the deeper (5th) card must not be looked at"
    );

    let dug_card = dug_card.expect("a card was dug");
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
