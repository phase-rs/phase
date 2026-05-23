import { LobbyDO } from "./lobby-do";

// The DO class must be exported from the Worker entry so the runtime can
// instantiate it for the binding declared in wrangler.toml.
export { LobbyDO };

interface Env {
  LOBBY: DurableObjectNamespace;
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    // Single global lobby: every request routes to the one DO instance named
    // "global". (Cloudflare multi-homes a single DO at the edge; there is no
    // second instance to fragment the pool — see plan §4/§5.)
    const id = env.LOBBY.idFromName("global");
    return env.LOBBY.get(id).fetch(request);
  },
};
