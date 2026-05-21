interface CubeCobraCard {
  name?: unknown;
  details?: {
    name?: unknown;
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

  const counts = new Map<string, number>();
  for (const card of cards as CubeCobraCard[]) {
    const rawName = typeof card.name === "string" ? card.name : card.details?.name;
    if (typeof rawName !== "string" || rawName.trim().length === 0) {
      throw new Error("CubeCobra response included a card without a name");
    }
    const name = rawName.trim();
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }

  return [...counts.entries()]
    .map(([name, count]) => `${count} ${name}`)
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
