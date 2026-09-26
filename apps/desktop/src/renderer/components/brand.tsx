import React from "react";
import { Network } from "lucide-react";
import { brand } from "../brand/brand";
import { useAppearance } from "./appearance";
import { macOS } from "../lib/environment";
import { SERVICE_ICONS } from "../lib/services";
import { cn } from "../lib/utils";
import type { ServiceProvider } from "../../shared/contracts";

export function BrandMark({ className }: { className?: string }): React.JSX.Element {
  const selectedAppearance = useAppearance();
  const appearance = macOS ? selectedAppearance : "light";
  return (
    <picture className={cn("flex-none", className)} aria-hidden="true">
      {appearance === "system" && <source media="(prefers-color-scheme: dark)" srcSet={brand.appIcon.dark} />}
      <img className="size-full object-contain" src={appearance === "dark" ? brand.appIcon.dark : brand.appIcon.light} alt="" />
    </picture>
  );
}

export function ServiceLogo({ provider, size = "regular" }: { provider: ServiceProvider; size?: "regular" | "large" }): React.JSX.Element {
  const icon = SERVICE_ICONS[provider];
  return <span className={cn("grid flex-none place-items-center overflow-hidden rounded-md text-muted-foreground", size === "large" ? "size-7.5" : "size-6")}>
    {icon ? <img className="size-full object-contain" src={icon} alt="" /> : <Network size={size === "large" ? 16 : 14} aria-hidden="true" />}
  </span>;
}
