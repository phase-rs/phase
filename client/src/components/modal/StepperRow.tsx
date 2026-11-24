import type { ReactNode } from "react";

import type { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";

/** One extra button after the steppers, e.g. "assign the rest here". */
export interface StepperShortcut {
  key: string;
  label: string;
  ariaLabel: string;
  onClick: () => void;
  disabled: boolean;
}

/**
 * One division row: a labelled target with −/value/+ steppers and any
 * shortcuts the caller supplies. Presentation only — every label, disabled
 * state and callback comes from the caller, so the row holds no arithmetic
 * and no rules.
 */
export function StepperRow({
  children,
  value,
  valueClassName,
  ringClassName,
  onDecrement,
  onIncrement,
  decrementDisabled,
  incrementDisabled,
  decrementLabel,
  incrementLabel,
  shortcuts,
  hoverProps,
}: {
  children: ReactNode;
  value: number;
  valueClassName?: string;
  ringClassName?: string;
  onDecrement: () => void;
  onIncrement: () => void;
  decrementDisabled: boolean;
  incrementDisabled: boolean;
  decrementLabel: string;
  incrementLabel: string;
  shortcuts?: readonly StepperShortcut[];
  hoverProps?: ReturnType<ReturnType<typeof useInspectHoverProps>>;
}) {
  // 44px touch target: `!` overrides the xs size's own min height.
  const buttonClass = gameButtonClass({
    tone: "neutral",
    size: "xs",
    className: "!min-h-11 min-w-11",
  });
  return (
    <div
      {...hoverProps}
      className={`flex flex-wrap items-center justify-between gap-x-3 gap-y-2 rounded-lg bg-gray-800/60 p-3${ringClassName ? ` ${ringClassName}` : ""}`}
    >
      <div className="flex items-center gap-2">{children}</div>
      <div className="ml-auto flex flex-wrap items-center justify-end gap-2">
        <button className={buttonClass} onClick={onDecrement} disabled={decrementDisabled}>
          {decrementLabel}
        </button>
        <span className={`w-8 text-center text-sm font-bold ${valueClassName ?? "text-white"}`}>
          {value}
        </span>
        <button className={buttonClass} onClick={onIncrement} disabled={incrementDisabled}>
          {incrementLabel}
        </button>
        {shortcuts?.map(({ key, label, ariaLabel, onClick, disabled }) => (
          <button
            key={key}
            className={buttonClass}
            onClick={onClick}
            disabled={disabled}
            aria-label={ariaLabel}
            title={ariaLabel}
          >
            {label}
          </button>
        ))}
      </div>
    </div>
  );
}
