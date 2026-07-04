import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { STORAGE_KEY_PREFIX } from "../../../constants/storage";
import { useAppNotificationStore } from "../../../stores/appToastStore";
import { ImportDeckModal } from "../ImportDeckModal";

describe("ImportDeckModal", () => {
  beforeEach(() => {
    localStorage.clear();
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
  });

  afterEach(() => {
    cleanup();
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

    await waitFor(() => {
      expect(screen.getByText(/couldn't find any cards/i)).toBeInTheDocument();
    });
    expect(onImported).not.toHaveBeenCalled();
  });
});
