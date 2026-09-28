import React from "react";
import { Network } from "lucide-react";
import { brand } from "../brand/brand";
import { useAppearance } from "./appearance";
import { macOS } from "../lib/environment";
import { SERVICE_ICONS } from "../lib/services";
import type { ServiceProvider } from "../../shared/contracts";
import { cn } from "../lib/utils";

export function BrandMark({ className }: { className?: string }): React.JSX.Element {
  const selectedAppearance = useAppearance();
  const appearance = macOS ? selectedAppearance : "light";
  return (
    <picture className={cn("inline-grid flex-none place-items-center", className)} aria-hidden="true">
      {appearance === "system" && <source media="(prefers-color-scheme: dark)" srcSet={brand.appIcon.dark} />}
      <img className="block size-full object-contain" src={appearance === "dark" ? brand.appIcon.dark : brand.appIcon.light} alt="" />
    </picture>
  );
}

/** `size` enlarges only the generic icon of a custom service; `className` sizes either. */
export function ServiceLogo({ provider, size = "regular", className }: { provider: ServiceProvider; size?: "regular" | "large"; className?: string }): React.JSX.Element {
  const icon = SERVICE_ICONS[provider];
  const frame = "grid size-6 flex-none place-items-center overflow-hidden rounded-md";
  if (!icon) {
    return <span className={cn(frame, size === "large" && "size-7.5", "text-muted-foreground", className)}><Network size={size === "large" ? 16 : 14} /></span>;
  }
  return <span className={cn(frame, className)}><img className="size-full object-contain" src={icon} alt="" /></span>;
}
