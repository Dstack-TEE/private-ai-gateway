import { TriangleAlert } from "lucide-react";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";

export function AgentAttention({ name, message, authorized, action, onRepair }: {
  name: string;
  message: string;
  authorized: boolean;
  action?: "reconnect" | "disconnect";
  onRepair(): void;
}) {
  const label = authorized ? "Check model" : action === "reconnect" ? "Reconnect required" : action === "disconnect" ? "Finish disconnecting" : "Check configuration";
  return <Popover>
    <PopoverTrigger render={<Badge variant="outline" className="border-warning/30 bg-warning/10 text-warning" render={<button type="button" />} />} aria-label={`${name}: ${label}`}>
      <TriangleAlert aria-hidden="true" />{label}
    </PopoverTrigger>
    <PopoverContent align="start">
      <p className="break-words">{message}</p>
      {action && <Button size="sm" onClick={onRepair}>{action === "reconnect" ? "Reconnect" : "Disconnect"} {name}</Button>}
    </PopoverContent>
  </Popover>;
}
