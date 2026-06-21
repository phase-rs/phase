interface CubeCobraCard {
  name?: unknown;
  details?: {
    name?: unknown;
    oracle_id?: unknown;
  };
}

interface CubeCobraCards {
  mainboard?: unknown;
}

interface CubeCobraExport {
  cards?: CubeCobraCards;
}

const CUBECOBRA_HOSTS = new Set(["cubecobra.com", "www.cubecobra.com"]);

function cubeCobraApiUrl(input: string): string | null {
  let url: URL;
  try {
    url = new URL(input);
  } catch {
    return null;
  }

  if (!CUBECOBRA_HOSTS.has(url.hostname)) return null;

  const parts = url.pathname.split("/").filter(Boolean);
  if (parts[0] !== "cube") return null;
  if (parts[1] === "api" && parts[2] === "cubeJSON" && parts[3]) {
    return url.toString();
  }
  if (parts[1] === "list" && parts[2]) {
    return `${url.origin}/cube/api/cubeJSON/${parts[2]}`;
  }

  return null;
}

function cubeCobraJsonToCountedList(data: CubeCobraExport): string {
  const cards = data.cards?.mainboard;
  if (!Array.isArray(cards)) {
    throw new Error("CubeCobra response did not include a mainboard");
  }

  // Carry each card's Scryfall oracle id as a `[oracle-id]` annotation. The
  // engine resolves by name first, falling back to the id when CubeCobra's
  // cached name no longer matches the printed name (e.g. a cube snapshotted
  // under a set's pre-reveal placeholder names).
  const entries = new Map<string, { count: number; oracleId?: string }>();
  for (const card of cards as CubeCobraCard[]) {
    const rawName = typeof card.name === "string" ? card.name : card.details?.name;
    if (typeof rawName !== "string" || rawName.trim().length === 0) {
      throw new Error("CubeCobra response included a card without a name");
    }
    const name = rawName.trim();
    const oracleId = typeof card.details?.oracle_id === "string" ? card.details.oracle_id : undefined;
    const existing = entries.get(name);
    if (existing) {
      existing.count += 1;
    } else {
      entries.set(name, { count: 1, oracleId });
    }
  }

  return [...entries.entries()]
    .map(([name, { count, oracleId }]) =>
      oracleId ? `${count} ${name} [${oracleId}]` : `${count} ${name}`,
    )
    .join("\n");
}

export async function fetchCubeList(url: string): Promise<string> {
  const trimmed = url.trim();
  const apiUrl = cubeCobraApiUrl(trimmed);
  const resp = await fetch(apiUrl ?? trimmed);
  if (!resp.ok) throw new Error(`Fetch failed: ${resp.status}`);

  if (apiUrl) {
    return cubeCobraJsonToCountedList(await resp.json() as CubeCobraExport);
  }

  return resp.text();
}
