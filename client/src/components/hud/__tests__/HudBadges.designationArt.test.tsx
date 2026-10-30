import { act } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DungeonRoomView } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import {
  CityBlessingBadge,
  CounterBadge,
  DayNightBadge,
  DungeonBadge,
  InitiativeBadge,
  MonarchBadge,
  RingBenefitsBadge,
} from "../HudBadges.tsx";

// Badge art resolves through the Scryfall sidecars, which are static assets
// the test environment does not serve. Stubbing the service keeps these tests
// about the badges' behaviour — which lookup each designation performs, what
// renders while loading, and what survives a failure — rather than about
// image loading.
const fetchTokenImageUrl = vi.fn();
const fetchTokenImageAssetByRef = vi.fn();
const fetchCardImageAssetByOracleId = vi.fn();
const fetchTokenImageByRef = vi.fn();

vi.mock("../../../services/scryfall.ts", () => ({
  fetchTokenImageUrl: (...args: unknown[]) => fetchTokenImageUrl(...args),
  fetchTokenImageAssetByRef: (...args: unknown[]) =>
    fetchTokenImageAssetByRef(...args),
  fetchCardImageAssetByOracleId: (...args: unknown[]) =>
    fetchCardImageAssetByOracleId(...args),
  fetchTokenImageByRef: (...args: unknown[]) => fetchTokenImageByRef(...args),
  deriveImageUrl: (url: string, size: string) =>
    url.replace("/normal/", `/${size}/`),
}));

/** Undercity with the marker in Secret Entrance (room index 0). */
function undercity(): DungeonRoomView {
  return {
    dungeon: "Undercity",
    dungeon_name: "Undercity",
    room: {
      index: 0,
      name: "Secret Entrance",
      text: "Search your library for a basic land card, reveal it, put it into your hand, then shuffle.",
    },
    room_count: 18,
    card: {
      oracle_id: "36b61021-72f0-4c22-a41d-9b2f093d7ca8",
      scryfall_id: "2c65185b-6cf0-451d-985e-56aa45d9a57d",
      face_name: "Undercity",
    },
    rooms: [
      {
        index: 0,
        name: "Secret Entrance",
        text: "Search your library for a basic land card, reveal it, put it into your hand, then shuffle.",
        next_rooms: [1],
        marker: { x_permille: 500, y_permille: 215 },
      },
    ],
  };
}

function imgInside(label: string): HTMLImageElement {
  const badge = screen.getByLabelText(label);
  const img = badge.querySelector("img");
  expect(img).not.toBeNull();
  return img as HTMLImageElement;
}

describe("HudBadges designation art", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    fetchTokenImageUrl.mockImplementation(
      async (name: string) => `https://img.example/token/${name}.jpg`,
    );
    fetchTokenImageAssetByRef.mockImplementation(
      async (ref: { face_name: string }) => ({
        src: `https://img.example/ref/${ref.face_name}.jpg`,
        faceIndex: 0,
      }),
    );
    fetchCardImageAssetByOracleId.mockResolvedValue({ src: null });
    fetchTokenImageByRef.mockResolvedValue("https://img.example/undercity.jpg");
    act(() => {
      useGameStore.setState({ gameState: buildGameState() });
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("shows the Monarch glyph first, then the token art", async () => {
    render(<MonarchBadge />);
    expect(screen.getByLabelText("Monarch")).toHaveTextContent("👑");
    expect(fetchTokenImageUrl).toHaveBeenCalledWith("The Monarch", "art_crop");
    const img = await screen.findByRole("presentation", { hidden: true });
    expect(img).toHaveAttribute(
      "src",
      "https://img.example/token/The Monarch.jpg",
    );
    expect(imgInside("Monarch")).toBe(img);
  });

  it("keeps the glyph until the art finishes loading", async () => {
    render(<MonarchBadge />);
    const badge = screen.getByLabelText("Monarch");
    const img = (await screen.findByRole("presentation", {
      hidden: true,
    })) as HTMLImageElement;
    // URL resolved but bytes pending: glyph stays, image hidden.
    expect(badge).toHaveTextContent("👑");
    expect(img).toHaveClass("hidden");
    fireEvent.load(img);
    // Load confirmed: art revealed, glyph gone.
    expect(img).not.toHaveClass("hidden");
    expect(badge).not.toHaveTextContent("👑");
  });

  it("falls back to the glyph while the day/night art switches", async () => {
    act(() => {
      useGameStore.setState({ gameState: buildGameState({ day_night: "Day" }) });
    });
    render(<DayNightBadge />);
    const dayImg = (await screen.findByRole("presentation", {
      hidden: true,
    })) as HTMLImageElement;
    fireEvent.load(dayImg);
    expect(screen.getByLabelText("Day")).not.toHaveTextContent("☀");
    act(() => {
      useGameStore.setState({ gameState: buildGameState({ day_night: "Night" }) });
    });
    // New src pending: moon glyph back until the night art loads.
    expect(screen.getByLabelText("Night")).toHaveTextContent("🌙");
    const nightImg = (await screen.findByRole("presentation", {
      hidden: true,
    })) as HTMLImageElement;
    expect(nightImg).toHaveAttribute(
      "src",
      "https://img.example/ref/Night.jpg",
    );
    expect(nightImg).toHaveClass("hidden");
    fireEvent.load(nightImg);
    expect(nightImg).not.toHaveClass("hidden");
    expect(screen.getByLabelText("Night")).not.toHaveTextContent("🌙");
  });

  it("keeps the Monarch glyph when art fails", async () => {
    fetchTokenImageUrl.mockRejectedValueOnce(new Error("offline"));
    render(<MonarchBadge />);
    expect(screen.getByLabelText("Monarch")).toHaveTextContent("👑");
    // Let the rejected lookup settle: the glyph stays and no image appears.
    await act(async () => {});
    expect(screen.getByLabelText("Monarch")).toHaveTextContent("👑");
    expect(screen.getByLabelText("Monarch").querySelector("img")).toBeNull();
  });

  it("resolves the Initiative face of Undercity, not the dungeon face", async () => {
    render(<InitiativeBadge />);
    expect(screen.getByLabelText("Initiative")).toBeInTheDocument();
    await screen.findByRole("presentation", { hidden: true });
    expect(fetchTokenImageAssetByRef).toHaveBeenCalledWith(
      {
        scryfall_id: "2c65185b-6cf0-451d-985e-56aa45d9a57d",
        face_name: "The Initiative",
        preset_id: "",
      },
      "art_crop",
    );
    expect(imgInside("Initiative")).toHaveAttribute(
      "src",
      "https://img.example/ref/The Initiative.jpg",
    );
  });

  it("keeps the initiative badge separate from the dungeon display", async () => {
    render(
      <>
        <InitiativeBadge />
        <DungeonBadge room={undercity()} />
      </>,
    );
    // Two distinct labelled elements: the attacker reads the initiative
    // holder off the badge, never off the dungeon chip.
    const initiative = screen.getByLabelText("Initiative");
    const dungeon = screen.getByLabelText(
      "Venturing in Undercity, Secret Entrance, room 1 of 18",
    );
    expect(initiative).toBeInTheDocument();
    expect(dungeon).toBeInTheDocument();
    expect(initiative).not.toBe(dungeon);
    expect(initiative.contains(dungeon)).toBe(false);
    expect(dungeon.contains(initiative)).toBe(false);
    // And they resolve different faces: the badge takes "The Initiative",
    // the dungeon panel takes the dungeon face.
    fireEvent.mouseEnter(dungeon);
    await screen.findAllByRole("presentation", { hidden: true });
    expect(fetchTokenImageAssetByRef).toHaveBeenCalledWith(
      expect.objectContaining({ face_name: "The Initiative" }),
      "art_crop",
    );
    expect(fetchTokenImageByRef).toHaveBeenCalledWith(
      expect.objectContaining({ face_name: "Undercity" }),
      "normal",
    );
  });

  it("shows City's Blessing token art", async () => {
    render(<CityBlessingBadge />);
    expect(fetchTokenImageUrl).toHaveBeenCalledWith(
      "City's Blessing",
      "art_crop",
    );
    await screen.findByRole("presentation", { hidden: true });
    expect(imgInside("City's Blessing")).toHaveAttribute(
      "src",
      "https://img.example/token/City's Blessing.jpg",
    );
  });

  it("shows Radiation token art next to the rad count", async () => {
    render(<CounterBadge kind="rad" value={3} />);
    expect(fetchTokenImageUrl).toHaveBeenCalledWith("Radiation", "art_crop");
    const badge = screen.getByLabelText("3 rad counters");
    expect(badge).toHaveTextContent("3");
    const img = await screen.findByRole("presentation", { hidden: true });
    expect(img).toHaveAttribute(
      "src",
      "https://img.example/token/Radiation.jpg",
    );
    expect(badge.contains(img)).toBe(true);
  });

  it("keeps the rad glyph and count when art fails", async () => {
    fetchTokenImageUrl.mockRejectedValueOnce(new Error("offline"));
    render(<CounterBadge kind="rad" value={3} />);
    await act(async () => {});
    const badge = screen.getByLabelText("3 rad counters");
    expect(badge).toHaveTextContent("3");
    expect(badge.querySelector("img")).toBeNull();
  });

  it("shows the Ring emblem art in the benefits popover", async () => {
    render(<RingBenefitsBadge level={2} ringBearerName="Frodo" />);
    fireEvent.mouseEnter(screen.getByLabelText(/level 2/));
    const img = await screen.findByRole("presentation", { hidden: true });
    expect(fetchTokenImageAssetByRef).toHaveBeenCalledWith(
      {
        scryfall_id: "7215460e-8c06-47d0-94e5-d1832d0218af",
        face_name: "The Ring",
        preset_id: "",
      },
      "art_crop",
    );
    expect(img).toHaveAttribute(
      "src",
      "https://img.example/ref/The Ring.jpg",
    );
    // Fallback diamond beside the two active-level diamonds until load, then
    // just the level diamonds once the emblem art arrives.
    expect(img).toHaveClass("hidden");
    expect(screen.getAllByText("◆")).toHaveLength(3);
    fireEvent.load(img);
    expect(img).not.toHaveClass("hidden");
    expect(screen.getAllByText("◆")).toHaveLength(2);
  });

  describe("DayNightBadge", () => {
    it("renders nothing when neither day nor night is in effect", () => {
      render(<DayNightBadge />);
      expect(screen.queryByLabelText("Day")).toBeNull();
      expect(screen.queryByLabelText("Night")).toBeNull();
      expect(fetchTokenImageAssetByRef).not.toHaveBeenCalled();
    });

    it("shows the Day face when it is day", async () => {
      act(() => {
        useGameStore.setState({ gameState: buildGameState({ day_night: "Day" }) });
      });
      render(<DayNightBadge />);
      expect(screen.getByLabelText("Day")).toHaveTextContent("☀");
      await screen.findByRole("presentation", { hidden: true });
      expect(fetchTokenImageAssetByRef).toHaveBeenCalledWith(
        {
          scryfall_id: "9c0f7843-4cbb-4d0f-8887-ec823a9238da",
          face_name: "Day",
          preset_id: "",
        },
        "art_crop",
      );
      expect(imgInside("Day")).toHaveAttribute(
        "src",
        "https://img.example/ref/Day.jpg",
      );
    });

    it("shows the Night face when it is night", async () => {
      act(() => {
        useGameStore.setState({
          gameState: buildGameState({ day_night: "Night" }),
        });
      });
      render(<DayNightBadge />);
      expect(screen.getByLabelText("Night")).toHaveTextContent("🌙");
      await screen.findByRole("presentation", { hidden: true });
      expect(imgInside("Night")).toHaveAttribute(
        "src",
        "https://img.example/ref/Night.jpg",
      );
    });
  });
});
