import { useLayoutEffect, useRef, useState } from "react";
import { TriangleAlert } from "lucide-react";
import { badgeVariants } from "./ui/badge";
import { cn } from "../lib/utils";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "./ui/tooltip";

export function NetworkWarning() {
  const host = useRef<HTMLSpanElement>(null);
  const [container, setContainer] = useState<HTMLDialogElement | null>(null);
  useLayoutEffect(() => { setContainer(host.current?.closest("dialog") ?? null); }, []);
  return <span ref={host}><TooltipProvider><Tooltip>
    <TooltipTrigger className={cn(badgeVariants({ variant: "outline" }), "border-warning/30 bg-warning/10 text-warning")} render={<button type="button" aria-label="Network access warning" />}><TriangleAlert aria-hidden="true" />Non-loopback</TooltipTrigger>
    <TooltipContent role="tooltip" container={container ?? undefined}>This address allows connections beyond this device. Requests use unencrypted HTTP and require the client key. Use a trusted network; never expose this port to the internet.</TooltipContent>
  </Tooltip></TooltipProvider></span>;
}
