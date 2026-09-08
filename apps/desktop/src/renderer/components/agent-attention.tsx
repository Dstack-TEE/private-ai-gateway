import { StatusDot } from "./state-label";
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
    <PopoverTrigger render={<Badge variant="outline" className="text-muted-foreground" render={<button type="button" />} />} aria-label={`${name}: ${label}`}>
      <StatusDot tone="warning" />{label}
    </PopoverTrigger>
    <PopoverContent align="start">
      <p className="break-words">{message}</p>
      {action && <Button size="sm" onClick={onRepair}>{action === "reconnect" ? "Reconnect" : "Disconnect"} {name}</Button>}
    </PopoverContent>
  </Popover>;
}
