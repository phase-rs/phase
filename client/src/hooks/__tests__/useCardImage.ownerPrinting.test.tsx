import { act, cleanup, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { CardPrinting, PrintedRef } from "../../adapter/types.ts";
import type * as Platform from "../../services/platform.ts";
import type { PrintingEntry } from "../../services/scryfall.ts";
import type { VisualPackBackend } from "../../services/visualPacks/backend.ts";
import type { ResolutionKey, ResolutionResponse, ResolvedAsset, RevisionEvent } from "../../services/visualPacks/types.ts";
import { assetKey, catalogRoot, decodeCandidateKey, installedRevision, packId } from "../../services/visualPacks/types.ts";
import { encodeCandidateKey } from "../../services/visualPacks/candidateKeys.ts";

const A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const B = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const O1 = "11111111-1111-4111-8111-111111111111";
const O2 = "22222222-2222-4222-8222-222222222222";
const P = "33333333-3333-4333-8333-333333333333";
const C = "44444444-4444-4444-8444-444444444444";
const S = "55555555-5555-4555-8555-555555555555";
const FOREIGN = "66666666-6666-4666-8666-666666666666";
const owner: CardPrinting = { set_code: "OWN", collector_number: "01" };
const secondOwner: CardPrinting = { set_code: "own", collector_number: "2" };
const foreignOwner: CardPrinting = { set_code: "bad", collector_number: "9" };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function json(data: unknown) {
  return new Response(JSON.stringify(data), { headers: { "Content-Type": "application/json" } });
}

function printing(id: string, set: string, collector_number: string): PrintingEntry {
  return {
    id, set, collector_number, set_name: set, released_at: "2026-01-01", border_color: "black",
    frame_effects: [], full_art: false,
    faces: [
      { normal: `https://img.example/${id}-front.jpg`, art_crop: `https://img.example/${id}-front-art.jpg` },
      { normal: `https://img.example/${id}-back.jpg`, art_crop: `https://img.example/${id}-back-art.jpg` },
    ],
  };
}

function catalogs() {
  const entry = {
    oracle_id: A, name: "Alpha // Reverse", face_names: ["alpha", "reverse"], layout: "transform",
    mana_cost: "{1}", cmc: 1, type_line: "Creature", colors: [], color_identity: [], keywords: [],
    faces: [
      { normal: "/default-front.jpg", orientation: "landscape" as const },
      { normal: "/default-back.jpg", orientation: "portrait" as const },
    ],
  };
  const other = { ...entry, oracle_id: B, name: "Beta", face_names: ["beta"] };
  return {
    main: { [A]: entry, alpha: entry, reverse: entry, [B]: other, beta: other },
    catalog: {
      [A]: [printing(O1, "own", "01"), printing(O2, "own", "2"), printing(P, "pin", "3"),
        printing(C, "chn", "4"), printing(S, "src", "5"), printing(FOREIGN, "plst", "9")],
      [B]: [printing(FOREIGN, "bad", "9")],
    },
  };
}

interface SetupOptions {
  holdCatalog?: boolean;
  offline?: boolean;
  realPlatform?: boolean;
  canonicalMain?: () => Response | Promise<Response>;
  privateMain?: () => Response | Promise<Response>;
}

// Each fixture intentionally reloads module boundaries: canonical promises, private
// resources, repositories and normal caches must start independently in paired runs.
async function setup(options: SetupOptions = {}) {
  const data = catalogs();
  const gate = deferred<Response>();
  const installed = new Map<string, ResolvedAsset[]>();
  const revisions = new Set<(event: RevisionEvent) => void>();
  let revision = installedRevision("0");
  let beforeResolve: ((keys: ResolutionKey[]) => Promise<void>) | undefined;
  const backendResolve = vi.fn(async (keys: ResolutionKey[]): Promise<ResolutionResponse> => {
    await beforeResolve?.(keys);
    return {
      revision,
      entries: keys.map((key, ordinal) => ({ ordinal, key, matches: installed.get(key.key) ?? [] })),
    };
  });
  const backend = {
    resolve: backendResolve,
    subscribeRevision: vi.fn(async (listener: (event: RevisionEvent) => void) => {
      revisions.add(listener);
      return () => revisions.delete(listener);
    }),
  } as unknown as VisualPackBackend;
  const factory = vi.fn(async () => backend);
  if (options.realPlatform) {
    vi.doMock("../../services/visualPacks/backendFactory.ts", () => ({ createVisualPackBackend: factory }));
  } else {
    vi.doMock("../../services/platform.ts", async (original) => ({
      ...await original<typeof Platform>(),
      loadVisualPackBackend: vi.fn(async () => backend),
    }));
  }
  const platform = await import("../../services/platform.ts");
  const loader = vi.spyOn(platform, "loadVisualPackBackend");
  const fetcher = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) => {
    const url = new URL(String(input), "http://localhost/");
    const privateRequest = url.searchParams.has("phase-owner-art");
    if (url.pathname === "/scryfall-data.json") {
      return await (privateRequest ? options.privateMain?.() : options.canonicalMain?.()) ?? json(data.main);
    }
    if (url.pathname === "/scryfall-printings.json") {
      return options.holdCatalog ? gate.promise : json(data.catalog);
    }
    if (url.pathname.includes("scryfall-images.v2.")) {
      return json({ [O1]: { id: O2, faces: [
        { small: "/fr-small.jpg", normal: "/fr-owner.jpg", art_crop: "/fr-art.jpg" },
        { small: "/fr-back-small.jpg", normal: "/fr-back.jpg", art_crop: "/fr-back-art.jpg" },
      ] } });
    }
    throw new Error(`Unexpected request ${String(input)}`);
  });
  vi.stubGlobal("fetch", fetcher);
  const preferences = (await import("../../stores/preferencesStore.ts")).usePreferencesStore;
  preferences.setState({ language: "en", showOwnerPrintings: true,
    artOverrides: { [A]: { scryfallId: P, setCode: "pin", collectorNumber: "3" } }, artChain: [] });
  const connectivity = (await import("../../stores/connectivityStore.ts")).useConnectivityStore;
  connectivity.setState({ forcedOffline: options.offline ?? false, browserOnline: true });
  const repositoryModule = await import("../../services/visualPacks/repository.ts");
  const resolve = vi.spyOn(repositoryModule.VisualPackRepository.prototype, "resolve");
  const subscribe = vi.spyOn(repositoryModule.VisualPackRepository.prototype, "subscribe");
  const revisionReads = vi.spyOn(repositoryModule.VisualPackRepository.prototype, "currentRevision");
  const normalRepository = repositoryModule.visualPackRepository;
  const service = await import("../../services/scryfall.ts");
  const hook = await import("../useCardImage.ts");
  function install(key: string, url: string, suffix = "QQ", pack = "complete") {
    const match: ResolvedAsset = {
      packId: packId(pack), assetKey: assetKey(`asset:v1:exact_printing:${suffix}`),
      catalogRoot: catalogRoot("a".repeat(64)), url, media: "image/jpeg",
    };
    installed.set(key, [...installed.get(key) ?? [], match]);
  }
  function installPrinting(id: string, url: string, face = 0, variant: "full_card" | "art_crop" = "full_card", suffix = "QQ") {
    for (const rung of variant === "art_crop" ? ["art_crop" as const] : ["small" as const, "normal" as const]) {
      install(encodeCandidateKey("english_printing", [id, face, variant, rung]), url, suffix);
    }
  }
  function ownerCalls() {
    return resolve.mock.calls.flatMap(([request], index) => resolve.mock.contexts[index] === normalRepository ? [] : [request]);
  }
  function normalCalls() {
    return resolve.mock.calls.flatMap(([request], index) => resolve.mock.contexts[index] === normalRepository ? [request] : []);
  }
  return {
    ...hook, service, data, gate, installed, install, installPrinting, fetcher, preferences, connectivity,
    normalRepository, resolve, subscribe, revisionReads, backendResolve, backend, factory, loader, ownerCalls, normalCalls,
    blockBackend(block: (keys: ResolutionKey[]) => Promise<void>) { beforeResolve = block; },
    bumpRevision() {
      revision = installedRevision(String(Number(revision) + 1));
      for (const listener of revisions) listener({ cause: "install", revision, operationId: null, catalogRoot: null });
    },
  };
}

async function settle() {
  await act(async () => { for (let index = 0; index < 20; index += 1) await Promise.resolve(); });
}

function resetFixture() {
  cleanup();
  vi.resetModules();
  vi.restoreAllMocks();
  vi.doUnmock("../../services/platform.ts");
  vi.doUnmock("../../services/visualPacks/backendFactory.ts");
  vi.doUnmock("../../services/scryfall.ts");
  vi.doUnmock("../../services/visualPacks/repository.ts");
  localStorage.clear();
}
beforeEach(resetFixture);
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("object-owned Oracle admission", () => {
  it("withdraws and admits a homonymous printing on identity-only A to B to A transitions", async () => {
    const env = await setup();
    Object.assign(env.data.main, {
      twin: { ...env.data.main[A], name: "Twin", face_names: ["twin"] },
      [A]: { ...env.data.main[A], name: "Twin", face_names: ["twin"] },
      [B]: { ...env.data.main[B], name: "Twin", face_names: ["twin"] },
    });
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(FOREIGN, "/twin-b.jpg");
    const hook = renderHook(({ oracleId }) => env.useCardImage("Twin", {
      ownerPrinting: foreignOwner, ownerPrintedRef: { oracle_id: oracleId, face_name: "Twin" },
    }), { initialProps: { oracleId: A } });
    const reader = env.service.createOwnerArtCatalogReader();
    await act(async () => { await reader.prepare({ cardName: "Twin", faceIndex: 0, offline: false }); });
    reader.release();
    await waitFor(() => expect(hook.result.current.src).toBe("/viewer.jpg"));
    await settle();
    hook.rerender({ oracleId: B });
    await waitFor(() => expect(hook.result.current.src).toBe("/twin-b.jpg"));
    hook.rerender({ oracleId: A });
    await waitFor(() => expect(hook.result.current.src).toBe("/viewer.jpg"));
  });

  it("requires carrier identity even when the display name resolves to an admitting catalog", async () => {
    const env = await setup();
    Object.assign(env.data.main, {
      twin: { ...env.data.main[A], name: "Twin", face_names: ["twin"] },
      [A]: { ...env.data.main[A], name: "Twin", face_names: ["twin"] },
      [B]: { ...env.data.main[B], name: "Twin", face_names: ["twin"] },
    });
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O1, "/twin-a.jpg");
    env.installPrinting(FOREIGN, "/twin-b.jpg");
    const hook = renderHook(({ pair, ref }) => env.useCardImage("Twin", {
      ownerPrinting: pair, ownerPrintedRef: ref,
    }), { initialProps: { pair: foreignOwner,
      ref: { oracle_id: B, face_name: "Twin" } as PrintedRef | undefined } });
    await waitFor(() => expect(hook.result.current.src).toBe("/twin-b.jpg"));
    hook.rerender({ pair: owner, ref: { oracle_id: B, face_name: "Twin" } });
    await settle();
    await waitFor(() => expect(hook.result.current.src).toBe("/viewer.jpg"));
    hook.rerender({ pair: owner, ref: undefined });
    await settle();
    expect(hook.result.current.src).toBe("/viewer.jpg");
    hook.rerender({ pair: owner, ref: { oracle_id: A, face_name: "Twin" } });
    await waitFor(() => expect(hook.result.current.src).toBe("/twin-a.jpg"));
  });

  it("does not create an owner reader without identity, while oracleId alone admits", async () => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O1, "/owner.jpg");
    const reader = vi.spyOn(env.service, "createOwnerArtCatalogReader");
    const hook = renderHook(({ oracleId }: { oracleId?: string }) => env.useCardImage("Alpha", {
      ownerPrinting: owner, oracleId,
    }), { initialProps: {} });
    await waitFor(() => expect(hook.result.current.src).toBe("/viewer.jpg"));
    await settle();
    expect(reader).not.toHaveBeenCalled();
    expect(env.fetcher.mock.calls.filter(([url]) => String(url).includes("phase-owner-art"))).toEqual([]);
    hook.rerender({ oracleId: A });
    await waitFor(() => expect(hook.result.current.src).toBe("/owner.jpg"));
    expect(reader).toHaveBeenCalled();
  });

  it("overrides conflicting legacy identity only for owner admission and strips owner-only options", async () => {
    const env = await setup();
    const legacy = await import("../useLegacyCardImage.ts");
    const calls = vi.spyOn(legacy, "useCardImage");
    env.installPrinting(O1, "/owner-back.jpg", 1);
    const ref = { oracle_id: A, face_name: "Reverse" };
    const ordinary = { oracleId: B, faceName: "Beta", faceIndex: 0, size: "normal" as const };
    const hook = renderHook(({ onlyRef }) => env.useCardImage("Beta", onlyRef
      ? { ownerPrintedRef: ref }
      : { ...ordinary, ownerPrinting: owner, ownerPrintedRef: ref }), { initialProps: { onlyRef: false } });
    await waitFor(() => expect(hook.result.current.src).toBe("/owner-back.jpg"));
    expect(calls).toHaveBeenLastCalledWith("Beta", ordinary);
    act(() => env.preferences.setState({ showOwnerPrintings: false }));
    await waitFor(() => expect(hook.result.current.src).toBe("/default-front.jpg"));
    expect(calls).toHaveBeenLastCalledWith("Beta", ordinary);
    hook.rerender({ onlyRef: true });
    await waitFor(() => expect(hook.result.current.src).toBe("/default-front.jpg"));
    expect(calls).toHaveBeenLastCalledWith("Beta", {});
  });

  it("uses an untransformed MDFC's printed back face without altering toggle-off art", async () => {
    const env = await setup();
    env.data.main[A].layout = "modal_dfc";
    env.data.main.alpha.layout = "modal_dfc";
    env.installPrinting(O1, "/owner-back.jpg", 1);
    env.installPrinting(P, "/viewer-front.jpg");
    const { CardImage } = await import("../../components/card/CardImage.tsx");
    const object = { name: "Alpha", transformed: false, printed_ref: { oracle_id: A, face_name: "Reverse" } };
    const surface = render(<CardImage cardName={object.name} faceIndex={object.transformed ? 1 : 0}
      ownerPrinting={owner} ownerPrintedRef={object.printed_ref} />);
    await waitFor(() => expect(surface.container.querySelector("img")).toHaveAttribute("src", "/owner-back.jpg"));
    act(() => env.preferences.setState({ showOwnerPrintings: false }));
    await waitFor(() => expect(surface.container.querySelector("img")).toHaveAttribute("src", "/viewer-front.jpg"));
  });

  it("conceals owner identity and art when a populated CardImage turns face down", async () => {
    const env = await setup();
    env.installPrinting(O1, "/owner-front.jpg");
    const { CardImage } = await import("../../components/card/CardImage.tsx");
    const props = { cardName: "Alpha", ownerPrinting: owner, ownerPrintedRef: { oracle_id: A, face_name: "Alpha" } };
    const surface = render(<CardImage {...props} />);
    await waitFor(() => expect(surface.container.querySelector("img")).toHaveAttribute("src", "/owner-front.jpg"));
    surface.rerender(<CardImage {...props} faceDown />);
    await settle();
    expect(surface.container.querySelector('img[src="/owner-front.jpg"]')).toBeNull();
    expect(screen.getByRole("img", { name: "Card back" })).toBeInTheDocument();
    surface.rerender(<CardImage {...props} />);
    await waitFor(() => expect(surface.container.querySelector("img")).toHaveAttribute("src", "/owner-front.jpg"));
  });
});

describe("owner printing non-gating overlay", () => {
  it.each(["normal", "art_crop"] as const)("shows normal then exact installed owner for %s with authoritative back face", async (size) => {
    const env = await setup({ holdCatalog: true });
    env.installPrinting(P, "/viewer-back.jpg", 1, size === "art_crop" ? "art_crop" : "full_card");
    env.installPrinting(O1, "/owner-back.jpg", 1, size === "art_crop" ? "art_crop" : "full_card");
    await env.service.loadScryfallData();
    const { result } = renderHook(() => env.useCardImage("Alpha", {
      oracleId: A, ownerPrinting: owner, size, faceIndex: 0, faceName: "Reverse",
    }));
    await waitFor(() => expect(result.current.src).toBe("/viewer-back.jpg"));
    expect(result.current).toMatchObject({ isLoading: false, isRotated: false, isFlip: false });
    expect(env.ownerCalls()).toEqual([]);
    await act(async () => { env.gate.resolve(json(env.data.catalog)); });
    await waitFor(() => expect(result.current.src).toBe("/owner-back.jpg"));
    expect(result.current).toMatchObject({ isLoading: false, isRotated: false, isFlip: false, source: { kind: "installed" } });
  });

  it("restricts both repository stages despite installed Oracle, alias, source, chain and viewer art", async () => {
    const env = await setup({ holdCatalog: true });
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(C, "/chain.jpg");
    env.installPrinting(S, "/source.jpg");
    env.install(encodeCandidateKey("oracle", [A, 0, "full_card", "normal"]), "/canonical.jpg");
    env.install(encodeCandidateKey("english_alias", ["alpha", 0, "full_card", "normal"]), "/alias.jpg");
    const options = { oracleId: A, ownerPrinting: owner, scryfallId: P, sourcePrinting: { setCode: "src", collectorNumber: "5" } };
    const { result } = renderHook(() => env.useCardImage("Alpha", options));
    await waitFor(() => expect(result.current.src).toBe("/viewer.jpg"));
    await act(async () => { env.gate.resolve(json(env.data.catalog)); });
    await waitFor(() => expect(result.current.src).toBe("https://img.example/11111111-1111-4111-8111-111111111111-front.jpg"));
    expect(env.ownerCalls().map((request) => request.allowRemote)).toEqual([false, true]);
    expect(result.current).toMatchObject({ source: { kind: "remote" }, isRotated: true, isLoading: false });
  });

  it("rejects foreign and excluded pairs, but reconsiders the same pair under its current Oracle", async () => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(FOREIGN, "/foreign.jpg");
    const hook = renderHook(({ oracleId, pair }) => env.useCardImage("Beta", { oracleId, ownerPrinting: pair }), {
      initialProps: { oracleId: A, pair: foreignOwner },
    });
    await waitFor(() => expect(hook.result.current.src).toBe("/viewer.jpg"));
    await settle();
    expect(env.ownerCalls()).toEqual([]);
    hook.rerender({ oracleId: B, pair: foreignOwner });
    await waitFor(() => expect(hook.result.current.src).toBe("/foreign.jpg"));
    hook.rerender({ oracleId: A, pair: { set_code: "plst", collector_number: "9" } });
    expect(hook.result.current.src).not.toBe("/foreign.jpg");
    await settle();
    expect(hook.result.current.src).toBe("/viewer.jpg");
  });

  it.each([undefined, null, { set_code: "", collector_number: "01" }, { set_code: "own", collector_number: "" }, { set_code: 3, collector_number: "01" }])(
    "does not prepare or access the owner repository for malformed/absent %j", async (pair) => {
      const env = await setup({ offline: true });
      env.installPrinting(P, "/viewer.jpg");
      const { result } = renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: pair as CardPrinting }));
      await waitFor(() => expect(result.current.src).toBe("/viewer.jpg"));
      expect(env.fetcher).not.toHaveBeenCalled();
      expect(env.ownerCalls()).toEqual([]);
      expect(env.revisionReads.mock.contexts.every((receiver) => receiver === env.normalRepository)).toBe(true);
    },
  );

  it("switches live, preserves scalar-equivalent inputs, and returns a warm owner immediately", async () => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O1, "/owner-one.jpg");
    env.installPrinting(O2, "/owner-two.jpg");
    const mount = () => renderHook(({ pair }) => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: pair }), {
      initialProps: { pair: { ...owner } },
    });
    const hook = mount();
    await waitFor(() => expect(hook.result.current.src).toBe("/owner-one.jpg"));
    const count = env.ownerCalls().length;
    hook.rerender({ pair: { ...owner } });
    act(() => env.preferences.setState({ artChain: [{ type: "set", setCode: "chn", label: "Chain" }] }));
    await settle();
    expect(env.ownerCalls()).toHaveLength(count);
    act(() => env.preferences.setState({ showOwnerPrintings: false }));
    expect(hook.result.current.src).toBe("/viewer.jpg");
    act(() => env.preferences.setState({ showOwnerPrintings: true }));
    expect(hook.result.current.src).toBe("/owner-one.jpg");
    hook.unmount();
    const warm = mount();
    expect(warm.result.current.src).toBe("/owner-one.jpg");
    warm.rerender({ pair: secondOwner });
    expect(warm.result.current.src).toBe("/viewer.jpg");
    await waitFor(() => expect(warm.result.current.src).toBe("/owner-two.jpg"));
  });

  it("honors explicit toggles over both store values", async () => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O1, "/owner.jpg");
    act(() => env.preferences.setState({ showOwnerPrintings: false }));
    const hook = renderHook(({ show }) => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner, showOwnerPrintings: show }), {
      initialProps: { show: true },
    });
    await waitFor(() => expect(hook.result.current.src).toBe("/owner.jpg"));
    hook.rerender({ show: false });
    act(() => env.preferences.setState({ showOwnerPrintings: true }));
    expect(hook.result.current.src).toBe("/viewer.jpg");
  });

  it("does not coerce collector numbers and accepts uppercase sets and token art crops", async () => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O1, "/owner-art.jpg", 0, "art_crop");
    const hook = renderHook(({ pair, isToken, size }: { pair: CardPrinting; isToken: boolean; size: "normal" | "art_crop" }) =>
      env.useCardImage("Alpha", { oracleId: A, ownerPrinting: pair, isToken, size }), {
      initialProps: { pair: { ...owner, collector_number: "1" }, isToken: false, size: "normal" },
    });
    await waitFor(() => expect(hook.result.current.src).toBe("/viewer.jpg"));
    await settle();
    expect(env.ownerCalls()).toEqual([]);
    act(() => env.connectivity.setState({ forcedOffline: true }));
    hook.rerender({ pair: owner, isToken: true, size: "art_crop" });
    await waitFor(() => expect(hook.result.current.src).toBe("/owner-art.jpg"));
  });

  it("keeps name-only owner art disabled after canonical readiness until identity arrives", async () => {
    const env = await setup({ offline: true });
    env.preferences.setState({ artOverrides: {} });
    env.install(encodeCandidateKey("name_face", ["alpha", "alpha", "full_card", "normal"]), "/name.jpg", "TkFNRQ", "deck_library");
    env.installPrinting(O1, "/owner.jpg", 1);
    const hook = renderHook(({ oracleId }: { oracleId?: string }) =>
      env.useCardImage("Alpha", { ownerPrinting: owner, oracleId, faceName: "Reverse" }), { initialProps: {} });
    // Its ordinary face-name candidate is Reverse, while name identity remains cold.
    env.install(encodeCandidateKey("name_face", ["alpha", "reverse", "full_card", "normal"]), "/name.jpg", "TkFNRQ", "deck_library");
    await waitFor(() => expect(hook.result.current.src).toBe("/name.jpg"));
    await env.service.loadPrintingsData();
    await settle();
    expect(env.ownerCalls()).toEqual([]);
    await act(async () => { await env.service.loadScryfallData(); });
    await settle();
    expect(hook.result.current.src).toBe("/name.jpg");
    expect(env.ownerCalls()).toEqual([]);
    hook.rerender({ oracleId: A });
    await waitFor(() => expect(hook.result.current.src).toBe("/owner.jpg"));
    expect(hook.result.current.isRotated).toBe(false);
  });

  it("localizes the same admitted printing and preserves English installed art for unsupported locales", async () => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    // Exact locale projection uses a sized Scryfall URL, not an unrelated localized printing.
    env.data.catalog[A][0].faces[0].normal = `https://cards.scryfall.io/normal/front/1/1/${O1}.jpg`;
    const hook = renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner }));
    await waitFor(() => expect(hook.result.current.src).toBe("https://cards.scryfall.io/normal/front/1/1/11111111-1111-4111-8111-111111111111.jpg"));
    act(() => env.preferences.setState({ language: "fr" }));
    await waitFor(() => expect(hook.result.current.src).toBe("/fr-owner.jpg"));
    env.installPrinting(O1, "/owner-english.jpg");
    act(() => env.preferences.setState({ language: "zh" }));
    await waitFor(() => expect(hook.result.current.src).toBe("/owner-english.jpg"));
  });

  it("exhausts only the owner ladder, rejects stale/duplicate errors, and retains exhaustion on remount", async () => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O1, "/owner-a.jpg", 0, "full_card", "QQ");
    env.installPrinting(O1, "/owner-b.jpg", 0, "full_card", "Qg");
    const mount = () => renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner }));
    const hook = mount();
    await waitFor(() => expect(hook.result.current.src).toBe("/owner-a.jpg"));
    const normalCount = env.normalCalls().length;
    act(() => hook.result.current.advanceFailedSource?.("/owner-a.jpg"));
    expect(hook.result.current.src).toBe("/owner-b.jpg");
    act(() => hook.result.current.advanceFailedSource?.("/owner-a.jpg"));
    expect(hook.result.current.src).toBe("/owner-b.jpg");
    act(() => hook.result.current.advanceFailedSource?.("/owner-b.jpg"));
    await waitFor(() => expect(hook.result.current.source?.kind).toBe("remote"));
    act(() => hook.result.current.advanceFailedSource?.("https://img.example/11111111-1111-4111-8111-111111111111-front.jpg"));
    expect(hook.result.current).toMatchObject({ src: null, isLoading: false, source: { kind: "fallback" } });
    expect(env.normalCalls()).toHaveLength(normalCount);
    hook.unmount();
    const remount = mount();
    expect(remount.result.current).toMatchObject({ src: null, isLoading: false, source: { kind: "fallback" } });
    act(() => env.preferences.setState({ showOwnerPrintings: false }));
    expect(remount.result.current.src).toBe("/viewer.jpg");
  });

  it.each([false, true])("discards stale repository completions before publication, cache, and remote continuation; unmount=%s", async (unmount) => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O2, "/owner-two.jpg");
    const entered = deferred<void>();
    const release = deferred<void>();
    env.blockBackend(async (keys) => {
      if (keys.some((key) => key.kind === "candidate" && decodeCandidateKey(key.key)[1][0] === O1)) {
        entered.resolve();
        await release.promise;
      }
    });
    const mount = (pair: CardPrinting) => renderHook(({ pair }) =>
      env.useCardImage("Alpha", { oracleId: A, ownerPrinting: pair }), { initialProps: { pair } });
    let hook = mount(owner);
    await entered.promise;
    if (unmount) {
      hook.unmount();
      hook = mount(secondOwner);
    } else {
      hook.rerender({ pair: secondOwner });
    }
    await waitFor(() => expect(hook.result.current.src).toBe("/owner-two.jpg"));
    const calls = env.ownerCalls().length;
    await act(async () => { release.resolve(); });
    await settle();
    expect(hook.result.current.src).toBe("/owner-two.jpg");
    expect(env.ownerCalls()).toHaveLength(calls);
    hook.unmount();
    const remount = renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: secondOwner }));
    expect(remount.result.current.src).toBe("/owner-two.jpg");
  });

  it("invalidates warm owner art on revision and never treats unavailable catalog as admission", async () => {
    const env = await setup({ holdCatalog: true, offline: true });
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O1, "/owner.jpg");
    const hook = renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner }));
    await waitFor(() => expect(hook.result.current.src).toBe("/viewer.jpg"));
    expect(env.ownerCalls()).toEqual([]);
    expect(env.fetcher).not.toHaveBeenCalled();
    await act(async () => {
      env.gate.resolve(json(env.data.catalog));
      await env.service.loadPrintingsData();
    });
    await waitFor(() => expect(hook.result.current.src).toBe("/owner.jpg"));
    env.installed.clear();
    act(() => env.bumpRevision());
    await waitFor(() => expect(hook.result.current).toMatchObject({ src: null, isLoading: false, source: { kind: "fallback" } }));
  });

  it.each([false, true])("DOM smoke: normal then owner, off/on and error, remote=%s", async (remote) => {
    const env = await setup({ holdCatalog: true });
    env.installPrinting(P, "/viewer.jpg");
    if (!remote) env.installPrinting(O1, "/owner.jpg");
    function Surface() {
      const image = env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner });
      return <img alt="selected art" src={image.src ?? undefined} data-loading={String(image.isLoading)}
        data-source={image.source?.kind} data-rotated={String(image.isRotated)}
        onError={(event) => image.advanceFailedSource?.(event.currentTarget.getAttribute("src") ?? "")} />;
    }
    render(<Surface />);
    const image = screen.getByRole("img", { name: "selected art" });
    await waitFor(() => expect(image).toHaveAttribute("src", "/viewer.jpg"));
    expect(image).toHaveAttribute("data-loading", "false");
    await act(async () => { env.gate.resolve(json(env.data.catalog)); });
    const expected = remote ? "https://img.example/11111111-1111-4111-8111-111111111111-front.jpg" : "/owner.jpg";
    await waitFor(() => expect(image).toHaveAttribute("src", expected));
    act(() => env.preferences.setState({ showOwnerPrintings: false }));
    expect(image).toHaveAttribute("src", "/viewer.jpg");
    act(() => env.preferences.setState({ showOwnerPrintings: true }));
    expect(image).toHaveAttribute("src", expected);
    fireEvent.error(image);
    await waitFor(() => expect(image).not.toHaveAttribute("src", expected));
  });
});

describe("isolated owner catalog authority", () => {
  it("coalesces cold private resources without installing canonical state or writing persistent caches", async () => {
    const env = await setup();
    const cache = { match: vi.fn(async () => undefined), open: vi.fn(), put: vi.fn(), delete: vi.fn() };
    vi.stubGlobal("caches", cache);
    const reader = env.service.createOwnerArtCatalogReader();
    const input = { cardName: "Alpha", faceIndex: 0, offline: false };
    await Promise.all(Array.from({ length: 16 }, () => reader.prepare(input)));
    expect(reader.read(input).oracleId).toBe(A);
    expect(reader.read(input).printings?.map((entry) => entry.id)).toContain(O1);
    expect(env.service.resolveOracleIdSync("Alpha")).toBeNull();
    expect(env.service.isCardDataResident()).toBe(false);
    expect(env.fetcher.mock.calls.map(([url, init]) => [new URL(String(url)).pathname, init?.cache])).toEqual([
      ["/scryfall-data.json", "no-store"], ["/scryfall-printings.json", "no-store"],
    ]);
    expect(env.fetcher.mock.calls.every(([url]) => new URL(String(url)).searchParams.get("phase-owner-art") === "1")).toBe(true);
    expect(cache.open).not.toHaveBeenCalled();
    expect(cache.put).not.toHaveBeenCalled();
    expect(cache.delete).not.toHaveBeenCalled();
    await reader.prepare(input);
    expect(env.fetcher).toHaveBeenCalledTimes(2);
    await env.service.loadScryfallData();
    await env.service.loadPrintingsData();
    expect(env.service.resolveOracleIdSync("Alpha")).toBe(A);
    expect(env.service.isCardDataResident()).toBe(true);
    await reader.prepare(input);
    expect(env.fetcher).toHaveBeenCalledTimes(4);
    reader.release();
  });

  it("admits cache-only offline owner data only with carrier identity and without network", async () => {
    const env = await setup({ offline: true });
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O1, "/owner.jpg");
    const cache = { match: vi.fn(async (url: string) => json(url.includes("printings") ? env.data.catalog : env.data.main)) };
    vi.stubGlobal("caches", cache);
    const hook = renderHook(({ ref }: { ref?: PrintedRef }) =>
      env.useCardImage("Alpha", { ownerPrinting: owner, ownerPrintedRef: ref }), { initialProps: {} });
    await settle();
    expect(env.ownerCalls()).toEqual([]);
    expect(cache.match).not.toHaveBeenCalled();
    hook.rerender({ ref: { oracle_id: A, face_name: "Alpha" } });
    await waitFor(() => expect(hook.result.current.src).toBe("/owner.jpg"));
    expect(env.fetcher).not.toHaveBeenCalled();
    expect(cache.match.mock.calls.map(([url]) => url)).toEqual(["/scryfall-data.json", "/scryfall-printings.json"]);
  });

  it.each(["main", "catalog", "locale"] as const)("isolates throwing and rejecting readiness observers from canonical %s success", async (resource) => {
    async function observe(withThrowers: boolean) {
      resetFixture();
      const env = await setup();
      const reader = env.service.createOwnerArtCatalogReader();
      const throwing = vi.fn(() => { throw new Error("observer"); });
      const rejecting = vi.fn(async () => { throw new Error("async observer"); });
      const healthy = vi.fn();
      const remove = [
        ...(withThrowers ? [reader.subscribe(throwing), reader.subscribe(rejecting)] : []),
        reader.subscribe(healthy),
      ];
      const load = () => resource === "main" ? env.service.loadScryfallData()
        : resource === "catalog" ? env.service.loadPrintingsData() : env.service.loadLocaleArt("fr");
      const result = await load();
      if (withThrowers) {
        expect(throwing).toHaveBeenCalled();
        expect(rejecting).toHaveBeenCalled();
        expect(healthy).toHaveBeenCalledTimes(throwing.mock.calls.length);
      }
      const fetchCount = env.fetcher.mock.calls.length;
      expect(await load()).toBe(result);
      expect(env.fetcher).toHaveBeenCalledTimes(fetchCount);
      for (const unsubscribe of remove) unsubscribe();
      if (resource === "main") expect(env.service.resolveOracleIdSync("Alpha")).toBe(A);
      if (resource === "catalog") expect(await env.service.getCardPrintings(A)).toContainEqual(env.data.catalog[A][0]);
      if (resource === "locale") expect(result instanceof Map && result.get(O1)?.faces[0].normal).toBe("/fr-owner.jpg");
      return { result: result instanceof Map ? [...result] : result, fetchCount, readiness: healthy.mock.calls.length };
    }
    expect(await observe(true)).toEqual(await observe(false));
  });

  it("does not consume a failed canonical attempt and retries a private failure only on a new attempt", async () => {
    let canonicalAttempts = 0;
    let privateAttempts = 0;
    const env = await setup({
      canonicalMain: () => ++canonicalAttempts === 1 ? new Response(null, { status: 503 }) : json(catalogs().main),
      privateMain: () => ++privateAttempts === 1 ? new Response(null, { status: 503 }) : json(catalogs().main),
    });
    const reader = env.service.createOwnerArtCatalogReader();
    const input = { cardName: "Alpha", offline: false, faceIndex: 0 };
    await reader.prepare(input);
    expect(reader.read(input).oracleId).toBeNull();
    expect(await env.service.loadScryfallData()).toBeNull();
    await reader.prepare(input);
    expect(reader.read(input).oracleId).toBe(A);
    expect(env.service.resolveOracleIdSync("Alpha")).toBeNull();
    expect(await env.service.loadScryfallData()).not.toBeNull();
    expect(canonicalAttempts).toBe(2);
    expect(privateAttempts).toBe(2);
    reader.release();
  });
});

describe("normal history and admission boundary", () => {
  it.each(["online", "offline", "token", "retry", "exhausted"] as const)(
    "preserves the normal %s history through held and rejected owner preparation", async (mode) => {
      async function history(withOwner: boolean) {
        resetFixture();
        let attempts = 0;
        const env = await setup({
          holdCatalog: true, offline: mode === "offline",
          canonicalMain: () => mode === "retry" && ++attempts === 1
            ? new Response(null, { status: 503 }) : json(catalogs().main),
        });
        const cachedRead = vi.fn(async (url: string) =>
          url.includes("printings") ? env.gate.promise : json(env.data.main));
        if (mode === "offline") vi.stubGlobal("caches", { match: cachedRead });
        env.preferences.setState({ artOverrides: {} });
        env.install(encodeCandidateKey("name_face", ["alpha", "alpha", "full_card", "normal"]), "/name.jpg", "TkFNRQ", "deck_library");
        env.install(encodeCandidateKey("oracle_face", [A, "alpha", "full_card", "normal"]), "/oracle.jpg", "T1JBQ0xF", "deck_library");
        if (mode === "token") env.installPrinting(P, "/token.jpg");
        const publications: unknown[] = [];
        let previous = "";
        const hook = renderHook(() => {
          const value = env.useCardImage("Alpha", {
            ownerPrinting: withOwner ? foreignOwner : undefined,
            ownerPrintedRef: withOwner ? { oracle_id: A, face_name: "Alpha" } : undefined,
            ...(mode === "token" ? { isToken: true, tokenImageRef: { preset_id: "", scryfall_id: P, scryfall_oracle_id: A, face_name: "Alpha" } } : {}),
          });
          const entry = [value.src, value.isLoading, value.source?.kind, value.isRotated, value.isFlip];
          const key = JSON.stringify(entry);
          if (key !== previous) { previous = key; publications.push(entry); }
          return value;
        });
        await waitFor(() => expect(hook.result.current.src).toBe(mode === "token" ? "/token.jpg"
          : mode === "offline" || mode === "retry" ? "/name.jpg" : "/oracle.jpg"));
        await settle();
        if (mode === "retry") {
          await act(async () => { await env.service.loadScryfallData(); });
          // Give both histories the same ordinary parent opportunity to see current globals.
          hook.rerender();
          await waitFor(() => expect(hook.result.current.src).toBe("/oracle.jpg"));
        }
        if (mode === "exhausted") {
          act(() => hook.result.current.advanceFailedSource?.("/oracle.jpg"));
          await waitFor(() => expect(hook.result.current.src).toBe("/default-front.jpg"));
          act(() => hook.result.current.advanceFailedSource?.("/default-front.jpg"));
          expect(hook.result.current.src).toBeNull();
        }
        if (withOwner) {
          await waitFor(() => expect(mode === "offline"
            ? cachedRead.mock.calls.some(([url]) => url.includes("printings"))
            : env.fetcher.mock.calls.some(([url]) => String(url).includes("scryfall-printings.json")))
            .toBe(true));
          expect(env.ownerCalls()).toEqual([]);
        }
        const before = {
          requests: structuredClone(env.normalCalls()),
          publications: [...publications],
          canonicalFetches: env.fetcher.mock.calls.filter(([url]) => !String(url).includes("phase-owner-art")).map(([url]) => String(url)),
        };
        await act(async () => { env.gate.resolve(json(env.data.catalog)); });
        await settle();
        const after = {
          requests: structuredClone(env.normalCalls()),
          publications: [...publications],
          canonicalFetches: env.fetcher.mock.calls.filter(([url]) => !String(url).includes("phase-owner-art")).map(([url]) => String(url)),
        };
        expect(after).toEqual(before);
        expect(env.ownerCalls()).toEqual([]);
        hook.unmount();
        return after;
      }
      const control = await history(false);
      expect(await history(true)).toEqual(control);
    },
  );

  it("matches an owner preference render to a parent render after canonical identity becomes resident", async () => {
    async function history(withOwner: boolean) {
      resetFixture();
      const env = await setup({ offline: true });
      env.preferences.setState({ artOverrides: {} });
      const cachedRead = vi.fn(async (url: string) =>
        url.includes("printings") ? env.gate.promise : json(env.data.main));
      vi.stubGlobal("caches", { match: cachedRead });
      const publications: unknown[] = [];
      env.install(encodeCandidateKey("name_face", ["alpha", "alpha", "full_card", "normal"]), "/name.jpg", "TkFNRQ", "deck_library");
      env.install(encodeCandidateKey("oracle_face", [A, "alpha", "full_card", "normal"]), "/oracle.jpg", "T1JBQ0xF", "deck_library");
      const hook = renderHook(() => {
        const result = env.useCardImage("Alpha", {
          ownerPrinting: withOwner ? foreignOwner : undefined,
          ownerPrintedRef: withOwner ? { oracle_id: A, face_name: "Alpha" } : undefined,
        });
        const presentation = [result.src, result.isLoading, result.source?.kind];
        if (JSON.stringify(publications[publications.length - 1]) !== JSON.stringify(presentation)) publications.push(presentation);
        return result;
      });
      await waitFor(() => expect(hook.result.current.src).toBe("/name.jpg"));
      if (withOwner) {
        await waitFor(() => expect(cachedRead.mock.calls.some(([url]) => url.includes("printings"))).toBe(true));
        expect(env.ownerCalls()).toEqual([]);
      }
      await act(async () => { await env.service.loadScryfallData(); await env.service.loadPrintingsData(); });
      expect(env.service.resolveOracleIdSync("Alpha")).toBe(A);
      expect(hook.result.current.src).toBe("/name.jpg");
      await act(async () => { env.gate.resolve(json(env.data.catalog)); });
      await settle();
      expect(hook.result.current.src).toBe("/name.jpg");
      if (withOwner) act(() => env.preferences.setState({ showOwnerPrintings: false }));
      else hook.rerender();
      await waitFor(() => expect(hook.result.current.src).toBe("/oracle.jpg"));
      expect(env.ownerCalls()).toEqual([]);
      const requests = structuredClone(env.normalCalls());
      hook.unmount();
      return { requests, publications };
    }
    expect(await history(true)).toEqual(await history(false));
  });

  it("retains normal warm LRU behavior under pending/rejected owner pressure", async () => {
    const env = await setup({ holdCatalog: true, offline: true });
    env.installPrinting(P, "/viewer.jpg");
    async function warm(name: string) {
      const hook = renderHook(() => env.useCardImage(name, { oracleId: A, ownerPrinting: foreignOwner }));
      await settle();
      expect(hook.result.current.src).toBe("/viewer.jpg");
      hook.unmount();
    }
    await warm("Identity 0");
    for (let index = 1; index < 256; index += 1) await warm(`Identity ${index}`);
    const refresh = renderHook(() => env.useCardImage("Identity 0", { oracleId: A, ownerPrinting: foreignOwner }));
    expect(refresh.result.current.src).toBe("/viewer.jpg");
    refresh.unmount();
    await warm("Identity 256");
    const retained = renderHook(() => env.useCardImage("Identity 0", { oracleId: A, ownerPrinting: foreignOwner }));
    expect(retained.result.current.src).toBe("/viewer.jpg");
    retained.unmount();
    const evicted = renderHook(() => env.useCardImage("Identity 1", { oracleId: A, ownerPrinting: foreignOwner }));
    expect(evicted.result.current).toMatchObject({ src: "/viewer.jpg", isLoading: true, source: null });
    await settle();
    expect(evicted.result.current.src).toBe("/viewer.jpg");
    await act(async () => { env.gate.resolve(json(env.data.catalog)); await env.service.loadPrintingsData(); });
    expect(evicted.result.current.src).toBe("/viewer.jpg");
    expect(env.ownerCalls()).toEqual([]);
  });

  it.each(["pending", "rejected", "off", "absent", "malformed"] as const)(
    "makes no owner repository/backend calls for %s through the real platform initializer", async (mode) => {
      async function history(withOwner: boolean) {
        resetFixture();
        const env = await setup({ realPlatform: true, holdCatalog: true });
        env.preferences.setState({ artOverrides: {} });
        env.factory.mockRejectedValue(new Error("initialization failed"));
        const pair = mode === "absent" ? undefined : mode === "malformed"
          ? { set_code: "", collector_number: "01" } : mode === "rejected" ? foreignOwner : owner;
        const hook = renderHook(() => env.useCardImage("Alpha", {
          oracleId: A, ownerPrinting: withOwner ? pair : undefined,
          ...(mode === "off" ? { showOwnerPrintings: false } : {}),
        }));
        await waitFor(() => expect(hook.result.current.isLoading).toBe(false));
        await settle();
        expect(env.factory).toHaveBeenCalled();
        const before = [env.loader.mock.calls.length, env.factory.mock.calls.length, env.backendResolve.mock.calls.length];
        if (mode === "rejected") {
          await act(async () => { env.gate.resolve(json(env.data.catalog)); });
          await settle();
        }
        expect([env.loader.mock.calls.length, env.factory.mock.calls.length, env.backendResolve.mock.calls.length]).toEqual(before);
        expect(env.ownerCalls()).toEqual([]);
        expect(env.subscribe.mock.contexts.every((receiver) => receiver === env.normalRepository)).toBe(true);
        expect(env.revisionReads.mock.contexts.every((receiver) => receiver === env.normalRepository)).toBe(true);
        const result = { calls: before, requests: structuredClone(env.normalCalls()), src: hook.result.current.src };
        hook.unmount();
        return result;
      }
      const control = await history(false);
      expect(await history(true)).toEqual(control);
    },
  );

  it("retries the real shared initializer only after positive owner admission", async () => {
    const env = await setup({ realPlatform: true, holdCatalog: true });
    env.factory.mockRejectedValue(new Error("initialization failed"));
    env.installPrinting(O1, "/owner.jpg");
    const hook = renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner }));
    await waitFor(() => expect(hook.result.current.isLoading).toBe(false));
    await settle();
    const attempts = env.factory.mock.calls.length;
    expect(attempts).toBeGreaterThan(0);
    expect(env.ownerCalls()).toEqual([]);
    env.factory.mockResolvedValue(env.backend);
    await act(async () => { env.gate.resolve(json(env.data.catalog)); });
    await waitFor(() => expect(hook.result.current.src).toBe("/owner.jpg"));
    expect(env.factory.mock.calls.length).toBeGreaterThan(attempts);
    expect(env.ownerCalls().some((request) => !request.allowRemote)).toBe(true);
    expect(env.backendResolve).toHaveBeenCalled();
  });

  it("does not publish late catalog/identity work after toggle or unmount", async () => {
    const main = deferred<Response>();
    const env = await setup({ holdCatalog: true, privateMain: () => main.promise });
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O1, "/owner.jpg");
    const hook = renderHook(() => env.useCardImage("Alpha", { ownerPrinting: owner, oracleId: A }));
    await waitFor(() => expect(hook.result.current.src).toBe("/viewer.jpg"));
    act(() => env.preferences.setState({ showOwnerPrintings: false }));
    await act(async () => { env.gate.resolve(json(env.data.catalog)); main.resolve(json(env.data.main)); });
    await settle();
    expect(hook.result.current.src).toBe("/viewer.jpg");
    expect(env.ownerCalls()).toEqual([]);
    hook.unmount();
    const next = renderHook(() => env.useCardImage("Alpha", { ownerPrinting: owner, oracleId: A, showOwnerPrintings: true }));
    await waitFor(() => expect(next.result.current.src).toBe("/owner.jpg"));
    next.unmount();
  });
});

describe("owner authority invalidation", () => {
  it("terminates missing admitted art instead of retaining an unrelated installed printing", async () => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    env.data.catalog[A][0].faces = [{ normal: "https://errors.scryfall.com/soon.jpg" }];
    env.installPrinting(O2, "/second-owner.jpg");
    const hook = renderHook(({ pair }) => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: pair }), { initialProps: { pair: owner } });
    await waitFor(() => expect(hook.result.current).toMatchObject({ src: null, isLoading: false, source: { kind: "fallback" } }));
    expect(env.ownerCalls().some((request) => request.allowRemote && request.remote === null)).toBe(true);
    hook.rerender({ pair: secondOwner });
    await waitFor(() => expect(hook.result.current.src).toBe("/second-owner.jpg"));
  });

  it("does not route an old owner's callback into a new owner with the same URL", async () => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    env.installPrinting(O1, "/shared-owner.jpg");
    env.installPrinting(O2, "/shared-owner.jpg");
    const hook = renderHook(({ pair }) => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: pair }), { initialProps: { pair: owner } });
    await waitFor(() => expect(hook.result.current.src).toBe("/shared-owner.jpg"));
    const oldFailure = hook.result.current.advanceFailedSource;
    hook.rerender({ pair: secondOwner });
    await settle();
    expect(hook.result.current.src).toBe("/shared-owner.jpg");
    act(() => oldFailure?.("/shared-owner.jpg"));
    expect(hook.result.current.src).toBe("/shared-owner.jpg");
    act(() => hook.result.current.advanceFailedSource?.("/shared-owner.jpg"));
    await waitFor(() => expect(hook.result.current.src).toBe("https://img.example/22222222-2222-4222-8222-222222222222-front.jpg"));
  });

  it("keeps carrier identity authoritative across canonical name changes and withdraws when identity is removed", async () => {
    const env = await setup({ offline: true, canonicalMain: () => {
      const data = catalogs().main;
      return json({ ...data, alpha: data[B] });
    } });
    env.preferences.setState({ artOverrides: {} });
    env.install(encodeCandidateKey("name_face", ["alpha", "alpha", "full_card", "normal"]), "/normal.jpg", "TkFNRQ", "deck_library");
    env.installPrinting(O1, "/owner.jpg");
    vi.stubGlobal("caches", { match: vi.fn(async (url: string) => json(url.includes("printings") ? env.data.catalog : env.data.main)) });
    const hook = renderHook(({ identified }) => env.useCardImage("Alpha", {
      ownerPrinting: owner,
      ownerPrintedRef: identified ? { oracle_id: A, face_name: "Alpha" } : undefined,
    }), { initialProps: { identified: true } });
    await waitFor(() => expect(hook.result.current.src).toBe("/owner.jpg"));
    await act(async () => { await env.service.loadScryfallData(); });
    expect(hook.result.current.src).toBe("/owner.jpg");
    hook.rerender({ identified: false });
    await settle();
    expect(hook.result.current.src).toBe("/normal.jpg");
  });

  it("rekeys front/back and online/offline presentations without retaining remote owner art offline", async () => {
    const env = await setup();
    env.installPrinting(P, "/viewer.jpg");
    const hook = renderHook(({ faceName }) => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner, faceName }), {
      initialProps: { faceName: "Alpha" },
    });
    await waitFor(() => expect(hook.result.current.src).toBe("https://img.example/11111111-1111-4111-8111-111111111111-front.jpg"));
    expect(hook.result.current.isRotated).toBe(true);
    hook.rerender({ faceName: "Reverse" });
    expect(hook.result.current.src).not.toBe("https://img.example/11111111-1111-4111-8111-111111111111-front.jpg");
    await waitFor(() => expect(hook.result.current.src).toBe("https://img.example/11111111-1111-4111-8111-111111111111-back.jpg"));
    expect(hook.result.current.isRotated).toBe(false);
    const requests = env.fetcher.mock.calls.length;
    act(() => env.connectivity.setState({ forcedOffline: true }));
    await waitFor(() => expect(hook.result.current).toMatchObject({ src: null, isLoading: false, source: { kind: "fallback" } }));
    expect(env.fetcher).toHaveBeenCalledTimes(requests);
  });
});

it("refines installed owner face/layout after optional main metadata arrives", async () => {
  const main = deferred<unknown>();
  const env = await setup({
    canonicalMain: async () => json(await main.promise),
    privateMain: async () => json(await main.promise),
  });
  env.installPrinting(P, "/viewer-front.jpg");
  env.installPrinting(P, "/viewer-back.jpg", 1);
  env.installPrinting(O1, "/owner-front.jpg");
  env.installPrinting(O1, "/owner-back.jpg", 1);
  const hook = renderHook(() => env.useCardImage("Alpha", {
    oracleId: A, ownerPrinting: owner, faceName: "Reverse", faceIndex: 0,
  }));
  await waitFor(() => expect(hook.result.current.src).toBe("/owner-front.jpg"));
  expect(hook.result.current.isFlip).toBe(false);
  env.data.main[A].layout = "flip";
  await act(async () => { main.resolve(env.data.main); });
  await waitFor(() => expect(hook.result.current.src).toBe("/owner-back.jpg"));
  expect(hook.result.current).toMatchObject({ isFlip: true, isRotated: false, isLoading: false });
});


async function privateOwnerFixture(options: SetupOptions = {}) {
  const env = await setup({ ...options, canonicalMain: () => new Response(null, { status: 503 }) });
  env.preferences.setState({ artOverrides: {}, artChain: [] });
  env.install(encodeCandidateKey("oracle_face", [A, "alpha", "full_card", "normal"]), "/normal.jpg", "Tk9STUFM", "deck_library");
  env.installPrinting(O1, "/owner.jpg");
  return env;
}

describe("owner acquisition policy transitions", () => {
  it("escalates a joined offline cache miss once after reconnect without another render trigger", async () => {
    const env = await privateOwnerFixture({ offline: true });
    const main = deferred<Response | undefined>();
    const catalog = deferred<Response | undefined>();
    const cache = vi.fn((url: string) => {
      if (cache.mock.calls.length <= 2) return url.includes("printings") ? catalog.promise : main.promise;
      return Promise.resolve(undefined);
    });
    vi.stubGlobal("caches", { match: cache });
    const hooks = Array.from({ length: 4 }, () => renderHook(() =>
      env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner })));
    await waitFor(() => expect(hooks.every((hook) => hook.result.current.src === "/normal.jpg")).toBe(true));
    expect(cache).toHaveBeenCalledTimes(2);
    act(() => env.connectivity.setState({ forcedOffline: false }));
    await act(async () => { main.resolve(undefined); catalog.resolve(undefined); });
    await waitFor(() => expect(hooks.every((hook) => hook.result.current.src === "/owner.jpg")).toBe(true));
    const requests = env.fetcher.mock.calls.filter(([url]) => String(url).includes("phase-owner-art"));
    expect(requests.map(([url, init]) => [new URL(String(url)).pathname, init?.cache]).sort()).toEqual([
      ["/scryfall-data.json", "no-store"], ["/scryfall-printings.json", "no-store"],
    ]);
    expect(env.service.resolveOracleIdSync("Alpha")).toBeNull();
    expect(env.service.isCardDataResident()).toBe(false);
    expect(env.service.hasAlternatePrintingsSync(A)).toBe(false);
    expect(env.fetcher.mock.calls.filter(([url]) => String(url) === "/scryfall-printings.json")).toEqual([]);
    for (const hook of hooks) hook.unmount();
  });

  it("does not begin network work after going offline with cache lookups held", async () => {
    const env = await privateOwnerFixture();
    const main = deferred<Response | undefined>();
    const catalog = deferred<Response | undefined>();
    const cache = vi.fn((url: string) => url.includes("printings") ? catalog.promise : main.promise);
    vi.stubGlobal("caches", { match: cache });
    const hook = renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner }));
    await waitFor(() => expect(hook.result.current.src).toBe("/normal.jpg"));
    expect(cache).toHaveBeenCalledTimes(2);
    act(() => env.connectivity.setState({ forcedOffline: true }));
    const requestsBeforeRelease = env.fetcher.mock.calls.length;
    await act(async () => { main.resolve(undefined); catalog.resolve(undefined); });
    await settle();
    expect(env.fetcher).toHaveBeenCalledTimes(requestsBeforeRelease);
    expect(env.fetcher.mock.calls.filter(([url]) => String(url).includes("phase-owner-art"))).toEqual([]);
    expect(hook.result.current.src).toBe("/normal.jpg");
  });

  it("coalesces concurrent offline-to-online reader escalation without retrying a failed online attempt", async () => {
    const env = await setup();
    const main = deferred<Response | undefined>();
    const catalog = deferred<Response | undefined>();
    const cache = vi.fn((url: string) => {
      if (cache.mock.calls.length <= 2) return url.includes("printings") ? catalog.promise : main.promise;
      return Promise.resolve(undefined);
    });
    vi.stubGlobal("caches", { match: cache });
    env.fetcher.mockResolvedValue(new Response(null, { status: 503 }));
    const readers = Array.from({ length: 8 }, () => env.service.createOwnerArtCatalogReader());
    const input = { cardName: "Alpha", oracleId: A, faceIndex: 0 };
    const offline = readers.map((reader) => reader.prepare({ ...input, offline: true }));
    const online = readers.map((reader) => reader.prepare({ ...input, offline: false }));
    main.resolve(undefined);
    catalog.resolve(undefined);
    await Promise.all([...offline, ...online]);
    expect(env.fetcher).toHaveBeenCalledTimes(2);
    expect(readers[0].read({ ...input, offline: false }).printings).toBeNull();
    await settle();
    expect(env.fetcher).toHaveBeenCalledTimes(2);
    for (const reader of readers) reader.release();
  });
});

describe("owner private resource retention", () => {
  it.each(["unmount", "toggle"] as const)("keeps rapid warm reuse but refetches after the final %s grace expires", async (mode) => {
    const env = await privateOwnerFixture();
    vi.useFakeTimers();
    const mount = () => renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner }));
    let hook = mount();
    await settle();
    expect(hook.result.current.src).toBe("/owner.jpg");
    const privateCount = () => env.fetcher.mock.calls.filter(([url]) => String(url).includes("phase-owner-art")).length;
    expect(privateCount()).toBe(2);
    if (mode === "unmount") hook.unmount();
    else act(() => env.preferences.setState({ showOwnerPrintings: false }));
    await act(async () => { await vi.advanceTimersByTimeAsync(1999); });
    if (mode === "unmount") hook = mount();
    else act(() => env.preferences.setState({ showOwnerPrintings: true }));
    expect(hook.result.current.src).toBe("/owner.jpg");
    await settle();
    expect(privateCount()).toBe(2);
    if (mode === "unmount") hook.unmount();
    else act(() => env.preferences.setState({ showOwnerPrintings: false }));
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    if (mode === "unmount") hook = mount();
    else act(() => env.preferences.setState({ showOwnerPrintings: true }));
    expect(hook.result.current.src).toBe("/normal.jpg");
    await settle();
    expect(hook.result.current.src).toBe("/owner.jpg");
    expect(privateCount()).toBe(4);
    expect(env.service.resolveOracleIdSync("Alpha")).toBeNull();
  });

  it("does not evict shared resources until the final mounted consumer leaves", async () => {
    const env = await privateOwnerFixture();
    vi.useFakeTimers();
    const mount = () => renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner }));
    const first = mount();
    const second = mount();
    await settle();
    expect(first.result.current.src).toBe("/owner.jpg");
    expect(second.result.current.src).toBe("/owner.jpg");
    first.unmount();
    await act(async () => { await vi.advanceTimersByTimeAsync(2001); });
    const third = mount();
    expect(third.result.current.src).toBe("/owner.jpg");
    await settle();
    expect(env.fetcher.mock.calls.filter(([url]) => String(url).includes("phase-owner-art"))).toHaveLength(2);
    second.unmount();
    third.unmount();
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    const afterIdle = mount();
    await settle();
    expect(afterIdle.result.current.src).toBe("/owner.jpg");
    expect(env.fetcher.mock.calls.filter(([url]) => String(url).includes("phase-owner-art"))).toHaveLength(4);
  });

  it("discards an in-flight catalog completion after disposal even within the idle grace", async () => {
    const env = await privateOwnerFixture({ holdCatalog: true });
    const original = env.fetcher.getMockImplementation()!;
    let catalogsRequested = 0;
    env.fetcher.mockImplementation(async (input, init) => {
      if (String(input).includes("scryfall-printings") && ++catalogsRequested > 1) return json(env.data.catalog);
      return original(input, init);
    });
    vi.useFakeTimers();
    const mount = () => renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner }));
    const first = mount();
    await settle();
    expect(first.result.current.src).toBe("/normal.jpg");
    expect(catalogsRequested).toBe(1);
    first.unmount();
    await act(async () => { env.gate.resolve(json(env.data.catalog)); });
    await settle();
    expect(env.ownerCalls()).toEqual([]);
    const next = mount();
    await settle();
    expect(next.result.current.src).toBe("/owner.jpg");
    expect(catalogsRequested).toBe(2);
  });

  it("evicts unused locale snapshots while retaining the active locale and rapid locale reuse", async () => {
    const env = await privateOwnerFixture();
    env.installed.delete(encodeCandidateKey("english_printing", [O1, 0, "full_card", "small"]));
    env.installed.delete(encodeCandidateKey("english_printing", [O1, 0, "full_card", "normal"]));
    env.data.catalog[A][0].faces[0].normal = `https://cards.scryfall.io/normal/front/1/1/${O1}.jpg`;
    const original = env.fetcher.getMockImplementation()!;
    env.fetcher.mockImplementation(async (input, init) => {
      const match = String(input).match(/scryfall-images\.v2\.([a-z]+)\.json/);
      if (match) return json({ [O1]: { id: O2, faces: [{
        small: `/${match[1]}-small.jpg`, normal: `/${match[1]}-owner.jpg`, art_crop: `/${match[1]}-art.jpg`,
      }] } });
      return original(input, init);
    });
    env.preferences.setState({ language: "fr" });
    vi.useFakeTimers();
    const hook = renderHook(() => env.useCardImage("Alpha", { oracleId: A, ownerPrinting: owner }));
    await settle();
    await settle();
    expect(hook.result.current.src).toBe("/fr-owner.jpg");
    const frenchRequests = () => env.fetcher.mock.calls.filter(([url]) => String(url).includes("scryfall-images.v2.fr")).length;
    expect(frenchRequests()).toBe(1);
    act(() => env.preferences.setState({ language: "de" }));
    await settle();
    expect(hook.result.current.src).toBe("/de-owner.jpg");
    await act(async () => { await vi.advanceTimersByTimeAsync(1999); });
    act(() => env.preferences.setState({ language: "fr" }));
    await settle();
    expect(hook.result.current.src).toBe("/fr-owner.jpg");
    expect(frenchRequests()).toBe(1);
    act(() => env.preferences.setState({ language: "de" }));
    await settle();
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    act(() => env.preferences.setState({ language: "fr" }));
    await settle();
    expect(hook.result.current.src).toBe("/fr-owner.jpg");
    expect(frenchRequests()).toBe(2);
    expect(env.service.isLocaleArtReady("fr")).toBe(false);
  });
});

it("releases private resources when carrier identity withdraws admission without unmounting", async () => {
  const replacement = catalogs().main;
  const env = await setup({ offline: true, canonicalMain: () => json({ ...replacement, alpha: replacement[B] }) });
  env.preferences.setState({ artOverrides: {}, artChain: [] });
  env.install(encodeCandidateKey("name_face", ["alpha", "alpha", "full_card", "normal"]), "/normal.jpg", "Tk9STUFM", "deck_library");
  env.installPrinting(O1, "/owner.jpg");
  const cache = vi.fn(async (url: string) => cache.mock.calls.length <= 2
    ? json(url.includes("printings") ? env.data.catalog : env.data.main) : undefined);
  vi.stubGlobal("caches", { match: cache });
  vi.useFakeTimers();
  const hook = renderHook(({ identified }) => env.useCardImage("Alpha", {
    ownerPrinting: owner,
    ownerPrintedRef: identified ? { oracle_id: A, face_name: "Alpha" } : undefined,
  }), { initialProps: { identified: true } });
  await settle();
  expect(hook.result.current.src).toBe("/owner.jpg");
  await act(async () => { await env.service.loadScryfallData(); });
  await settle();
  expect(hook.result.current.src).toBe("/owner.jpg");
  hook.rerender({ identified: false });
  await settle();
  expect(hook.result.current.src).toBe("/normal.jpg");
  await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
  const reader = env.service.createOwnerArtCatalogReader();
  const input = { cardName: "Alpha", oracleId: A, offline: false, faceIndex: 0 };
  await act(async () => { await reader.prepare(input); });
  expect(reader.read(input).printings?.map((entry) => entry.id)).toContain(O1);
  expect(env.fetcher.mock.calls.filter(([url]) => String(url).includes("phase-owner-art"))).toHaveLength(1);
  expect(hook.result.current.src).toBe("/normal.jpg");
  reader.release();
});

