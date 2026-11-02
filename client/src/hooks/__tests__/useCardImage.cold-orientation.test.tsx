import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { VisualRepositoryRequest } from "../../services/visualPacks/repository.ts";
import { assetKey, catalogRoot, decodeCandidateKey, packId } from "../../services/visualPacks/types.ts";
import type { CardImageSource } from "../../services/visualPacks/types.ts";

const repository = vi.hoisted(() => ({ revision: "0", listeners: new Set<() => void>(), resolve: vi.fn() }));
vi.mock("../../services/visualPacks/repository.ts", () => ({
  visualPackRepository: {
    currentRevision: () => repository.revision,
    subscribe: (listener: () => void) => { repository.listeners.add(listener); return () => repository.listeners.delete(listener); },
    resolve: repository.resolve,
  },
}));

// Two finite real module graphs keep failure/retry and successful cold admission
// independent. Resetting the cache does not mutate the previously bound graph.
const coldService = await import("../../services/scryfall.ts");
const coldHook = await import("../useCardImage.ts");
const coldConnectivity = await import("../../stores/connectivityStore.ts");
const coldPreferences = await import("../../stores/preferencesStore.ts");
vi.resetModules();
const retryService = await import("../../services/scryfall.ts");
const retryHook = await import("../useCardImage.ts");
const retryConnectivity = await import("../../stores/connectivityStore.ts");
const retryPreferences = await import("../../stores/preferencesStore.ts");

const FRONT = "Invasion of Alara";
const BACK = "Awaken the Maelstrom";
const PIN = "44444444-4444-4444-8444-444444444444";
const OTHER_PIN = "55555555-5555-4555-8555-555555555555";
const ORACLE = "33333333-3333-4333-8333-333333333333";
const DURING = "66666666-6666-4666-8666-666666666666";
const FILTERED = "77777777-7777-4777-8777-777777777777";
const AFTER = "88888888-8888-4888-8888-888888888888";
const STALE = "99999999-9999-4999-8999-999999999999";
const REMOTE_NULL = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const RETRY_INSTALLED = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const FALLBACK: CardImageSource = { kind: "fallback", src: null };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => { resolve = settle; });
  return { promise, resolve };
}
function response(value: unknown) { return new Response(JSON.stringify(value)); }
function entry(oracleId: string) {
  return {
    oracle_id: oracleId, name: `${FRONT} // ${BACK}`, face_names: [FRONT.toLowerCase(), BACK.toLowerCase()],
    layout: "transform", mana_cost: "{W}{U}{B}{R}{G}", cmc: 5, type_line: "Battle — Siege // Sorcery",
    colors: [], color_identity: [], keywords: [],
    faces: [{ normal: `https://img.example/${oracleId}-front.jpg`, orientation: "landscape" }, { normal: `https://img.example/${oracleId}-back.jpg`, orientation: "portrait" }],
  };
}
function printing(id: string) {
  return {
    id, set: "mom", set_name: "March of the Machine", collector_number: id === PIN ? "230" : "231",
    released_at: "2023-04-21", border_color: "black", frame_effects: [], full_art: false,
    faces: [{ normal: `https://img.example/${id}-front.jpg` }, { normal: `https://img.example/${id}-back.jpg` }],
  };
}
function installed(src: string): CardImageSource {
  return { kind: "installed", src, assetKey: assetKey("asset:v1:canonical_card:cold"), packId: packId("deck_library"), catalogRoot: catalogRoot("a".repeat(64)) };
}
function semanticRequest(request: VisualRepositoryRequest) {
  return request.groups.flatMap((group) => group.requested).map(decodeCandidateKey).find(([kind]) => kind === "oracle_face")?.[1];
}

afterEach(() => { cleanup(); repository.listeners.clear(); vi.unstubAllGlobals(); });

describe("cold orientation with the real service and hook", () => {
  it("admits printings first, binds the active pin only after main metadata, and preserves installed exhaustion ownership", async () => {
    coldPreferences.usePreferencesStore.setState({ language: "en", artChain: [], artOverrides: {} });
    coldConnectivity.useConnectivityStore.setState({ forcedOffline: true, browserOnline: true });
    const main = deferred<Response>();
    const printings = deferred<Response>();
    const fetchMock = vi.fn((url: string) => {
      if (url === "/scryfall-data.json") return main.promise;
      if (url === "/scryfall-printings.json") return printings.promise;
      throw new Error(`unexpected metadata URL: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    repository.resolve.mockImplementation(async (request: VisualRepositoryRequest) => {
      if (request.allowRemote) return { revision: repository.revision, sources: request.remote ? [{ kind: "remote", src: request.remote.src }, FALLBACK] : [FALLBACK] };
      return { revision: repository.revision, sources: [installed("/offline.jpg"), FALLBACK] };
    });
    const offline = renderHook(() => coldHook.useCardImage(FRONT, { oracleId: ORACLE, faceName: FRONT }));
    const token = renderHook(() => coldHook.useCardImage("Saproling", { isToken: true }));
    await waitFor(() => {
      expect(offline.result.current.src).toBe("/offline.jpg");
      expect(offline.result.current.isLoading).toBe(false);
      expect(token.result.current.src).toBe("/offline.jpg");
      expect(token.result.current.isRotated).toBe(false);
    });
    expect(fetchMock).not.toHaveBeenCalled();
    expect(repository.resolve.mock.calls.every(([request]) => !request.allowRemote)).toBe(true);
    offline.unmount(); token.unmount();
    coldConnectivity.useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    repository.resolve.mockClear();
    const requests: VisualRepositoryRequest[] = [];
    const remoteDuring = deferred<{ revision: string; sources: CardImageSource[] }>();
    repository.resolve.mockImplementation(async (request: VisualRepositoryRequest) => {
      requests.push(request);
      if (request.allowRemote) {
        if (request.remote?.src === `https://img.example/${DURING}-front.jpg`) return remoteDuring.promise;
        return { revision: repository.revision, sources: request.remote?.src === `https://img.example/${REMOTE_NULL}-front.jpg` ? [FALLBACK] : [{ kind: "remote", src: request.remote!.src }, FALLBACK] };
      }
      const identity = semanticRequest(request);
      const id = identity?.[0];
      let sources: CardImageSource[] = [FALLBACK];
      if (id === DURING) sources = [installed("/during-first.jpg"), installed("/during-first.jpg"), installed("/during-second.jpg"), FALLBACK];
      if (id === FILTERED) sources = [installed("/filtered-first.jpg"), installed("/filtered-second.jpg"), FALLBACK];
      if (id === AFTER) sources = [installed("/after-first.jpg"), installed("/after-second.jpg"), FALLBACK];
      if (id === STALE) sources = [installed(identity?.[1] === BACK.toLowerCase() ? "/stale-back.jpg" : "/stale-front.jpg"), FALLBACK];
      return { revision: repository.revision, sources };
    });
    const front = renderHook(() => coldHook.useCardImage(FRONT, { oracleId: ORACLE, faceName: FRONT, scryfallId: PIN }));
    const back = renderHook(() => coldHook.useCardImage(FRONT, { oracleId: ORACLE, faceName: BACK, faceIndex: 0, scryfallId: PIN }));
    const during = renderHook(() => coldHook.useCardImage(FRONT, { oracleId: DURING, faceName: FRONT }));
    const filtered = renderHook(() => coldHook.useCardImage(FRONT, { oracleId: FILTERED, faceName: FRONT }));
    const after = renderHook(() => coldHook.useCardImage(FRONT, { oracleId: AFTER, faceName: FRONT }));
    const stale = renderHook(({ face }) => coldHook.useCardImage(FRONT, { oracleId: STALE, faceName: face, faceIndex: 0 }), { initialProps: { face: FRONT } });
    const remoteNull = renderHook(() => coldHook.useCardImage(FRONT, { oracleId: REMOTE_NULL, faceName: FRONT }));
    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith("/scryfall-data.json");
      expect(fetchMock).toHaveBeenCalledWith("/scryfall-printings.json");
      expect(during.result.current.src).toBe("/during-first.jpg");
      expect(during.result.current.isLoading).toBe(false);
      expect(filtered.result.current.src).toBe("/filtered-first.jpg");
      expect(after.result.current.src).toBe("/after-first.jpg");
      expect(stale.result.current.src).toBe("/stale-front.jpg");
    });
    const missesBeforePrintings = requests.filter((request) => !request.allowRemote && semanticRequest(request)?.[0] === ORACLE).length;
    expect(missesBeforePrintings).toBeGreaterThanOrEqual(2);
    expect(front.result.current.src).toBeNull();
    expect(back.result.current.src).toBeNull();
    expect(coldService.resolveFaceIndexSync(ORACLE, BACK)).toBeNull();
    act(() => during.result.current.advanceFailedSource?.("/during-first.jpg"));
    expect(during.result.current.src).toBe("/during-second.jpg");
    act(() => during.result.current.advanceFailedSource?.("/during-second.jpg"));
    expect(during.result.current.isLoading).toBe(true);
    act(() => filtered.result.current.advanceFailedSource?.("/filtered-first.jpg"));
    expect(filtered.result.current.src).toBe("/filtered-second.jpg");
    stale.rerender({ face: BACK });
    expect(stale.result.current.src).toBeNull();
    await waitFor(() => {
      expect(stale.result.current.src).toBe("/stale-back.jpg");
      expect(stale.result.current.isLoading).toBe(false);
    });
    await act(async () => printings.resolve(response({ [ORACLE]: [printing(PIN), printing(OTHER_PIN)] })));
    expect(await coldService.getCardPrintings(ORACLE)).toEqual([printing(PIN), printing(OTHER_PIN)]);
    await waitFor(() => expect(requests.filter((request) => !request.allowRemote && semanticRequest(request)?.[0] === ORACLE).length).toBeGreaterThan(missesBeforePrintings));
    expect(coldService.resolveFaceIndexSync(ORACLE, BACK)).toBeNull();
    expect(front.result.current.isLoading).toBe(true);
    expect(requests.some((request) => request.allowRemote && request.remote?.src.includes(PIN))).toBe(false);
    await act(async () => main.resolve(response(Object.fromEntries([ORACLE, DURING, FILTERED, AFTER, STALE, REMOTE_NULL].map((id) => [id, entry(id)])))));
    await waitFor(() => {
      expect(front.result.current.src).toBe(`https://img.example/${PIN}-front.jpg`);
      expect(front.result.current.isRotated).toBe(true);
      expect(front.result.current.isLoading).toBe(false);
      expect(back.result.current.src).toBe(`https://img.example/${PIN}-back.jpg`);
      expect(back.result.current.isRotated).toBe(false);
      expect(back.result.current.isLoading).toBe(false);
      expect(filtered.result.current.src).toBe("/filtered-second.jpg");
      expect(filtered.result.current.isRotated).toBe(true);
      expect(after.result.current.isRotated).toBe(true);
      expect(stale.result.current.src).toBe("/stale-back.jpg");
      expect(stale.result.current.isRotated).toBe(false);
      expect(requests.some((request) => request.allowRemote && request.remote?.src === `https://img.example/${DURING}-front.jpg`)).toBe(true);
      expect(requests.some((request) => request.allowRemote && request.remote?.src === `https://img.example/${REMOTE_NULL}-front.jpg`)).toBe(true);
      expect(remoteNull.result.current.src).toBeNull();
      expect(remoteNull.result.current.isLoading).toBe(false);
    });
    expect(during.result.current.src).toBeNull();
    expect(during.result.current.isLoading).toBe(true);
    await act(async () => remoteDuring.resolve({ revision: "0", sources: [installed("/during-first.jpg"), installed("/during-second.jpg"), { kind: "remote", src: `https://img.example/${DURING}-front.jpg` }, FALLBACK] }));
    await waitFor(() => {
      expect(during.result.current.src).toBe(`https://img.example/${DURING}-front.jpg`);
      expect(during.result.current.isLoading).toBe(false);
      expect(during.result.current.isRotated).toBe(true);
    });
    act(() => after.result.current.advanceFailedSource?.("/after-first.jpg"));
    expect(after.result.current.src).toBe("/after-second.jpg");
    act(() => after.result.current.advanceFailedSource?.("/after-second.jpg"));
    await waitFor(() => {
      expect(after.result.current.src).toBe(`https://img.example/${AFTER}-front.jpg`);
      expect(after.result.current.isLoading).toBe(false);
    });
    expect(requests.some((request) => request.allowRemote && request.remote?.src === `https://img.example/${AFTER}-front.jpg`)).toBe(true);
    expect(fetchMock.mock.calls.filter(([url]) => url === "/scryfall-data.json")).toHaveLength(1);
  });

  it("keeps installed and noninstalled pin art usable after null metadata, then repairs both on a mounted revision", async () => {
    retryConnectivity.useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    retryPreferences.usePreferencesStore.setState({ language: "en", artChain: [], artOverrides: {} });
    const firstMain = deferred<Response>();
    const nextMain = deferred<Response>();
    let mainRequests = 0;
    const fetchMock = vi.fn((url: string) => {
      if (url === "/scryfall-data.json") return ++mainRequests === 1 ? firstMain.promise : nextMain.promise;
      if (url === "/scryfall-printings.json") return Promise.resolve(response({ [ORACLE]: [printing(PIN), printing(OTHER_PIN)] }));
      throw new Error(`unexpected metadata URL: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    await expect(retryService.getCardPrintings(ORACLE)).resolves.toEqual([printing(PIN), printing(OTHER_PIN)]);
    const requests: VisualRepositoryRequest[] = [];
    repository.resolve.mockImplementation(async (request: VisualRepositoryRequest) => {
      requests.push(request);
      const identity = semanticRequest(request);
      return { revision: repository.revision, sources: request.allowRemote
        ? [{ kind: "remote", src: request.remote!.src }, FALLBACK]
        : identity?.[0] === RETRY_INSTALLED ? [installed("/retry-installed.jpg"), FALLBACK] : [FALLBACK] };
    });
    const local = renderHook(() => retryHook.useCardImage(FRONT, { oracleId: RETRY_INSTALLED, faceName: FRONT }));
    const front = renderHook(() => retryHook.useCardImage(FRONT, { oracleId: ORACLE, faceName: FRONT, scryfallId: PIN }));
    const back = renderHook(() => retryHook.useCardImage(FRONT, { oracleId: ORACLE, faceName: BACK, faceIndex: 0, scryfallId: PIN }));
    await waitFor(() => {
      expect(local.result.current.src).toBe("/retry-installed.jpg");
      expect(local.result.current.isLoading).toBe(false);
      expect(mainRequests).toBe(1);
    });
    // The first printing update is observed before settling the shared failed load.
    await waitFor(() => expect(requests.filter((request) => !request.allowRemote && semanticRequest(request)?.[0] === ORACLE).length).toBeGreaterThanOrEqual(3));
    await act(async () => firstMain.resolve(response({ invalid: true })));
    await waitFor(() => {
      expect(front.result.current.src).toBe(`https://img.example/${PIN}-front.jpg`);
      expect(front.result.current.isLoading).toBe(false);
      expect(front.result.current.isRotated).toBe(false);
      expect(back.result.current.src).toBe(`https://img.example/${PIN}-front.jpg`);
      expect(back.result.current.isLoading).toBe(false);
      expect(local.result.current.src).toBe("/retry-installed.jpg");
      expect(local.result.current.isLoading).toBe(false);
    });
    expect(requests.filter((request) => request.allowRemote).every((request) => request.remote?.src.includes(PIN))).toBe(true);
    expect(mainRequests).toBe(1);
    act(() => { repository.revision = "1"; for (const listener of repository.listeners) listener(); });
    await waitFor(() => {
      expect(mainRequests).toBe(2);
      expect(local.result.current.src).toBe("/retry-installed.jpg");
      expect(local.result.current.isLoading).toBe(false);
    });
    await act(async () => nextMain.resolve(response({ [ORACLE]: entry(ORACLE), [RETRY_INSTALLED]: entry(RETRY_INSTALLED) })));
    await waitFor(() => {
      expect(front.result.current.src).toBe(`https://img.example/${PIN}-front.jpg`);
      expect(front.result.current.isRotated).toBe(true);
      expect(front.result.current.isLoading).toBe(false);
      expect(back.result.current.src).toBe(`https://img.example/${PIN}-back.jpg`);
      expect(back.result.current.isRotated).toBe(false);
      expect(back.result.current.isLoading).toBe(false);
      expect(local.result.current.src).toBe("/retry-installed.jpg");
      expect(local.result.current.isRotated).toBe(true);
      expect(local.result.current.isLoading).toBe(false);
    });
    expect(requests.some((request) => request.allowRemote && request.remote?.src.includes(RETRY_INSTALLED))).toBe(false);
    expect(retryService.resolveFaceIndexSync(ORACLE, BACK)).toBe(1);
  });
});
