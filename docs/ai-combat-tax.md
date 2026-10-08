# AI and combat taxes

How the AI decides whether to pay a "creatures can't attack you unless their controller pays {N}" cost (Propaganda, Ghostly Prison, Sphere of Safety, Windborn Muse, Norn's Annex, Archangel of Tithes), and the block-side twin (CR 509.1c–d).

## The shape of the problem

`handle_declare_attackers` validates a declaration, then, if any attacker is taxed, pauses at `WaitingFor::CombatTaxPayment` **without committing**. Declining rebuilds the identical `DeclareAttackers` prompt: per CR 508.1d a player is never required to pay, and the whole proposal is discarded. Blocks behave the same way against `DeclareBlockers`.

That rebuild makes the AI's answer at the prompt a correctness concern rather than a tuning concern. A seat that declares a taxed attack and then declines the resulting quote re-enters the same declare prompt, re-proposes the same attack, and loops forever.

## The contract

The decision to pay is made exactly once, before the declaration is submitted:

| Seam | Where | Role |
|------|-------|------|
| `plan_attack_tax` | `crates/phase-ai/src/combat_tax.rs` | Pick the attack posture; trim the strike to one worth paying for and affordable. |
| `plan_block_tax` | `crates/phase-ai/src/combat_tax.rs` | Pick the block posture. |
| `pending_combat_tax_is_affordable` | `crates/engine/src/game/combat.rs` | Answer the live `CombatTaxPayment` prompt. |

A declaration this AI completes reaches a tax prompt only under `CombatTaxPosture::Accept`, and `Accept` is only honoured when the quote is affordable. So the prompt's answer is simply "pay if affordable": it is the payment the declaration was made for, and it is independent of deck features, policies, or determinized samples. Nothing can outvote it, so the round trip terminates.

`deterministic_combat_choice` answers the prompt inside `score_candidates_core`'s combat bypass, which always returns an action. `deterministic_choice`, which drives lookahead rollouts, gives the same engine-owned answer, so a rollout that plans a taxed attack commits it. The deadlock-safe `fallback_action` does the same.

## The engine side

`CombatTaxPosture` (`game/combat.rs`) is what a caller tells the completion authority:

- `Refuse`: any taxed proposal collapses to the deterministic tax-free witness (`best_free_declaration`). With no must-attack requirement on the board that witness is the **empty** declaration, so a refusing seat simply does not attack into a Propaganda.
- `Accept`: the taxed proposal survives, but only while `attack_tax_is_affordable` / `block_tax_is_affordable` says the paying player can cover the quote. Those probe through `casting::can_pay_effect_mana_cost_after_auto_tap`, the same payment path `handle_pay_combat_tax` spends through, with `PausedManaPayment::Unresumable`: that spend cannot suspend, so a mana source whose own cost would pause for a replacement choice counts as unaffordable. The preview and the spend therefore agree.

Hard legality (CR 508.1a–e) and the CR 508.1d maximum-requirement bar gate the proposal before the posture is consulted, and the engine remains the single legality authority. A posture is a request, not an override.

Engine candidate generation (`ai_support::candidates`) passes `Refuse`: its enumerated proposals carry no plan to pay, so they complete tax-free.

## Trimming

Most taxes in this class scale with the number of taxed creatures (Propaganda's "{2} for each creature"), and CR 508.1h totals those per-creature costs into one locked-in quote, so `plan_attack_tax` does not treat an unaffordable alpha strike as a reason to stay home. It drops the weakest taxed attacker and re-prices, until the remaining strike is both worth its cost and affordable. The cheap judgement runs before the affordability probe, which clones the state to simulate auto-tapping. An empty result hands the engine's tax-free witness the final say, which is also what honours any must-attack requirement the trimming walked past.

Block proposals are posture-only. They are not trimmed.

## Scoring

`is_worth_paying` compares paying against declining. Each planner supplies the damage at stake: for an attack, the taxed attackers' combined power; for a block, the damage the taxed blockers decide. An attacker whose every blocker is taxed gets through if they drop out, so its power is at stake, or for a trampler only the lethal damage its blockers were absorbing. An attacker that keeps an untaxed blocker stays blocked either way, so its taxed blockers are worth their own power toward killing it. Paying earns a damage bias when that damage exceeds the quote (scaled by deck archetype: aggro amplifies, control dampens, and blocking dampens control less so a control seat keeps its blockers), minus a penalty for tapping out of interaction (counting only sources the engine says can be activated now, so a summoning-sick mana creature does not count), plus bonuses when declining would collapse most of the declaration and for keeping blockers. Declining earns the opposite of the damage bias. The constants live at the top of `crates/phase-ai/src/combat_tax.rs`.

## Roadmap

- Block proposals get the same per-blocker trimming the attack side has.
- Tax cost is not yet an input to attacker *valuation*. The AI picks its strike first and prices it second, so it cannot trade a big attacker's tax against a cheaper line during selection.
