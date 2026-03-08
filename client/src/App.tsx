import { useEffect, useState } from "react";
import { WasmAdapter } from "./adapter";
import type { GameState } from "./adapter";

export function App() {
  const [status, setStatus] = useState<"loading" | "ready" | "error">(
    "loading",
  );
  const [pingResult, setPingResult] = useState<string>("");
  const [gameState, setGameState] = useState<GameState | null>(null);
  const [error, setError] = useState<string>("");

  useEffect(() => {
    const adapter = new WasmAdapter();

    async function initEngine() {
      try {
        await adapter.initialize();
        setPingResult(adapter.ping());
        const state = await adapter.getState();
        setGameState(state);
        setStatus("ready");
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
        setStatus("error");
      }
    }

    initEngine();
    return () => adapter.dispose();
  }, []);

  return (
    <div className="app">
      <h1>forge.ts</h1>
      <div className="engine-status">
        Engine: {status === "loading" && "Loading..."}
        {status === "ready" && "Ready"}
        {status === "error" && `Error - ${error}`}
      </div>
      {pingResult && (
        <div className="ping-result">
          Ping: <code>{pingResult}</code>
        </div>
      )}
      {gameState && (
        <div className="game-state">
          <h2>Initial Game State</h2>
          <p>Turn: {gameState.turn_number}</p>
          <p>Phase: {gameState.phase}</p>
          <p>Players: {gameState.players.length}</p>
          {gameState.players.map((player) => (
            <div key={player.id} className="player">
              Player {player.id}: {player.life} life
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
