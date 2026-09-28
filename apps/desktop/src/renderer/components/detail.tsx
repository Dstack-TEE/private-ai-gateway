import React from "react";
import { cn } from "../lib/utils";

/** A labelled value in a two-column grid; `wide` spans both columns. */
export function Detail({
  label,
  value,
  mono = false,
  wide = false,
}: {
  label: string;
  value: string;
  mono?: boolean;
  wide?: boolean;
}): React.JSX.Element {
  return (
    <div className={cn("min-w-0", wide && "col-span-full")}>
      <span className="mb-0.5 block text-xs text-muted-foreground">{label}</span>
      <strong className={cn("block select-text", mono ? "font-mono text-xs font-medium wrap-anywhere" : "font-semibold")}>{value}</strong>
    </div>
  );
}

export function EmptyState({ text, className }: { text: string; className?: string }): React.JSX.Element {
  return <div className={cn("grid min-h-18 place-items-center p-3.25 text-center text-xs text-muted-foreground", className)}>{text}</div>;
}
