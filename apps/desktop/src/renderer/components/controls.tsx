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
  tone?: "default" | "success";
  size?: "sm" | "default" | "lg";
  title?: string;
  "aria-describedby"?: string;
  "aria-busy"?: boolean;
  onToggle(): void;
};

export function SwitchControl({ label, developmentMode = false, tone = "default", onToggle, title, ...props }: SwitchControlProps): React.JSX.Element {
  return <Hint content={title ?? label}><Switch className={developmentMode ? "is-development" : tone === "success" ? "is-success" : undefined} aria-label={label} onCheckedChange={onToggle} {...props} /></Hint>;
}
