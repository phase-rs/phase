# Contributing to phase.rs

Thanks for helping build an open-source Magic: The Gathering rules engine. Two
documents are the real authority — read them before opening a PR:

- **[`CLAUDE.md`](CLAUDE.md)** — the design constitution: idiomatic Rust,
  composable building-block architecture, strict fidelity to the Comprehensive
  Rules. Every change is judged against these.
- **[`docs/AI-CONTRIBUTOR.md`](docs/AI-CONTRIBUTOR.md)** — step-by-step script
  (with copy-paste prompts) for implementing a card end-to-end with an LLM.

## The one hard rule: engine and parser fixes go through `/engine-implementer`

**All changes to `crates/engine/` — game logic, the Oracle parser, effects,
triggers, replacement effects, static abilities, targeting, the casting/stack
state machine — MUST be implemented through the
[`/engine-implementer`](.claude/skills/engine-implementer/SKILL.md) skill.** This
is not satisfied by reading the skill and editing by hand.

The skill orchestrates the full pipeline — plan → review-plan → implement →
review-impl → commit — each step in a fresh agent context. The review loops are
unbounded; "two rounds and ship" is not acceptable. This is how the repo keeps
ad-hoc edits from shipping plausible-but-wrong ASTs, special-cased logic that
breaks the next card, and unverified CR annotations.

**Narrow exceptions you may edit directly:** non-engine code (frontend,
transport layers, scripts, docs, CI) and truly mechanical engine edits with no
behavior or AST change. When in doubt, run the pipeline.

## Verification

```bash
./scripts/setup.sh  # one-time bootstrap
tilt up             # continuous build/test — leave running
```

**Tilt is the build system.** Do not run `cargo build`/`clippy`/`test` or
`pnpm type-check` directly — they fight Tilt for target locks. Read results with
`tilt logs <resource> --tail 50`. The one command always run directly is
`cargo fmt --all`. Full reference: the
[`project-reference`](.claude/skills/project-reference/SKILL.md) skill.

## Pull requests

- Target `origin/main` (`phase-rs/phase`).
- Don't modify `mtgish/`, `crates/mtgish-import/`, or `data/mtgish-*` (dormant);
  PRs that only touch them are rejected.
- If you used an LLM, report the model on a `Model:` line in the PR body per
  `docs/AI-CONTRIBUTOR.md`.

## License

Contributions are dual-licensed under [MIT](LICENSE-MIT) or
[Apache 2.0](LICENSE-APACHE), at the user's option — the same terms as the project.
