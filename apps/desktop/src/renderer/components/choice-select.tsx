import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "./ui/select";

type Choice = { value: string; label: string; disabled?: boolean };

/** Shared Select composition for a flat list of choices. */
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
  return <Select items={options} value={value} disabled={disabled}
    onValueChange={(next) => { if (next !== null) onChange(next); }}>
    <SelectTrigger id={id} aria-label={label} aria-describedby={describedBy} size={size} className={className}>
      <SelectValue />
    </SelectTrigger>
    <SelectContent>
      <SelectGroup>{options.map((option) => <SelectItem key={option.value} value={option.value} disabled={option.disabled}>{option.label}</SelectItem>)}</SelectGroup>
    </SelectContent>
  </Select>;
}
