import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { GameState } from "../../../adapter/types";
import type { DownloadResult } from "../../../services/fileDownload";
import { useGameStore } from "../../../stores/gameStore";
import { useUiStore } from "../../../stores/uiStore";
import { HelpSheet } from "../HelpSheet";

const { downloadCurrentReplay } = vi.hoisted(() => ({ downloadCurrentReplay: vi.fn() }));
vi.mock("../../../services/replayExport.ts", () => ({ downloadCurrentReplay }));

describe("HelpSheet replay export status", () => {
  beforeEach(() => {
    downloadCurrentReplay.mockReset();
    useUiStore.setState({ helpSheetOpen: true });
    // The only thing the Export Replay button is gated on.
    useGameStore.setState({ gameState: { turn_number: 1 } as GameState });
  });

  afterEach(() => {
    cleanup();
    useUiStore.setState({ helpSheetOpen: false });
    useGameStore.setState({ gameState: null });
  });

  async function clickExportReplay() {
    render(<HelpSheet />);
    fireEvent.click(screen.getByText("Export Replay"));
    return screen.findByText(/./, { selector: "p.text-emerald-300" });
  }

  it.each<[string, DownloadResult | null, string]>([
    [
      "a shell save reports the absolute path it landed on",
      { kind: "saved", filename: "replay.json", path: "/home/u/Downloads/replay.json" },
      "Exported to /home/u/Downloads/replay.json.",
    ],
    [
      "a browser save reports the filename it asked for",
      { kind: "saved", filename: "replay.json" },
      "Replay saved as replay.json.",
    ],
    [
      "a shell that never reported claims only that the export was requested",
      { kind: "requested", filename: "replay.json" },
      "Export requested — replay.json.",
    ],
    [
      "a failed download is not reported as a save",
      { kind: "failed", filename: "replay.json", path: "/home/u/Downloads/replay.json" },
      "Could not export the replay.",
    ],
    ["no recording says so", null, "No replay recording is available for this game."],
  ])("%s", async (_name, result, expected) => {
    downloadCurrentReplay.mockResolvedValue(result);

    expect(await clickExportReplay()).toHaveTextContent(expected);
  });

  it("stays silent when the user cancels the save picker", async () => {
    downloadCurrentReplay.mockRejectedValue(new DOMException("cancelled", "AbortError"));
    render(<HelpSheet />);

    fireEvent.click(screen.getByText("Export Replay"));
    // The rejection settles two microtasks out and React commits on a later
    // task; without this flush the assertion below passes with the AbortError
    // guard deleted.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(downloadCurrentReplay).toHaveBeenCalledOnce();
    expect(document.querySelector("p.text-emerald-300")).toBeNull();
  });
});
