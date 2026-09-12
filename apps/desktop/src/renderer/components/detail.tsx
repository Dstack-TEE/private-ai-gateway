import React from "react";

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
    <div className={wide ? "wide" : undefined}>
      <span>{label}</span>
      <strong className={mono ? "mono font-mono text-xs" : undefined}>{value}</strong>
    </div>
  );
}

export function EmptyState({ text }: { text: string }): React.JSX.Element {
  return <div className="empty-state min-h-18 p-3.25 grid place-items-center text-muted-foreground text-xs text-center">{text}</div>;
}
