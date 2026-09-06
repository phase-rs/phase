import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChoiceModal, type ChoiceOption } from "../ChoiceModal.tsx";

/**
 * CR 118.3 — matrix rows 15 and 16: the ability picker must render a
 * NON-SELECTABLE row for an ability the engine is withholding solely because
 * its cost is unpayable right now, instead of silently omitting it (the
 * reported defect: Sliver Overlord's two printed `{3}:` abilities simply
 * vanished from the picker).
 *
 * Row 15 — the row renders and cannot be chosen.
 * Row 16 — the reason reaches assistive tech: `aria-disabled`, NOT the native
 *          `disabled` attribute, so the row stays in the tab order and its
 *          explanatory `<p>` (rendered INSIDE the `<button>`, with no
 *          `aria-describedby`) still feeds the accessible NAME.
 *
 * Every negative here is paired with a positive in the SAME render: an
 * affordable row that IS selectable and carries no `aria-disabled`. A modal
 * that rendered nothing, or disabled everything, fails.
 */

const AFFORDABLE = "Regenerate target Sliver";
const BLOCKED = "Search your library for a Sliver card";
const REASON = "You can't pay this cost right now";

function options(): ChoiceOption[] {
  return [
    { id: "0", label: AFFORDABLE },
    { id: "blocked:1", label: BLOCKED, description: REASON, disabled: true },
  ];
}

function renderModal(onChoose = vi.fn()) {
  render(
    <ChoiceModal title="Sliver Overlord" options={options()} onChoose={onChoose} />,
  );
  return onChoose;
}

afterEach(cleanup);

describe("ChoiceModal — non-selectable rows (CR 118.3)", () => {
  // Row 15.
  it("renders a non-selectable row per blocked ability and dispatches nothing on click", () => {
    const onChoose = renderModal();

    const blocked = screen.getByRole("button", { name: /Search your library/ });
    const affordable = screen.getByRole("button", { name: /Regenerate target Sliver/ });

    expect(blocked).toHaveAttribute("aria-disabled", "true");
    // PAIRED POSITIVE, mandatory: without it, a modal that disabled every row
    // would satisfy the assertion above.
    expect(affordable).not.toHaveAttribute("aria-disabled");

    fireEvent.click(blocked);
    expect(onChoose).not.toHaveBeenCalled();

    // REACH-GUARD: the same handler in the same render DOES fire for the
    // affordable row, so the zero above is a refusal, not a dead modal.
    fireEvent.click(affordable);
    expect(onChoose).toHaveBeenCalledTimes(1);
    expect(onChoose).toHaveBeenCalledWith("0");
  });

  // Row 16.
  it("keeps the blocked row in the tab order and puts the reason in its accessible name", () => {
    renderModal();

    const blocked = screen.getByRole("button", { name: /Search your library/ });

    // `aria-disabled`, NOT native `disabled`: a native `disabled` button is
    // removed from interactive screen-reader navigation, which would hide the
    // explanation from exactly the users who need it. Asserted directly so the
    // decision is pinned rather than incidental.
    expect(blocked).not.toBeDisabled();
    expect(blocked.hasAttribute("disabled")).toBe(false);

    // The reason renders as a <p> INSIDE the <button> with no
    // `aria-describedby`, so it feeds the accessible NAME.
    expect(within(blocked).getByText(REASON)).toBeInTheDocument();
    expect(blocked).toHaveAccessibleName(expect.stringContaining(REASON) as unknown as string);
  });

  // Guard: the `disabled` field is optional, so every one of the 26 shipped
  // `<ChoiceModal>` call sites keeps working untouched.
  it("leaves rows selectable when `disabled` is omitted entirely", () => {
    const onChoose = vi.fn();
    render(
      <ChoiceModal
        title="Sliver Overlord"
        options={[{ id: "0", label: AFFORDABLE }]}
        onChoose={onChoose}
      />,
    );
    const row = screen.getByRole("button", { name: /Regenerate target Sliver/ });
    expect(row).not.toHaveAttribute("aria-disabled");
    fireEvent.click(row);
    expect(onChoose).toHaveBeenCalledWith("0");
  });
});
