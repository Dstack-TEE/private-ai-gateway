import type { ComponentProps } from "react";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";
import { Switch } from "./ui/switch";
import { Hint } from "./hint";

type IconButtonProps = Omit<ComponentProps<typeof Button>, "aria-label" | "size" | "className"> & { label: string; className?: string; size?: "icon" | "icon-sm" | "icon-xs" };

export function IconButton({ label, className, variant = "outline", size = "icon", ...props }: IconButtonProps): React.JSX.Element {
  return <Hint content={label}><Button type="button" variant={variant} size={size} className={className} aria-label={label} {...props} /></Hint>;
}

type SwitchControlProps = {
  id?: string;
  className?: string;
  label: string;
  checked: boolean;
  disabled?: boolean;
  developmentMode?: boolean;
  size?: "sm" | "default";
  "aria-describedby"?: string;
  "aria-busy"?: boolean;
  onToggle(): void;
};

export function SwitchControl({ label, developmentMode = false, className, onToggle, ...props }: SwitchControlProps): React.JSX.Element {
  return <Switch className={cn(developmentMode && "data-checked:border-warning data-checked:bg-warning group-has-[:focus-visible]/field-label:data-checked:border-warning", className)} aria-label={label} onCheckedChange={onToggle} {...props} />;
}
