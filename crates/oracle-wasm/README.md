# Browser Oracle checker

A standalone, synchronous `checkOracleCard(json)` export intended for a disposable Web Worker.
It loads no card catalogue or game session. Input and output are JSON strings; acceptance means
compiler/parser support, not official Oracle validity, semantic equivalence, or playability.

Build from the repository root (Rust's wasm32-unknown-unknown target and wasm-bindgen-cli 0.2.127):

```sh
scripts/build-oracle-wasm.sh /tmp/oracle-checker
```

The size-focused release uses LTO, one codegen unit and stripped symbols. Optional `wasm-opt -Oz`
shrinks the result further. The output is self-contained JavaScript plus WASM; consumers need no
checkout, game engine initialization, corpus, network service, or inference model.

```js
import init, { checkOracleCard } from './checker.js';
await init({ module_or_path: await fetch('./checker_bg.wasm') });
const input = {name:'Probe', oracle_text:'Draw a card.', types:['Sorcery'], subtypes:[]};
const result = JSON.parse(checkOracleCard(JSON.stringify(input)));
```

Keep unsupported/partial results, warnings and runtime failures visible separately from acceptance.
Workers can be terminated on a host timeout; do not execute arbitrary checks on the UI thread.
Multi-face relationship validation is outside this single-face API. Do not flatten a whole card and
present a face-only check as acceptance of its entire layout.

This deliberately checks parser compatibility, not installed runtime handlers (`scope: "parser"`).
The handler registries and full parse-tree JSON serializer are excluded from the boundary to keep
the artifact smaller. An observed library-top CastFromZone fallback is reported as `needs_review`,
not clean acceptance.

This is an isolated workspace, like broker-wasm. Its binding/toolchain dependencies never change
the engine/client workspace lockfile or the CLI versions used by existing deployment jobs.
