import type { ComponentProps } from "react";
import { Button } from "./ui/button";
import { Switch } from "./ui/switch";
import { Hint } from "./hint";

type IconButtonProps = Omit<ComponentProps<typeof Button>, "aria-label" | "size" | "className"> & { label: string; size?: "icon" | "icon-sm" | "icon-xs" };

export function IconButton({ label, variant = "outline", size = "icon", ...props }: IconButtonProps): React.JSX.Element {
  return <Hint content={label}><Button type="button" variant={variant} size={size} aria-label={label} {...props} /></Hint>;
}

type SwitchControlProps = {
  id?: string;
  label: string;
  checked: boolean;
  disabled?: boolean;
  size?: "sm" | "default";
  "aria-describedby"?: string;
  "aria-busy"?: boolean;
  onToggle(): void;
};

export function SwitchControl({ label, onToggle, ...props }: SwitchControlProps): React.JSX.Element {
  return <Switch aria-label={label} onCheckedChange={onToggle} {...props} />;
}
