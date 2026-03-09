import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { FloatingNumber } from "../FloatingNumber";

vi.mock("framer-motion", () => ({
  motion: {
    div: ({
      children,
      onAnimationComplete,
      ...props
    }: {
      children: React.ReactNode;
      onAnimationComplete?: () => void;
      style?: React.CSSProperties;
      initial?: Record<string, number>;
      animate?: Record<string, number>;
      transition?: Record<string, number>;
    }) => (
      <div
        data-testid="floating-number"
        data-transition-duration={props.transition?.duration}
        style={props.style}
        onClick={onAnimationComplete}
      >
        {children}
      </div>
    ),
  },
}));

describe("FloatingNumber", () => {
  const baseProps = {
    value: -3,
    position: { x: 100, y: 200 },
    color: "#ef4444",
    onComplete: vi.fn(),
  };

  afterEach(() => {
    cleanup();
  });

  it("renders with the value text", () => {
    render(<FloatingNumber {...baseProps} />);
    expect(screen.getByTestId("floating-number")).toHaveTextContent("-3");
  });

  it("formats positive values with a + prefix", () => {
    render(<FloatingNumber {...baseProps} value={5} />);
    expect(screen.getByTestId("floating-number")).toHaveTextContent("+5");
  });

  it("uses default speedMultiplier of 1.0", () => {
    render(<FloatingNumber {...baseProps} />);
    const el = screen.getByTestId("floating-number");
    expect(el.dataset.transitionDuration).toBe("0.8");
  });

  it("applies custom speedMultiplier to duration", () => {
    render(<FloatingNumber {...baseProps} speedMultiplier={0.5} />);
    const el = screen.getByTestId("floating-number");
    expect(el.dataset.transitionDuration).toBe("0.4");
  });

  it("calls onComplete when animation finishes", () => {
    const onComplete = vi.fn();
    render(<FloatingNumber {...baseProps} onComplete={onComplete} />);
    screen.getByTestId("floating-number").click();
    expect(onComplete).toHaveBeenCalledOnce();
  });
});
