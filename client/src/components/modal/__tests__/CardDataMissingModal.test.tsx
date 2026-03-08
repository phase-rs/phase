import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { CardDataMissingModal } from "../CardDataMissingModal";

describe("CardDataMissingModal", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders modal with title and generation command", () => {
    render(<CardDataMissingModal onContinue={vi.fn()} />);

    expect(screen.getByText("Card Data Missing")).toBeInTheDocument();
    expect(screen.getAllByText(/card-data\.json/)).toHaveLength(2);
  });

  it("shows cargo run --bin card_data_export command", () => {
    render(<CardDataMissingModal onContinue={vi.fn()} />);

    expect(screen.getByText(/cargo run --bin card_data_export/)).toBeInTheDocument();
  });

  it("Continue anyway button calls onContinue", () => {
    const onContinue = vi.fn();
    render(<CardDataMissingModal onContinue={onContinue} />);

    fireEvent.click(screen.getByText("Continue anyway"));

    expect(onContinue).toHaveBeenCalledTimes(1);
  });
});
