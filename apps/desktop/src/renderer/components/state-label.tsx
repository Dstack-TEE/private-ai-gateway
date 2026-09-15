import type { Tone } from "../lib/usage-presentation";
import { Badge } from "./ui/badge";

export function StatusDot({ tone }: { tone: Tone }) {
  return <span data-slot="status-dot" className={`size-1.5 shrink-0 rounded-full ${tone === "success" ? "bg-primary" : tone === "warning" ? "bg-warning" : tone === "danger" ? "bg-destructive" : "bg-muted-foreground"}`} aria-hidden="true" />;
}

export function StateLabel({ tone, text }: { tone: Tone; text: string }) {
  return <Badge variant="outline" className="text-muted-foreground">
    <StatusDot tone={tone} />
    {text}
  </Badge>;
}
