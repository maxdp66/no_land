import clsx from "clsx";
import type { ButtonHTMLAttributes } from "react";
import { playArcadeClick } from "../../lib/arcadeAudio";

type Variant = "primary" | "secondary" | "ghost" | "danger";

interface Props extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: "default" | "compact";
  loading?: boolean;
  loadingText?: string;
}

const variantClasses: Record<Variant, string> = {
  primary:
    "border-[#61f7ff] bg-[#1b2f4d] text-[#7cf8ff] shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#2f5f86,0_0_20px_rgba(68,214,255,0.25)] hover:bg-[#22466e] hover:text-white",
  secondary:
    "border-[#7bff48] bg-[#1d3620] text-[#b4ff88] shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#366230] hover:bg-[#2b4f28]",
  ghost:
    "border-[#495188] bg-transparent text-[#b9caf0] shadow-[0_0_0_2px_#090a17,inset_0_0_0_1px_#11152f] hover:border-[#61f7ff] hover:text-[#84fbff]",
  danger:
    "border-[#ff8ca2] bg-[#4b1f2f] text-[#ffc1cf] shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#6f2c45] hover:bg-[#673149]"
};

export function Button({
  variant = "primary",
  size = "default",
  className,
  loading = false,
  loadingText,
  children,
  ...props
}: Props) {
  const { onClick, disabled, ...rest } = props;
  const resolvedLoadingText =
    loadingText ?? (typeof children === "string" ? children : "Working...");

  return (
    <button
      className={clsx(
        "relative inline-flex min-h-10 items-center justify-center border font-display uppercase tracking-[0.08em] transition duration-100 focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-[#61f7ff] focus-visible:ring-offset-2 focus-visible:ring-offset-[#05050c] active:translate-y-px disabled:cursor-not-allowed disabled:opacity-50",
        size === "compact"
          ? "px-2 py-2 text-[9px] leading-normal"
          : "px-3 py-2 text-[11px] leading-normal",
        variantClasses[variant],
        className
      )}
      onClick={(event) => {
        if (!disabled && !loading) {
          playArcadeClick();
        }
        onClick?.(event);
      }}
      disabled={disabled || loading}
      {...rest}
    >
      <span className={clsx("inline-flex min-w-0 max-w-full flex-wrap items-center justify-center gap-1.5", loading && "opacity-0")}>
        {children}
      </span>

      {loading && (
        <span className="absolute inset-0 flex flex-wrap items-center justify-center gap-1.5 px-2 text-center">
          <span
            aria-hidden="true"
            className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-current border-t-transparent"
          />
          <span>{resolvedLoadingText}</span>
        </span>
      )}
    </button>
  );
}
