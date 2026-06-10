import type { SelectHTMLAttributes } from "react";

const SIZE_STYLES = {
  sm: {
    select: "pr-7",
    iconWrapper: "pr-2",
    icon: "h-3 w-3",
  },
  md: {
    select: "pr-9",
    iconWrapper: "pr-3",
    icon: "h-4 w-4",
  },
} as const;

function ChevronDownIcon({ className }: { className: string }) {
  return (
    <svg aria-hidden="true" viewBox="0 0 20 20" className={`fill-current ${className}`}>
      <path d="M5.47 7.97a.75.75 0 0 1 1.06 0L10 11.44l3.47-3.47a.75.75 0 1 1 1.06 1.06l-4 4a.75.75 0 0 1-1.06 0l-4-4a.75.75 0 0 1 0-1.06Z" />
    </svg>
  );
}

export interface SelectFieldProps extends SelectHTMLAttributes<HTMLSelectElement> {
  /** Class on the outer relative wrapper (e.g. `w-full`, `flex-1 min-w-0`). */
  wrapperClassName?: string;
  /** Chevron color/styling on the icon container. */
  iconWrapperClassName?: string;
  /** Chevron size preset — `sm` for compact toolbars, `md` (default) for forms. */
  chevronSize?: keyof typeof SIZE_STYLES;
}

export function SelectField({
  className = "",
  wrapperClassName = "",
  iconWrapperClassName = "text-white/70",
  chevronSize = "md",
  disabled,
  children,
  ...props
}: SelectFieldProps) {
  const sizeStyle = SIZE_STYLES[chevronSize];

  return (
    <div className={`relative ${wrapperClassName}`.trim()}>
      <select
        {...props}
        disabled={disabled}
        className={["appearance-none", sizeStyle.select, className].filter(Boolean).join(" ")}
      >
        {children}
      </select>
      <div
        aria-hidden="true"
        className={[
          "pointer-events-none absolute inset-y-0 right-0 flex items-center",
          sizeStyle.iconWrapper,
          disabled ? "opacity-50" : "",
          iconWrapperClassName,
        ]
          .filter(Boolean)
          .join(" ")}
      >
        <ChevronDownIcon className={sizeStyle.icon} />
      </div>
    </div>
  );
}
