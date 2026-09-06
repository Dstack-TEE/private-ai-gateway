import type { LucideIcon } from "lucide-react";
import type { Tone } from "../lib/usage-presentation";
import { Badge } from "./ui/badge";

export function StateLabel({ tone, icon: Icon, text }: { tone: Tone; icon?: LucideIcon; text: string }) {
  return <Badge variant={tone === "danger" ? "destructive" : "outline"} className={tone === "success" ? "border-success/20 bg-success/10 text-success" : tone === "warning" ? "border-warning/20 bg-warning/10 text-warning" : undefined}>
    {Icon ? <Icon size={13} aria-hidden="true" /> : <span data-slot="status-dot" className="size-1.5 shrink-0 rounded-full bg-current" aria-hidden="true" />}
    {text}
  </Badge>;
}
