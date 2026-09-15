import { useRef, useState } from "react";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "./ui/select";

type Choice = { value: string; label: string; disabled?: boolean };

/** Shared Select composition, including portal ownership inside native dialog content. */
export function ChoiceSelect({ id, label, describedBy, value, options, disabled, size, className, onChange }: {
  id?: string;
  label: string;
  describedBy?: string;
  value: string;
  options: Choice[];
  disabled?: boolean;
  size?: "sm" | "default";
  className?: string;
  onChange(value: string): void;
}) {
  const trigger = useRef<HTMLButtonElement>(null);
  const [container, setContainer] = useState<HTMLDialogElement | null>(null);
  return <Select items={options} value={value} disabled={disabled}
    onValueChange={(next) => { if (next !== null) onChange(next); }}
    onOpenChange={(open) => { if (open) setContainer(trigger.current?.closest("dialog") ?? null); }}>
    <SelectTrigger ref={trigger} id={id} aria-label={label} aria-describedby={describedBy} size={size} className={className}>
      <SelectValue />
    </SelectTrigger>
    <SelectContent container={container ?? undefined} alignItemWithTrigger={container === null}>
      <SelectGroup>{options.map((option) => <SelectItem key={option.value} value={option.value} disabled={option.disabled}>{option.label}</SelectItem>)}</SelectGroup>
    </SelectContent>
  </Select>;
}
