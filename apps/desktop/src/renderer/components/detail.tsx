import React from "react";
import { cn } from "../lib/utils";

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
    <div className={cn("grid min-w-0 gap-0.5", wide && "col-span-full")}>
      <span className="text-xs text-muted-foreground">{label}</span>
      <strong className={cn("font-semibold select-text", mono && "font-mono text-xs font-medium wrap-anywhere")}>{value}</strong>
    </div>
  );
}

export function EmptyState({ text }: { text: string }): React.JSX.Element {
  return <div className="grid min-h-18 place-items-center p-3 text-center text-xs text-muted-foreground">{text}</div>;
}
