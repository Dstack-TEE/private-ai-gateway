import type { LucideIcon } from "lucide-react";
import { toneTextClass } from "../lib/tone";
import type { Tone } from "../../shared/contracts";
import { cn } from "../lib/utils";

const toneClass: Record<Tone, string> = {
  success: "border-primary/20 bg-primary/10",
  warning: "border-border bg-muted",
  danger: "border-current bg-transparent",
  neutral: "border-border bg-transparent",
};

export function VerificationVerdict({
  tone,
  icon: Icon,
  title,
  detail,
}: {
  tone: Tone;
  icon: LucideIcon;
  title: string;
  detail: string;
}) {
  return (
    <div className={cn("flex shrink-0 items-start gap-3 rounded-2xl border p-3.5", toneClass[tone], toneTextClass[tone])}>
      <Icon className="flex-none" size={22} aria-hidden="true" />
      <span className="grid min-w-0 gap-1.5 wrap-anywhere"><strong className="text-foreground">{title}</strong><small className="text-xs text-muted-foreground">{detail}</small></span>
    </div>
  );
}
