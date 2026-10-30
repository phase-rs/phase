/**
 * The main-thread engine instance, owned by `WasmAdapter`'s fallback alone.
 *
 * It exists for the one case where the engine worker cannot start. Anything
 * else that loads `@wasm/engine` on the main thread puts a second engine in the
 * tab beside the worker's, which is what pushed iOS Safari past its memory
 * ceiling. `boundary-guardrails.test.ts` enforces that this module and the
 * worker are the only runtime importers.
 */
import type { EngineModule } from "./engine-queries";
import { EngineModuleReloadRequiredError } from "./types";

let engineModulePromise: Promise<EngineModule> | null = null;
let wasmInitPromise: Promise<void> | null = null;
let cardDbPromise: Promise<number> | null = null;
type ComboTableLoadOutcome = "loaded" | "unavailable";
let comboTablePromise: Promise<ComboTableLoadOutcome> | null = null;

export async function loadEngineModule(): Promise<EngineModule> {
  if (!engineModulePromise) {
    engineModulePromise = import("@wasm/engine").catch((cause: unknown) => {
      throw new EngineModuleReloadRequiredError(cause);
    });
  }
  return engineModulePromise;
}

export async function ensureWasmInit(): Promise<void> {
  if (!wasmInitPromise) {
    const pending = (async () => {
      const engine = await loadEngineModule();
      if (__ENGINE_WASM_URL__) {
        await engine.default({ module_or_path: __ENGINE_WASM_URL__ });
      } else {
        await engine.default();
      }
    })();
    wasmInitPromise = pending;
    void pending.catch((error: unknown) => {
      if (!(error instanceof EngineModuleReloadRequiredError) && wasmInitPromise === pending) {
        wasmInitPromise = null;
      }
    });
  }
  return wasmInitPromise;
}

export async function ensureCardDatabase(): Promise<number> {
  if (!cardDbPromise) {
    const pending = (async () => {
      await ensureWasmInit();
      const engine = await loadEngineModule();
      const resp = await fetch(__CARD_DATA_URL__);
      if (!resp.ok) {
        throw new Error(`Failed to load card-data.json (${resp.status})`);
      }
      const text = await resp.text();
      const loaded = await engine.load_card_database(text);
      if (loaded <= 0) {
        throw new Error("Failed to load card-data.json (no cards loaded)");
      }
      return loaded;
    })();
    cardDbPromise = pending;
    void pending.catch((error: unknown) => {
      if (!(error instanceof EngineModuleReloadRequiredError) && cardDbPromise === pending) {
        cardDbPromise = null;
      }
    });
  }
  return cardDbPromise;
}

/**
 * Lazily loads the optional combo artifact once per browser session. Missing,
 * unreachable, or malformed data resolves as unavailable so bracket estimates
 * retain the engine's explicit `Unmeasured` coverage instead of failing.
 */
export function ensureComboTable(): Promise<ComboTableLoadOutcome> {
  if (!comboTablePromise) {
    let definitiveMissing = false;
    const pending: Promise<ComboTableLoadOutcome> = (async () => {
      try {
        await ensureWasmInit();
        const engine = await loadEngineModule();
        const response = await fetch(__COMBO_TABLE_URL__);
        if (response.status === 404) {
          definitiveMissing = true;
          return "unavailable";
        }
        if (!response.ok) {
          throw new Error(`Failed to load combo-table.json (${response.status})`);
        }
        await engine.load_combo_table(await response.text());
        return "loaded";
      } catch {
        return "unavailable";
      }
    })();
    comboTablePromise = pending;
    void pending.then((outcome) => {
      if (outcome === "unavailable" && !definitiveMissing && comboTablePromise === pending) {
        comboTablePromise = null;
      }
    });
  }
  return comboTablePromise;
}
