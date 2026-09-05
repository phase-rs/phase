import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { STORAGE_KEY_PREFIX } from "../../../constants/storage";
import {
  isCardCommanderEligible,
  isCardCommanderEligibleForFormat,
  signatureSpellSelectionPolicy,
} from "../../../services/engineRuntime";
import { useAppNotificationStore } from "../../../stores/appToastStore";
import { useConnectivityStore } from "../../../stores/connectivityStore";
import { ImportDeckModal } from "../ImportDeckModal";

const mocks = vi.hoisted(() => ({
  fetchDeckFromUrl: vi.fn(),
}));

vi.mock("../../../services/engineRuntime", () => ({
  isCardCommanderEligible: vi.fn(),
  isCardCommanderEligibleForFormat: vi.fn(),
  signatureSpellSelectionPolicy: vi.fn(),
}));

vi.mock("../../../services/deckUrlImport", () => ({
  fetchDeckFromUrl: mocks.fetchDeckFromUrl,
}));

describe("ImportDeckModal", () => {
  beforeEach(() => {
    localStorage.clear();
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
    vi.mocked(isCardCommanderEligible).mockResolvedValue(false);
    vi.mocked(isCardCommanderEligibleForFormat).mockReset();
    vi.mocked(signatureSpellSelectionPolicy).mockReset();
    mocks.fetchDeckFromUrl.mockReset();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  afterEach(() => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    vi.unstubAllGlobals();
  });

  it("keeps URL import visible but unavailable offline without calling the service", async () => {
    const user = userEvent.setup();
    render(<ImportDeckModal open onClose={vi.fn()} onImported={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "From URL" }));
    const urlInput = screen.getByPlaceholderText(/moxfield\.com\/decks/i);
    const importButton = screen.getByRole("button", { name: "Import" });
    await user.type(urlInput, "https://moxfield.com/decks/abc");
    expect(importButton).toBeEnabled();

    act(() => useConnectivityStore.getState().setForcedOffline(true));

    expect(urlInput).toBeDisabled();
    expect(screen.getByText(/URL imports are unavailable offline/i)).toBeInTheDocument();
    expect(importButton).toBeDisabled();
    await user.click(importButton);
    expect(mocks.fetchDeckFromUrl).not.toHaveBeenCalled();
  });

  it("imports canonical deck text from a URL online", async () => {
    const user = userEvent.setup();
    const onImported = vi.fn();
    mocks.fetchDeckFromUrl.mockResolvedValue(
      "Name: URL Deck\n[Main]\n1 Sol Ring\n",
    );
    render(<ImportDeckModal open onClose={vi.fn()} onImported={onImported} />);

    await user.click(screen.getByRole("button", { name: "From URL" }));
    await user.type(screen.getByPlaceholderText(/moxfield\.com\/decks/i), "https://moxfield.com/decks/abc");
    await user.click(screen.getByRole("button", { name: "Import" }));

    await waitFor(() => expect(onImported).toHaveBeenCalledWith("URL Deck", ["URL Deck"]));
    expect(mocks.fetchDeckFromUrl).toHaveBeenCalledWith("https://moxfield.com/decks/abc");
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "URL Deck")).not.toBeNull();
  });

  it("keeps pasted and file deck imports local while offline", async () => {
    const user = userEvent.setup();
    const onImported = vi.fn();
    class OfflineFileReader {
      result = "Name: File Deck\n[Main]\n1 Sol Ring\n";
      onload: (() => void) | null = null;

      readAsText() {
        this.onload?.();
      }
    }
    vi.stubGlobal("FileReader", OfflineFileReader);
    useConnectivityStore.getState().setForcedOffline(true);
    render(<ImportDeckModal open onClose={vi.fn()} onImported={onImported} />);

    await user.type(screen.getByPlaceholderText(/Paste deck list here/i), "Name: Paste Deck\n[Main]\n1 Sol Ring");
    await user.click(screen.getByRole("button", { name: "Import" }));
    await waitFor(() => expect(onImported).toHaveBeenCalledWith("Paste Deck", ["Paste Deck"]));

    cleanup();
    render(<ImportDeckModal open onClose={vi.fn()} onImported={onImported} />);
    await user.click(screen.getByRole("button", { name: "From File" }));
    const fileInput = document.querySelector('input[type="file"]') as HTMLInputElement;
    await user.upload(fileInput, new File([""], "file-deck.txt", { type: "text/plain" }));

    await waitFor(() => expect(onImported).toHaveBeenCalledWith("File Deck", ["File Deck", "Paste Deck"]));
    expect(mocks.fetchDeckFromUrl).not.toHaveBeenCalled();
  });

  it("derives the saved deck name from pasted metadata when the name field is empty", async () => {
    const onImported = vi.fn();
    render(
      <ImportDeckModal
        open
        onClose={vi.fn()}
        onImported={onImported}
      />,
    );

    await userEvent.type(
      screen.getByPlaceholderText(/Paste deck list here/i),
      `About
Name Lagomos Sacrifice Pauper Duel Commander

Commander
1x Lagomos, Hand of Hatred (DMU) 205

Deck
1x Abrade (VOW) 139`,
    );
    await userEvent.click(screen.getByRole("button", { name: "Import" }));

    await waitFor(() => {
      expect(onImported).toHaveBeenCalledWith(
        "Lagomos Sacrifice Pauper Duel Commander",
        ["Lagomos Sacrifice Pauper Duel Commander"],
      );
    });
    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Deck imported",
      description: '"Lagomos Sacrifice Pauper Duel Commander" was added to your decks.',
    });
    expect(localStorage.getItem(
      STORAGE_KEY_PREFIX + "Lagomos Sacrifice Pauper Duel Commander",
    )).not.toBeNull();
  });

  it("shows an error when pasted text contains no recognizable cards", async () => {
    const onImported = vi.fn();
    render(
      <ImportDeckModal
        open
        onClose={vi.fn()}
        onImported={onImported}
      />,
    );

    await userEvent.type(
      screen.getByPlaceholderText(/Paste deck list here/i),
      "asdasd",
    );
    await userEvent.click(screen.getByRole("button", { name: "Import" }));

    expect(
      await screen.findByText(/couldn't find any cards/i),
    ).toBeInTheDocument();
    expect(onImported).not.toHaveBeenCalled();
  });

  it("lets the importer assign Oathbreaker and signature-spell slots", async () => {
    const user = userEvent.setup();
    const onImported = vi.fn();
    vi.mocked(isCardCommanderEligibleForFormat).mockImplementation(
      async (name) => name === "Daretti, Ingenious Iconoclast",
    );
    vi.mocked(signatureSpellSelectionPolicy).mockResolvedValue({
      type: "Required",
      data: { candidates: ["Scheming Symmetry", "Temporal Extortion"] },
    });
    render(
      <ImportDeckModal
        open
        onClose={vi.fn()}
        onImported={onImported}
      />,
    );

    await user.type(screen.getByPlaceholderText("Deck name"), "Daretti's Mirrors");
    await user.type(
      screen.getByPlaceholderText(/Paste deck list here/i),
      `Deck
1 Daretti, Ingenious Iconoclast
1 Scheming Symmetry
1 Temporal Extortion`,
    );
    await user.click(screen.getByLabelText("Set up as an Oathbreaker deck"));
    await user.click(screen.getByRole("button", { name: "Import" }));

    expect(await screen.findByLabelText("Oathbreaker")).toHaveValue(
      "Daretti, Ingenious Iconoclast",
    );
    await user.selectOptions(screen.getByLabelText("Signature spell"), "Scheming Symmetry");
    await user.click(screen.getByRole("button", { name: "Import Oathbreaker Deck" }));

    await waitFor(() => {
      expect(onImported).toHaveBeenCalledWith("Daretti's Mirrors", ["Daretti's Mirrors"]);
    });
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Daretti's Mirrors") ?? "{}")).toMatchObject({
      main: [{ count: 1, name: "Temporal Extortion" }],
      commander: ["Daretti, Ingenious Iconoclast"],
      signature_spell: ["Scheming Symmetry"],
      format: "Oathbreaker",
    });
  });
});
