# probe-pin's own pin

Human intent lives here, above the marker: probe-pin's mount-and-read isolation is what makes
every `pass` row in the table below mean "visible-and-still-passing" rather than "unmeasured".
Regenerate with `cargo probe-pin run --write crates/probe-pin/tests/fixtures/dogfood.toml`.

// PROBE-PIN:BEGIN manifest=crates/probe-pin/tests/fixtures/dogfood.toml digest=sha256:e8abe1adf1a5fd5a
// instrument rustc = rustc 1.97.0-nightly (0febdbab2 2026-04-18)
// | probe | mutation | expect | verdict | firing assertion (anchor) | provenance |
// |---|---|---|---|---|---|
// | P0_control | (none) | pass | pass | (control; no mounts) | — |
// | P1_drop_second_site | prod.txt ×1 | fail | fail | CENSUS VIOLATED: expected 2 sites, got 1 / text: "SITE-ONE marker alpha\n" | crates/probe-pin/tests/pure_logic.rs |
// | P2_pad_reaches_target | prod.txt ×1 | fail | fail | CENSUS VIOLATED: expected 2 sites, got 202 | crates/probe-pin/tests/pure_logic.rs |
// probe-pin validates only the lines between BEGIN and END. Prose outside is never checked.
// PROBE-PIN:END

And here, below the marker, is prose probe-pin never reads, never validates, and never
invalidates. If you write interpretation, label it yourself.
