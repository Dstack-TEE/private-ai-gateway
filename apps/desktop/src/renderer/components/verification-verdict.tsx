import type { LucideIcon } from "lucide-react";
import { toneTextClass, type Tone } from "../lib/tone";
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
    <div className={cn(
      "flex items-start gap-3 rounded-2xl border p-3.5 [&_>_svg]:flex-none [&_>_span]:grid [&_>_span]:min-w-0 [&_>_span]:gap-1.5 [&_>_span]:wrap-anywhere [&_small]:text-xs [&_small]:text-muted-foreground [&_strong]:text-foreground",
      toneClass[tone],
      toneTextClass[tone],
    )} data-slot="verification-verdict">
      <Icon size={22} aria-hidden="true" />
      <span><strong>{title}</strong><small>{detail}</small></span>
    </div>
  );
}
