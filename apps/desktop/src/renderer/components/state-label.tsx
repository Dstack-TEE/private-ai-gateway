import { toneDotClass } from "../lib/tone";
import type { Tone } from "../../shared/contracts";
import { cn } from "../lib/utils";
import { Badge } from "./ui/badge";

export function StatusDot({ tone }: { tone: Tone }) {
  return <span data-slot="status-dot" className={cn("size-1.5 shrink-0 rounded-full", toneDotClass[tone])} aria-hidden="true" />;
}

export function StateLabel({ tone, text }: { tone: Tone; text: string }) {
  return <Badge variant="outline" className="text-muted-foreground">
    <StatusDot tone={tone} />
    {text}
  </Badge>;
}
