import { toneDotClass } from "../lib/tone";
import type { Tone } from "../../shared/contracts";
import { cn } from "../lib/utils";
import { Badge } from "./ui/badge";

export function StatusDot({ tone }: { tone: Tone }) {
  return <span className={cn("size-1.5 shrink-0 rounded-full", toneDotClass[tone])} aria-hidden="true" />;
}

export function StateLabel({ tone, text }: { tone: Tone; text: string }) {
  return <Badge variant="outline">
    <StatusDot tone={tone} />
    {text}
  </Badge>;
}
