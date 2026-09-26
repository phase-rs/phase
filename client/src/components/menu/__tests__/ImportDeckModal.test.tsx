import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { STORAGE_KEY_PREFIX, listSavedDeckNames, writeSavedDeckData } from "../../../constants/storage";
import {
  isCardCommanderEligible,
  isCardCommanderEligibleForFormat,
  signatureSpellSelectionPolicy,
} from "../../../services/engineRuntime";
import { setSavedDeckTxnLockWaitForTests, withSavedDeckLibrary } from "../../../services/savedDeckTransaction";
import { useAppNotificationStore } from "../../../stores/appToastStore";
import { useConnectivityStore } from "../../../stores/connectivityStore";
import {
  installFifoWebLocks,
  resetSavedDeckLibraryForTests,
  uninstallWebLocks,
} from "../../../test/helpers/webLocks";
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

    await waitFor(() => expect(onImported).toHaveBeenCalledWith("URL Deck", ["URL Deck"], "open"));
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
    await waitFor(() => expect(onImported).toHaveBeenCalledWith("Paste Deck", ["Paste Deck"], "open"));

    cleanup();
    render(<ImportDeckModal open onClose={vi.fn()} onImported={onImported} />);
    await user.click(screen.getByRole("button", { name: "From File" }));
    const fileInput = document.querySelector('input[type="file"]') as HTMLInputElement;
    await user.upload(fileInput, new File([""], "file-deck.txt", { type: "text/plain" }));

    await waitFor(() => expect(onImported).toHaveBeenCalledWith("File Deck", ["File Deck", "Paste Deck"], "open"));
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
        "open",
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
      expect(onImported).toHaveBeenCalledWith("Daretti's Mirrors", ["Daretti's Mirrors"], "open");
    });
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Daretti's Mirrors") ?? "{}")).toMatchObject({
      main: [{ count: 1, name: "Temporal Extortion" }],
      commander: ["Daretti, Ingenious Iconoclast"],
      signature_spell: ["Scheming Symmetry"],
      format: "Oathbreaker",
    });
  });

  describe("cross-tab saved-deck transactions", () => {
    beforeEach(async () => {
      installFifoWebLocks();
      await resetSavedDeckLibraryForTests();
    });

    afterEach(() => {
      uninstallWebLocks();
      setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
    });

    it("a deck another writer creates under the chosen name while the import waits is not overwritten", async () => {
      const user = userEvent.setup();
      const onImported = vi.fn();
      let releaseHeld!: () => void;
      const held = new Promise<void>((resolve) => {
        releaseHeld = resolve;
      });
      const holder = withSavedDeckLibrary(async (txn) => {
        await held;
        writeSavedDeckData(txn, "Probe Deck", "OTHER-TAB");
      });
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(1);
      });

      render(<ImportDeckModal open onClose={vi.fn()} onImported={onImported} />);
      await user.type(
        screen.getByPlaceholderText(/Paste deck list here/i),
        "About\nName Probe Deck\n\nDeck\n1x Abrade (VOW) 139",
      );
      await user.click(screen.getByRole("button", { name: "Import" }));
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });
      releaseHeld();
      await holder;

      await waitFor(() => expect(onImported).toHaveBeenCalledWith("Probe Deck 2", ["Probe Deck", "Probe Deck 2"], "open"));
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Probe Deck")).toBe("OTHER-TAB");
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Probe Deck 2") ?? "{}").main).toEqual(
        expect.arrayContaining([expect.objectContaining({ name: "Abrade" })]),
      );
    });

    it("an import refused by a busy library keeps the modal open and writes nothing", async () => {
      const user = userEvent.setup();
      const onImported = vi.fn();
      let releaseHolder!: () => void;
      const held = new Promise<void>((resolve) => {
        releaseHolder = resolve;
      });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(1);
      });

      render(<ImportDeckModal open onClose={vi.fn()} onImported={onImported} />);
      const pasteText = "Name: Paste Deck\n[Main]\n1 Sol Ring";
      fireEvent.change(screen.getByPlaceholderText(/Paste deck list here/i), { target: { value: pasteText } });
      setSavedDeckTxnLockWaitForTests(50);
      await user.click(screen.getByRole("button", { name: "Import" }));

      await vi.waitFor(() => {
        expect(useAppNotificationStore.getState().notification?.title).toBe("Couldn't import deck");
      });
      expect(onImported).not.toHaveBeenCalled();
      expect(screen.getByPlaceholderText(/Paste deck list here/i)).toHaveValue(pasteText);
      expect(listSavedDeckNames()).toEqual([]);

      setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
      releaseHolder();
      await holder;
    });
  });

  describe("closing and reopening while an import waits", () => {
    beforeEach(async () => {
      installFifoWebLocks();
      await resetSavedDeckLibraryForTests();
    });

    afterEach(() => {
      uninstallWebLocks();
    });

    function Harness({
      onImported,
      onClose,
    }: {
      onImported: (name: string, deckNames: string[], session: "open" | "dismissed") => void;
      onClose?: () => void;
    }) {
      const [open, setOpen] = useState(true);
      return (
        <>
          <button type="button" onClick={() => setOpen(true)}>reopen</button>
          <ImportDeckModal open={open} onClose={() => { onClose?.(); setOpen(false); }} onImported={onImported} />
        </>
      );
    }

    it("an import that finishes after its host unmounts (without closing the modal first) is reported dismissed", async () => {
      const user = userEvent.setup();
      const onImported = vi.fn();
      let releaseHolder!: () => void;
      const held = new Promise<void>((resolve) => {
        releaseHolder = resolve;
      });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(1);
      });

      const r = render(<Harness onImported={onImported} />);
      await user.type(
        screen.getByPlaceholderText(/Paste deck list here/i),
        "Name: Paste Deck\n[Main]\n1 Sol Ring",
      );
      await user.click(screen.getByRole("button", { name: "Import" }));
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });

      r.unmount();
      releaseHolder();
      await holder;

      await waitFor(() => expect(onImported).toHaveBeenCalledWith("Paste Deck", ["Paste Deck"], "dismissed"));
    });

    it("an import that finishes after the modal was closed and reopened leaves the reopened modal open", async () => {
      const user = userEvent.setup();
      const onImported = vi.fn();
      const onClose = vi.fn();
      let releaseHolder!: () => void;
      const held = new Promise<void>((resolve) => {
        releaseHolder = resolve;
      });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(1);
      });

      render(<Harness onImported={onImported} onClose={onClose} />);
      await user.type(
        screen.getByPlaceholderText(/Paste deck list here/i),
        "Name: Paste Deck\n[Main]\n1 Sol Ring",
      );
      await user.click(screen.getByRole("button", { name: "Import" }));
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });

      await user.click(screen.getByRole("button", { name: /cancel/i }));
      await user.click(screen.getByRole("button", { name: "reopen" }));
      await user.type(screen.getByPlaceholderText(/Paste deck list here/i), "second session");

      releaseHolder();
      await holder;
      await waitFor(() => expect(onImported).toHaveBeenCalledWith("Paste Deck", ["Paste Deck"], "dismissed"));
      expect(onClose).toHaveBeenCalledTimes(1);
      expect(screen.getByPlaceholderText(/Paste deck list here/i)).toHaveValue("second session");
    });

    it("a URL import dismissed while its fetch is pending leaves the reopened modal open", async () => {
      const user = userEvent.setup();
      const onImported = vi.fn();
      const onClose = vi.fn();
      let releaseFetch!: (content: string) => void;
      mocks.fetchDeckFromUrl.mockImplementation(
        () => new Promise<string>((resolve) => { releaseFetch = resolve; }),
      );

      render(<Harness onImported={onImported} onClose={onClose} />);
      await user.click(screen.getByRole("button", { name: "From URL" }));
      await user.type(screen.getByPlaceholderText(/moxfield\.com\/decks/i), "https://moxfield.com/decks/abc");
      await user.click(screen.getByRole("button", { name: "Import" }));
      await vi.waitFor(() => expect(mocks.fetchDeckFromUrl).toHaveBeenCalled());

      await user.click(screen.getByRole("button", { name: /cancel/i }));
      await user.click(screen.getByRole("button", { name: "reopen" }));

      releaseFetch("Name: URL Deck\n[Main]\n1 Sol Ring\n");
      await waitFor(() =>
        expect(onImported).toHaveBeenCalledWith("URL Deck", ["URL Deck"], "dismissed"),
      );
      expect(onClose).toHaveBeenCalledTimes(1);
      expect(screen.queryByPlaceholderText(/Paste deck list here/i)).not.toBeNull();
    });

    it("a file import dismissed while its read is pending leaves the reopened modal open", async () => {
      const readers: Array<{ onload: (() => void) | null; result: string }> = [];
      class DeferredReader {
        result = "Name: File Deck\n[Main]\n1 Sol Ring\n";
        onload: (() => void) | null = null;
        readAsText() {
          readers.push(this);
        }
      }
      vi.stubGlobal("FileReader", DeferredReader);
      const user = userEvent.setup();
      const onImported = vi.fn();
      const onClose = vi.fn();

      render(<Harness onImported={onImported} onClose={onClose} />);
      await user.click(screen.getByRole("button", { name: "From File" }));
      const fileInput = document.querySelector('input[type="file"]') as HTMLInputElement;
      await user.upload(fileInput, new File([""], "file-deck.txt", { type: "text/plain" }));
      expect(readers).toHaveLength(1);
      expect(onImported).not.toHaveBeenCalled();

      await user.click(screen.getByRole("button", { name: /cancel/i }));
      await user.click(screen.getByRole("button", { name: "reopen" }));
      readers[0].onload?.();

      await waitFor(() => expect(onImported).toHaveBeenCalledWith("File Deck", ["File Deck"], "dismissed"));
      expect(onClose).toHaveBeenCalledTimes(1);
      expect(screen.queryByPlaceholderText(/Paste deck list here/i)).not.toBeNull();
    });

    it("an Oathbreaker setup staged after the modal was dismissed does not open on the reopened modal", async () => {
      const user = userEvent.setup();
      const onImported = vi.fn();
      const resolvers: Array<(v: boolean) => void> = [];
      vi.mocked(isCardCommanderEligibleForFormat).mockImplementation(
        () => new Promise<boolean>((r) => { resolvers.push(r); }),
      );
      vi.mocked(signatureSpellSelectionPolicy).mockResolvedValue({
        type: "Required",
        data: { candidates: ["Scheming Symmetry"] },
      });

      render(<Harness onImported={onImported} />);
      await user.click(screen.getByLabelText("Set up as an Oathbreaker deck"));
      await user.type(
        screen.getByPlaceholderText(/Paste deck list here/i),
        "Deck\n1 Daretti, Ingenious Iconoclast",
      );
      await user.click(screen.getByRole("button", { name: "Import" }));
      await vi.waitFor(() => expect(resolvers.length).toBeGreaterThan(0));

      await user.click(screen.getByRole("button", { name: /cancel/i }));
      await user.click(screen.getByRole("button", { name: "reopen" }));
      await act(async () => {
        resolvers.forEach((r) => r(true));
      });

      expect(screen.queryByLabelText("Oathbreaker")).not.toBeInTheDocument();
      expect(screen.getByPlaceholderText(/Paste deck list here/i)).toBeInTheDocument();
      expect(signatureSpellSelectionPolicy).not.toHaveBeenCalled();
    });
  });
});
