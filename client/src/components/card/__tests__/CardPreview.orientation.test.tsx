import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { VisualRepositoryRequest } from "../../../services/visualPacks/repository.ts";
import { assetKey, catalogRoot, decodeCandidateKey, packId } from "../../../services/visualPacks/types.ts";
import type { CardImageSource } from "../../../services/visualPacks/types.ts";
import { useConnectivityStore } from "../../../stores/connectivityStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { CardPreview } from "../CardPreview.tsx";

const repository = vi.hoisted(() => ({ resolve: vi.fn() }));
vi.mock("../../../services/visualPacks/repository.ts", () => ({
  visualPackRepository: { currentRevision: () => "0", subscribe: () => () => {}, resolve: repository.resolve },
}));
vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null, useCardParseDetails: () => null, useCardRulings: () => [],
}));

const FRONT = "Invasion of Alara";
const BACK = "Awaken the Maelstrom";
const ORACLE = "33333333-3333-4333-8333-333333333333";
const FALLBACK: CardImageSource = { kind: "fallback", src: null };

afterEach(() => {
  cleanup(); vi.unstubAllGlobals();
  useGameStore.setState({ gameState: null, spellCosts: {}, legalActionsByObject: {} });
  useUiStore.getState().dismissPreview();
});

describe("desktop CardPreview with real image metadata", () => {
  it("shows installed art while metadata is pending, follows Ctrl to the portrait back, and exhausts installed art into real remote art", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1280 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 900 });
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    usePreferencesStore.setState({ language: "en", artChain: [], artOverrides: {}, animationSpeedMultiplier: 0, showCardPreviewFooter: false });
    let resolveMain!: (value: Response) => void;
    const pendingMain = new Promise<Response>((resolve) => { resolveMain = resolve; });
    const fetchMock = vi.fn((url: string) => {
      if (url === "/scryfall-data.json") return pendingMain;
      throw new Error(`unexpected metadata URL: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);
    repository.resolve.mockImplementation(async (request: VisualRepositoryRequest) => {
      const face = request.groups.flatMap((group) => group.requested).map(decodeCandidateKey)
        .find(([kind]) => kind === "oracle_face")?.[1][1];
      const sources: CardImageSource[] = request.allowRemote
        ? [{ kind: "remote", src: request.remote!.src }, FALLBACK]
        : [{ kind: "installed", src: face === BACK.toLowerCase() ? "/preview-back.jpg" : "/preview-front.jpg", assetKey: assetKey("asset:v1:canonical_card:preview"), packId: packId("deck_library"), catalogRoot: catalogRoot("a".repeat(64)) }, FALLBACK];
      return { revision: "0", sources };
    });
    const back = gameObjectFactory.sorcery().named(BACK).params({ printed_ref: { oracle_id: ORACLE, face_name: BACK } }).build();
    const front = gameObjectFactory.onBattlefield().named(FRONT).withId(101).params({
      printed_ref: { oracle_id: ORACLE, face_name: FRONT }, transformed: false,
      back_face: { name: BACK, power: back.power, toughness: back.toughness, card_types: back.card_types, mana_cost: back.mana_cost, keywords: back.keywords, abilities: back.abilities, color: back.color, printed_ref: back.printed_ref, layout_kind: "Transform" },
    }).build();
    useGameStore.setState({ gameState: gameStateFactory.withObjects(front).build(), spellCosts: {} });
    const { container } = render(<CardPreview cardName={FRONT} backFaceName={BACK} objectId={front.id} dockSide dockPosition="middle-right" />);
    const image = await screen.findByRole("img", { name: FRONT });
    expect(image).toHaveAttribute("src", "/preview-front.jpg");
    expect(fetchMock).toHaveBeenCalledWith("/scryfall-data.json");
    expect(repository.resolve.mock.calls.every(([request]) => !request.allowRemote)).toBe(true);
    await waitFor(() => expect(screen.queryByLabelText(`Loading ${FRONT}`)).not.toBeInTheDocument());
    const preview = container.querySelector<HTMLElement>("[data-card-preview]")!;
    const portraitPosition = preview.style.top;
    expect(portraitPosition).not.toBe("");
    await act(async () => resolveMain(new Response(JSON.stringify({ [ORACLE]: {
      oracle_id: ORACLE, name: `${FRONT} // ${BACK}`, face_names: [FRONT.toLowerCase(), BACK.toLowerCase()],
      layout: "transform", mana_cost: "{W}{U}{B}{R}{G}", cmc: 5, type_line: "Battle — Siege // Sorcery", colors: [], color_identity: [], keywords: [],
      faces: [{ normal: "https://img.example/preview-remote-front.jpg", orientation: "landscape" }, { normal: "https://img.example/preview-remote-back.jpg", orientation: "portrait" }],
    } }))));
    await waitFor(() => {
      expect(screen.getByRole("img", { name: FRONT })).toHaveAttribute("src", "/preview-front.jpg");
      // The centered preview's height changes with the displayed face's geometry.
      expect(preview.style.top).not.toBe(portraitPosition);
    });
    const landscapePosition = preview.style.top;
    fireEvent.keyDown(window, { key: "Control" });
    await waitFor(() => {
      expect(screen.getByRole("img", { name: BACK })).toHaveAttribute("src", "/preview-back.jpg");
      expect(preview.style.top).toBe(portraitPosition);
    });
    fireEvent.keyUp(window, { key: "Control" });
    await waitFor(() => {
      expect(screen.getByRole("img", { name: FRONT })).toHaveAttribute("src", "/preview-front.jpg");
      expect(preview.style.top).toBe(landscapePosition);
    });
    fireEvent.error(screen.getByRole("img", { name: FRONT }));
    await waitFor(() => {
      expect(screen.getByRole("img", { name: FRONT })).toHaveAttribute("src", "https://img.example/preview-remote-front.jpg");
      expect(preview.style.top).toBe(landscapePosition);
    });
    expect(repository.resolve).toHaveBeenCalledWith(expect.objectContaining({ allowRemote: true, remote: expect.objectContaining({ src: "https://img.example/preview-remote-front.jpg" }) }));
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});
