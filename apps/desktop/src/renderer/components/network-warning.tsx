import { StatusDot } from "./state-label";
import { Badge } from "./ui/badge";
import { Hint } from "./hint";

export function NetworkWarning() {
  return <Hint content="This address allows connections beyond this device. Requests use unencrypted HTTP and require the client key. Use a trusted network; never expose this port to the internet.">
    <Badge variant="outline" render={<button type="button" />} aria-label="Network access warning" className="text-muted-foreground"><StatusDot tone="warning" />Non-loopback</Badge>
  </Hint>;
}
