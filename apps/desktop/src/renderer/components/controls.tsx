import type { ComponentProps } from "react";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";
import { Switch } from "./ui/switch";
import { Hint } from "./hint";

type IconButtonProps = Omit<ComponentProps<typeof Button>, "aria-label" | "size" | "className"> & { label: string; className?: string; size?: "icon" | "icon-sm" | "icon-xs" };

export function IconButton({ label, className, variant = "outline", size = "icon", ...props }: IconButtonProps): React.JSX.Element {
  return <Hint content={label}><Button type="button" variant={variant} size={size} className={cn("icon-button", className)} aria-label={label} {...props} /></Hint>;
}

type SwitchControlProps = {
  id?: string;
  label: string;
  checked: boolean;
  disabled?: boolean;
  developmentMode?: boolean;
  size?: "sm" | "default" | "lg";
  "aria-describedby"?: string;
  "aria-busy"?: boolean;
  onToggle(): void;
};

export function SwitchControl({ label, developmentMode = false, onToggle, ...props }: SwitchControlProps): React.JSX.Element {
  return <Switch className={developmentMode
    ? "data-checked:border-warning data-checked:bg-warning group-has-[:focus-visible]/field-label:data-checked:border-warning"
    : undefined} aria-label={label} onCheckedChange={onToggle} {...props} />;
}
