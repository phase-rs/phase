//! Issue #4235: Cloak and Dagger, Entwined must exile a chosen nonland card
//! from an opponent's hand UNTIL Cloak and Dagger leaves the battlefield, not
//! permanently.
//!
//! Oracle text (Marvel Spider-Man set MSH, read from `client/public/card-data.json`):
//!   "Deathtouch, lifelink
//!    When Cloak and Dagger enter, choose target opponent and up to one target
//!    creature they control. They reveal their hand. You may exile a nonland
//!    card from their hand or the chosen creature until Cloak and Dagger leave
//!    the battlefield."
//!
//! THE BUG this discriminates: `parse_until_body` (the "until X leaves the
//! battlefield" duration combinator in `parser/oracle_nom/duration.rs`) only
//! matched the singular verb form "leaves the battlefield". Cloak and Dagger's
//! own name is a plural subject ("Cloak and Dagger"), so its printed oracle
//! text uses plural agreement: "until Cloak and Dagger leave the battlefield".
//! That never matched, so the exile sub-ability's `duration` field silently
//! stayed `None` — no `ExileLinkKind::UntilSourceLeaves` link was ever
//! created, and a card exiled by this trigger stayed in exile forever, even
//! after Cloak and Dagger left the battlefield. This mirrors the *trigger*
//! detector for "leaves"/"leave the battlefield" (already handled correctly
//! in `oracle_trigger.rs` and `oracle_effect/mod.rs`), which just never had a
//! matching sibling in the *duration* combinator.
//!
//! Known, separate, OUT-OF-SCOPE gaps (not fixed here — see
//! investigated-issues.md #4235 for why):
//! 1. The "or the chosen creature" alternative exile target (choosing to
//!    exile the previously-targeted creature instead of a hand card) is not
//!    implemented by the parser at all, and neither is the "up to one target
//!    creature they control" secondary target declaration it depends on.
//! 2. The exile sub-ability's target filter (`Typed{Card, Non(Land)}`) is not
//!    scoped to the chosen opponent's hand specifically — `matches_target_filter`
//!    matches a nonland card in EITHER player's hand at the `scan_zone`
//!    fallback, because the filter carries no `controller`/`Owned` constraint
//!    tying "their hand" back to the opponent targeted earlier in the same
//!    clause. Distinct root cause from the duration bug this PR fixes, in the
//!    same family as gap 1 (anaphor binding across a RevealHand-chained
//!    sub-ability).
//! 3. Separately, `WaitingFor::EffectZoneChoice` (the interactive "choose
//!    which card" round-trip raised whenever `change_zone::resolve` finds
//!    MORE THAN ONE eligible candidate) carries no `duration` field at all, so
//!    `engine_resolution_choices.rs`'s resume handler hardcodes
//!    `ChangeZoneIterationCtx.duration: None` when replaying the player's
//!    pick — an "until leaves the battlefield" exile chosen interactively
//!    would silently lose its exile link even with this PR's parser fix
//!    applied. This is a general engine gap (affects any multi-candidate
//!    "exile ... until ... leaves" effect, not just this card) with a wide
//!    blast radius (~21 `EffectZoneChoice` construction sites), so it's out of
//!    scope for this tightly-scoped, card-specific fix.
//!
//! Because of gaps 2+3, this test deliberately keeps exactly ONE eligible
//! exile candidate in play (see the `destroy_spell` sequester/restore below)
//! so resolution takes `change_zone::resolve`'s single-eligible-candidate
//! shortcut, which reads `ability.duration` directly and is unaffected by gap
//! 3 — that's what actually isolates and discriminates this PR's parser fix
//! at the runtime level, without also requiring a fix for gaps 2/3.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Duration, Effect};
use engine::types::game_state::ExileLinkKind;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const CLOAK_AND_DAGGER: &str = "Deathtouch, lifelink\n\
When Cloak and Dagger enter, choose target opponent and up to one target creature they control. \
They reveal their hand. You may exile a nonland card from their hand or the chosen creature \
until Cloak and Dagger leave the battlefield.";

/// AST-shape regression: the plural "leave the battlefield" duration phrase
/// must resolve to `Duration::UntilHostLeavesPlay` on the exile sub-ability,
/// not silently drop to `None`.
#[test]
fn cloak_and_dagger_exile_sub_ability_has_until_leaves_duration() {
    let parsed = parse_oracle_text(
        CLOAK_AND_DAGGER,
        "Cloak and Dagger, Entwined",
        &[],
        &[],
        &[],
    );
    let etb = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone)
        .expect("ETB trigger");
    let execute = etb.execute.as_ref().expect("trigger.execute");

    // Walk the sub-ability chain (target opponent -> reveal hand -> exile) to
    // find the ChangeZone-to-Exile clause.
    let mut cursor = Some(execute.as_ref());
    let mut found_duration = None;
    while let Some(def) = cursor {
        if let Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        } = def.effect.as_ref()
        {
            found_duration = Some(def.duration.clone());
            break;
        }
        cursor = def.sub_ability.as_deref();
    }

    assert_eq!(
        found_duration,
        Some(Some(Duration::UntilHostLeavesPlay)),
        "expected the hand-exile sub-ability to carry Duration::UntilHostLeavesPlay \
         (plural 'leave the battlefield' must parse like the singular form)"
    );
}

/// Runtime regression: cast Cloak and Dagger, choose an opponent, reveal
/// their hand, exile the chosen nonland card, then destroy Cloak and Dagger
/// and verify the exiled card returns to the opponent's hand instead of
/// staying exiled forever.
#[test]
fn cloak_and_dagger_exiled_card_returns_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cloak_and_dagger = scenario
        .add_creature_to_hand_from_oracle(P0, "Cloak and Dagger, Entwined", 2, 2, CLOAK_AND_DAGGER)
        .id();

    // P1's hand: one nonland card (eligible for the "may exile" choice) and
    // one land (must stay excluded by the existing "nonland card" filter,
    // which this fix does not touch).
    let nonland_card = scenario.add_card_to_hand(P1, "Opponent's Spell");
    let land_card = scenario.add_land_to_hand(P1, "Opponent's Island").id();

    let destroy_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();

    let mut runner = scenario.build();

    // Sequester `destroy_spell` out of P0's hand for the exile step (see gap 2
    // in the module doc: the exile filter isn't scoped to the chosen
    // opponent's hand, so any nonland card in ANY hand — including the
    // caster's own — is currently offered as a candidate). Keeping exactly one
    // real candidate (`nonland_card`, in P1's hand) makes resolution take
    // `change_zone::resolve`'s single-eligible-candidate shortcut instead of
    // the interactive `EffectZoneChoice` round-trip (see gap 3), isolating
    // this PR's duration fix from both unrelated, out-of-scope gaps.
    {
        let state = runner.state_mut();
        state.objects.get_mut(&destroy_spell).unwrap().zone = Zone::Library;
        state.players[P0.0 as usize]
            .hand
            .retain(|&id| id != destroy_spell);
        state.players[P0.0 as usize]
            .library
            .push_back(destroy_spell);
    }

    // Cast Cloak and Dagger; the ETB trigger's "choose target opponent" is
    // satisfied by targeting P1. `accept_optional()` drives the CR 608.2d
    // "you may exile" decision to "yes", and with exactly one eligible
    // candidate the effect resolves the exile immediately (no further
    // interactive prompt) instead of auto-declining (the harness's default
    // policy for optional effects).
    runner
        .cast(cloak_and_dagger)
        .target_player(P1)
        .accept_optional()
        .resolve();

    assert_eq!(
        runner.state().objects[&nonland_card].zone,
        Zone::Exile,
        "chosen card must be exiled"
    );
    assert_eq!(
        runner.state().objects[&land_card].zone,
        Zone::Hand,
        "the land must NOT have been exiled (the 'nonland card' filter still works)"
    );
    assert!(
        runner.state().exile_links.iter().any(|link| {
            link.exiled_id == nonland_card
                && link.source_id == cloak_and_dagger
                && link.kind
                    == ExileLinkKind::UntilSourceLeaves {
                        return_zone: Zone::Hand,
                    }
        }),
        "expected an UntilSourceLeaves{{return_zone: Hand}} link for the exiled card, got {:?}",
        runner.state().exile_links
    );

    // Restore the sequestered destroy spell to P0's hand so it can be cast.
    {
        let state = runner.state_mut();
        state.objects.get_mut(&destroy_spell).unwrap().zone = Zone::Hand;
        state.players[P0.0 as usize]
            .library
            .retain(|&id| id != destroy_spell);
        state.players[P0.0 as usize].hand.push_back(destroy_spell);
    }

    // Cloak and Dagger leaves the battlefield -> the exiled card must return.
    runner
        .cast(destroy_spell)
        .target_object(cloak_and_dagger)
        .resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&cloak_and_dagger].zone,
        Zone::Graveyard,
        "Cloak and Dagger should be in the graveyard after being destroyed"
    );
    assert_eq!(
        runner.state().objects[&nonland_card].zone,
        Zone::Hand,
        "the exiled card must return to its owner's hand once Cloak and Dagger leaves \
         the battlefield, not stay exiled forever"
    );
    assert!(
        runner.state().players[P1.0 as usize]
            .hand
            .contains(&nonland_card),
        "returned card must actually be back in P1's hand zone list"
    );
    assert!(
        !runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == nonland_card),
        "the exile link must be cleared once the card has returned"
    );
}
