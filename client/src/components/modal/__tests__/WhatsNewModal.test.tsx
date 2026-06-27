import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ChangelogEntry } from "../../../services/changelog";
import { WhatsNewModal } from "../WhatsNewModal";

/** Build N newest-first entries (id N down to 1). Optional per-id overrides let
 * a test give one entry a distinctive title or tag to search for. */
function entries(
  count: number,
  overrides: Record<number, Partial<ChangelogEntry>> = {},
): ChangelogEntry[] {
  return Array.from({ length: count }, (_, i) => {
    const id = count - i;
    return {
      id,
      date: "2026-01-01",
      title: `Update ${id}`,
      tags: [],
      body: `Body for update ${id}`,
      ...overrides[id],
    } satisfies ChangelogEntry;
  });
}

function renderModal(list: ChangelogEntry[]) {
  const onRetry = vi.fn();
  const onClose = vi.fn();
  render(
    <WhatsNewModal
      entries={list}
      loading={false}
      failed={false}
      onRetry={onRetry}
      onClose={onClose}
    />,
  );
}

afterEach(() => {
  cleanup();
});

describe("WhatsNewModal", () => {
  it("windows entries into pages of 10 and steps through them", () => {
    renderModal(entries(12));

    // Page 1: newest 10 (ids 12..3). The two oldest are off-page.
    expect(screen.getByText("Update 12")).toBeInTheDocument();
    expect(screen.getByText("Update 3")).toBeInTheDocument();
    expect(screen.queryByText("Update 2")).not.toBeInTheDocument();
    expect(screen.getByText("Page 1 of 2")).toBeInTheDocument();

    // Prev is disabled at the start; Next advances to the final page.
    expect(screen.getByRole("button", { name: "Previous" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Next" }));

    expect(screen.getByText("Update 2")).toBeInTheDocument();
    expect(screen.getByText("Update 1")).toBeInTheDocument();
    expect(screen.queryByText("Update 12")).not.toBeInTheDocument();
    expect(screen.getByText("Page 2 of 2")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
  });

  it("omits paging controls when a single page suffices", () => {
    renderModal(entries(3));
    expect(screen.queryByText(/Page \d+ of \d+/)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Next" })).not.toBeInTheDocument();
  });

  it("filters by title text and resets paging to the first page", () => {
    renderModal(entries(12, { 5: { title: "Sliver overlord rework" } }));

    // Start on page 2 so we can prove search snaps back to page 1.
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "sliver" },
    });

    expect(screen.getByText("Sliver overlord rework")).toBeInTheDocument();
    expect(screen.queryByText("Update 12")).not.toBeInTheDocument();
    // Single result → no paging row.
    expect(screen.queryByText(/Page \d+ of \d+/)).not.toBeInTheDocument();
  });

  it("matches against translated tag labels, not just body text", () => {
    renderModal(entries(3, { 2: { tags: ["new-cards"] } }));

    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "new cards" },
    });

    // Only the new-cards-tagged entry survives the tag-label match.
    expect(screen.getByText("Update 2")).toBeInTheDocument();
    expect(screen.queryByText("Update 3")).not.toBeInTheDocument();
  });

  it("shows a no-results message when nothing matches", () => {
    renderModal(entries(5));
    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "zzzznomatch" },
    });
    expect(
      screen.getByText("No updates match your search."),
    ).toBeInTheDocument();
  });
});
