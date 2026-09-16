import { StatusDot } from "./state-label";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Dialog, DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle, DialogDescription, DialogTrigger } from "./ui/dialog";

export function AgentAttention({ name, message, authorized, action, onRepair }: {
  name: string;
  message: string;
  authorized: boolean;
  action?: "reconnect" | "disconnect";
  onRepair(): void;
}) {
  const label = authorized ? "Check model" : action === "reconnect" ? "Reconnect required" : action === "disconnect" ? "Finish disconnecting" : "Check configuration";
  return <Dialog>
    <DialogTrigger render={<Badge variant="outline" className="text-muted-foreground" render={<button type="button" />} />} aria-label={`${name}: ${label}`}>
      <StatusDot tone="warning" />{label}
    </DialogTrigger>
    <DialogContent>
      <DialogHeader><DialogTitle>{name}: {label}</DialogTitle><DialogDescription className="break-words">{message}</DialogDescription></DialogHeader>
      {action && <DialogFooter><DialogClose render={<Button size="sm" />} onClick={onRepair}>{action === "reconnect" ? "Reconnect" : "Disconnect"} {name}</DialogClose></DialogFooter>}
    </DialogContent>
  </Dialog>;
}
