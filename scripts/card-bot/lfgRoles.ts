import type { LfgFormat } from "./formats";

const ROLES_URL = "https://discord.com/api/v10/guilds";
const CACHE_MS = 60_000;
const REQUEST_TIMEOUT_MS = 1_500;

export function lfgRoleName(format: LfgFormat): string {
  return `LFG ${format.label}`;
}

/** Resolves the server's opt-in `LFG <format>` roles without keeping role IDs in source. */
export class LfgRoleCache {
  private ids = new Map<string, string>();
  private loadedAt = 0;
  private loading: Promise<void> | null = null;

  constructor(
    private readonly guildId: string,
    private readonly botToken: string,
    private readonly fetchFn: (url: string, init?: RequestInit) => Promise<Response> = fetch,
    private readonly now: () => number = Date.now,
  ) {}

  async resolve(guildId: string, format: LfgFormat): Promise<string | undefined> {
    if (guildId !== this.guildId) return undefined;
    if (this.now() - this.loadedAt >= CACHE_MS) {
      this.loading ??= this.refresh().finally(() => {
        this.loading = null;
      });
      await this.loading;
    }
    return this.ids.get(lfgRoleName(format));
  }

  private async refresh(): Promise<void> {
    try {
      const response = await this.fetchFn(`${ROLES_URL}/${this.guildId}/roles`, {
        headers: { Authorization: `Bot ${this.botToken}` },
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      });
      if (!response.ok) throw new Error(`GET guild roles → ${response.status}`);
      const body: unknown = await response.json();
      if (!Array.isArray(body)) throw new Error("GET guild roles: unexpected body");
      const ids = new Map<string, string>();
      for (const role of body) {
        if (role !== null && typeof role.name === "string" && typeof role.id === "string") {
          ids.set(role.name, role.id);
        }
      }
      this.ids = ids;
      this.loadedAt = this.now();
    } catch (err) {
      console.error("[lfg] role lookup failed:", err);
    }
  }
}
