import type { ComponentProps } from "react";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";
import { Switch } from "./ui/switch";

type IconButtonProps = Omit<ComponentProps<typeof Button>, "aria-label" | "size" | "className"> & { label: string; className?: string };

export function IconButton({ label, className, variant = "outline", ...props }: IconButtonProps): React.JSX.Element {
  return <Button type="button" variant={variant} size="icon" className={cn("icon-button", className)} aria-label={label} title={label} {...props} />;
}

type SwitchControlProps = {
  id?: string;
  label: string;
  checked: boolean;
  disabled?: boolean;
  developmentMode?: boolean;
  size?: "sm" | "default";
  title?: string;
  "aria-describedby"?: string;
  onToggle(): void;
};

export function SwitchControl({ label, developmentMode = false, onToggle, title, ...props }: SwitchControlProps): React.JSX.Element {
  return <Switch className={developmentMode ? "is-development" : undefined} aria-label={label} title={title ?? label} onCheckedChange={onToggle} {...props} />;
}
