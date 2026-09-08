import { TriangleAlert } from "lucide-react";
import { Badge } from "./ui/badge";
import { Hint } from "./hint";

export function NetworkWarning() {
  return <Hint content="This address allows connections beyond this device. Requests use unencrypted HTTP and require the client key. Use a trusted network; never expose this port to the internet.">
    <Badge variant="outline" render={<button type="button" />} aria-label="Network access warning" className="border-warning/30 bg-warning/10 text-warning"><TriangleAlert aria-hidden="true" />Non-loopback</Badge>
  </Hint>;
}
