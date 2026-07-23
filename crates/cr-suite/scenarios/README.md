# Scenario fixtures

Committed fixtures in this directory are the **executable seed set** only
(discriminating contracts for a handful of CR rules).

Do **not** commit the full CompRules skeleton corpus here. Generate it with:

```bash
cargo cr-suite --generate --update
```

Land generated skeletons in a separate, mechanically reproducible PR.
