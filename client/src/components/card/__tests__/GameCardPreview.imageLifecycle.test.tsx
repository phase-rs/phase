import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useCardImage } from "../../../hooks/useCardImage.ts";
import { decodeCandidateKey } from "../../../services/visualPacks/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { GameCardPreview } from "../GameCardPreview.tsx";

interface Deferred<T> {
  promise: Promise<T>;
  resolve(value: T): void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => { resolve = next; });
  return { promise, resolve };
}

const lifecycle = vi.hoisted(() => ({
  aRemote: null as Deferred<Record<string, unknown>> | null,
  exposeInstalledOnNextLocalResolve: false,
  localOracleIds: [] as string[],
  remoteRepositoryCalls: [] as string[],
  fetchCardImageAssetByOracleId: vi.fn(),
  repositoryResolve: vi.fn(),
}));

const REMOTE_A = "https://images.example/a.jpg";
const REMOTE_B = "https://images.example/b.jpg";
const BROKEN_B = "tauri://deck-catalog/broken-b.jpg";
const ORACLE_A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const ORACLE_B = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const PRINTING_A = "11111111-1111-4111-8111-111111111111";
const PRINTING_B = "22222222-2222-4222-8222-222222222222";

function imageAsset(oracleId: string, printingId: string, faceName: string, src: string) {
  return {
    src,
    isRotated: false,
    semantic: {
      oracleId,
      faceIndex: 0,
      alias: faceName.toLowerCase(),
      englishPrintingId: printingId,
    },
  };
}

function requestedOracleId(groups: Array<{ requested: string[] }>): string | null {
  for (const group of groups) {
    for (const key of group.requested) {
      const [kind, values] = decodeCandidateKey(key);
      if (kind === "oracle_face" && typeof values[0] === "string") return values[0];
    }
  }
  return null;
}

vi.mock("../../../services/scryfall.ts", () => ({
  CARD_BACK_URL: "card-back.png",
  IMAGE_SIZE_WIDTHS: { small: 146, normal: 488 },
  deriveImageUrl: (url: string) => url,
  fetchCardImageAsset: vi.fn(),
  fetchCardImageAssetByOracleId: lifecycle.fetchCardImageAssetByOracleId,
  fetchTokenImageAssetByRef: vi.fn(),
  fetchTokenImageUrl: vi.fn(),
  findPrintingById: vi.fn(),
  getCardPrintings: vi.fn().mockResolvedValue([]),
  imageUrlSize: vi.fn(() => null),
  isCardImageFlipLayoutSync: vi.fn(() => false),
  isCardImageRotatedSync: vi.fn(() => false),
  isLocaleArtReady: vi.fn(() => true),
  loadLocaleArt: vi.fn().mockResolvedValue(undefined),
  resolveFaceIndexSync: vi.fn(() => null),
  resolveOracleIdSync: vi.fn(() => null),
  resolvePrintingImageUrl: vi.fn(),
}));

vi.mock("../../../services/visualPacks/repository.ts", () => ({
  visualPackRepository: {
    currentRevision: () => "0",
    subscribe: () => () => {},
    resolve: lifecycle.repositoryResolve,
  },
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useCardParseDetails: () => null,
  useCardRulings: () => [],
  useEngineCardData: () => null,
}));

function WarmNormalTile() {
  const image = useCardImage("Beta Card", {
    size: "normal",
    oracleId: ORACLE_B,
    faceName: "Beta Face",
  });
  return image.src ? <img alt="warm beta tile" src={image.src} /> : null;
}

function HandMarkers({ activeObjectId }: { activeObjectId: number | null }) {
  return (
    <>
      {[101, 202].map((objectId) => (
        <div
          key={objectId}
          data-hand-card
          data-hand-touch-active={activeObjectId === objectId ? "true" : undefined}
          data-object-id={objectId}
        />
      ))}
    </>
  );
}

afterEach(() => {
  cleanup();
  lifecycle.aRemote = null;
  lifecycle.exposeInstalledOnNextLocalResolve = false;
  lifecycle.localOracleIds = [];
  lifecycle.remoteRepositoryCalls = [];
  lifecycle.fetchCardImageAssetByOracleId.mockReset();
  lifecycle.repositoryResolve.mockReset();
  useGameStore.setState({ gameState: null, spellCosts: {}, legalActionsByObject: {} });
  usePreferencesStore.setState({ artChain: [], artOverrides: {}, cardPreviewMode: "follow" });
  useUiStore.setState({
    inspectedObjectId: null,
    inspectedFaceIndex: 0,
    isDragging: false,
    previewSticky: false,
    previewPlacement: "cursor",
    shiftHeld: false,
  });
});

describe("GameCardPreview hand image lifecycle", () => {
  it("does not publish a warmed next-hand image before its local source fails", async () => {
    const pendingA = deferred<Record<string, unknown>>();
    lifecycle.aRemote = pendingA;
    lifecycle.fetchCardImageAssetByOracleId.mockImplementation((oracleId: string, faceName: string) => {
      if (oracleId === ORACLE_A) return pendingA.promise;
      if (oracleId === ORACLE_B) {
        return Promise.resolve(imageAsset(oracleId, PRINTING_B, faceName, REMOTE_B));
      }
      throw new Error(`unexpected oracle image request: ${oracleId}`);
    });
    lifecycle.repositoryResolve.mockImplementation(async (request: {
      allowRemote: boolean;
      remote?: { src: string };
      groups: Array<{ requested: string[] }>;
    }) => {
      if (!request.allowRemote) {
        const oracleId = requestedOracleId(request.groups);
        if (oracleId) lifecycle.localOracleIds.push(oracleId);
        if (lifecycle.exposeInstalledOnNextLocalResolve && oracleId === ORACLE_B) {
          lifecycle.exposeInstalledOnNextLocalResolve = false;
          return {
            revision: "0",
            sources: [
              {
                kind: "installed" as const,
                src: BROKEN_B,
                assetKey: "asset:v1:canonical_card:beta",
                packId: "deck_library",
                catalogRoot: "b".repeat(64),
              },
              { kind: "fallback" as const, src: null },
            ],
          };
        }
        return { revision: "0", sources: [{ kind: "fallback" as const, src: null }] };
      }

      const postErrorBResolution = request.remote?.src === REMOTE_B
        && lifecycle.remoteRepositoryCalls.includes(REMOTE_B);
      lifecycle.remoteRepositoryCalls.push(request.remote?.src ?? "");
      return {
        revision: "0",
        sources: [
          ...(postErrorBResolution
            ? [{
                kind: "installed" as const,
                src: BROKEN_B,
                assetKey: "asset:v1:canonical_card:beta",
                packId: "deck_library",
                catalogRoot: "b".repeat(64),
              }]
            : []),
          { kind: "remote" as const, src: request.remote!.src },
          { kind: "fallback" as const, src: null },
        ],
      };
    });

    const alpha = gameObjectFactory
      .withId(101)
      .inHand()
      .named("Alpha Card")
      .params({ printed_ref: { oracle_id: ORACLE_A, face_name: "Alpha Face" } })
      .build();
    const beta = gameObjectFactory
      .withId(202)
      .inHand()
      .named("Beta Card")
      .params({ printed_ref: { oracle_id: ORACLE_B, face_name: "Beta Face" } })
      .build();
    useGameStore.setState({
      gameState: gameStateFactory
        .withPlayers({ id: 0, hand: [alpha.id, beta.id] }, 1)
        .withObjects(alpha, beta)
        .build(),
      spellCosts: {},
    });

    const { container, rerender } = render(
      <>
        <HandMarkers activeObjectId={null} />
        <WarmNormalTile />
        <GameCardPreview />
      </>,
    );

    await screen.findByAltText("warm beta tile");
    expect(lifecycle.fetchCardImageAssetByOracleId).toHaveBeenCalledTimes(1);
    expect(lifecycle.fetchCardImageAssetByOracleId).toHaveBeenCalledWith(
      ORACLE_B,
      "Beta Face",
      "normal",
    );
    expect(lifecycle.remoteRepositoryCalls).toEqual([REMOTE_B]);

    act(() => {
      useUiStore.setState({ inspectedObjectId: alpha.id, inspectedFaceIndex: 0 });
      rerender(
        <>
          <HandMarkers activeObjectId={alpha.id} />
          <WarmNormalTile />
          <GameCardPreview />
        </>,
      );
    });
    await waitFor(() => {
      expect(lifecycle.fetchCardImageAssetByOracleId).toHaveBeenCalledWith(
        ORACLE_A,
        "Alpha Face",
        "normal",
      );
      expect(lifecycle.localOracleIds).toContain(ORACLE_A);
    });

    lifecycle.exposeInstalledOnNextLocalResolve = true;
    const previewBeforeSwitch = container.querySelector("[data-card-preview]");
    expect(previewBeforeSwitch).not.toBeNull();
    act(() => {
      useUiStore.setState({ inspectedObjectId: beta.id, inspectedFaceIndex: 0 });
      rerender(
        <>
          <HandMarkers activeObjectId={beta.id} />
          <WarmNormalTile />
          <GameCardPreview />
        </>,
      );
    });

    const previewAfterSwitch = container.querySelector("[data-card-preview]");
    expect(previewAfterSwitch).toBe(previewBeforeSwitch);
    expect(screen.queryByAltText("Beta Card")).toBeNull();

    const brokenImage = await screen.findByAltText("Beta Card");
    expect(brokenImage).toHaveAttribute("src", BROKEN_B);
    expect(lifecycle.localOracleIds).toContain(ORACLE_B);
    // The installed source settles the local stage. Its remote continuation is
    // deliberately deferred until this image reports failure.
    expect(lifecycle.remoteRepositoryCalls).toEqual([REMOTE_B]);

    fireEvent.error(brokenImage);
    fireEvent.error(brokenImage);

    await waitFor(() => {
      expect(screen.getByAltText("Beta Card")).toHaveAttribute("src", REMOTE_B);
    });
    expect(lifecycle.fetchCardImageAssetByOracleId).toHaveBeenCalledTimes(2);
    expect(lifecycle.remoteRepositoryCalls).toEqual([REMOTE_B, REMOTE_B]);

    act(() => {
      pendingA.resolve(imageAsset(ORACLE_A, PRINTING_A, "Alpha Face", REMOTE_A));
    });
    await waitFor(() => {
      expect(lifecycle.remoteRepositoryCalls).toContain(REMOTE_A);
    });

    expect(screen.getByAltText("Beta Card")).toHaveAttribute("src", REMOTE_B);
  });
});
