import { StatusDot } from "./state-label";
import { Badge } from "./ui/badge";
import { Hint } from "./hint";

/** `access` names what a request needs, e.g. "the client key". */
export function NetworkWarning({ access = "require the client key" }: { access?: string }) {
  return <Hint content={`This address allows connections beyond this device. Requests use unencrypted HTTP and ${access}. Use a trusted network; never expose this port to the internet.`}>
    <Badge variant="outline" render={<button type="button" />} aria-label="Network access warning" className="text-muted-foreground"><StatusDot tone="warning" />Non-loopback</Badge>
  </Hint>;
}
