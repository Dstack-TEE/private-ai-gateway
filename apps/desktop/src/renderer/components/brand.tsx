import React from "react";
import { Network } from "lucide-react";
import { brand } from "../generated/brand";
import { useAppearance } from "./appearance";
import { macOS } from "../lib/environment";
import { servicePreset } from "../lib/services";

export function BrandMark({ className = "", busy = false }: { className?: string; busy?: boolean }): React.JSX.Element {
  const selectedAppearance = useAppearance();
  const appearance = macOS ? selectedAppearance : "light";
  const classes = ["brand-logo inline-grid place-items-center flex-none [&>img]:block [&>img]:size-full [&>img]:object-contain motion-reduce:animate-none", className, busy ? "is-busy animate-brand-icon-pulse" : ""].filter(Boolean).join(" ");
  return (
    <picture className={classes} aria-hidden="true">
      {appearance === "system" && <source media="(prefers-color-scheme: dark)" srcSet={brand.appIcon.dark} />}
      <img src={appearance === "dark" ? brand.appIcon.dark : brand.appIcon.light} alt="" />
    </picture>
  );
}

export function ServiceLogo({ url, size = "regular" }: { url: string; size?: "regular" | "large" }): React.JSX.Element {
  const service = servicePreset(url);
  if (!service) {
    return <span className={`service-custom-icon w-6 h-6 flex-none grid place-items-center overflow-hidden rounded-md [&.service-logo-large]:w-7.5 [&.service-logo-large]:h-7.5 text-muted-foreground service-logo-${size}`}><Network size={size === "large" ? 16 : 14} /></span>;
  }
  return <span className={`service-logo w-6 h-6 flex-none grid place-items-center overflow-hidden rounded-md [&_img]:w-full [&_img]:h-full [&_img]:object-contain service-${service.id} service-logo-${size}`}><img src={service.icon} alt="" /></span>;
}
