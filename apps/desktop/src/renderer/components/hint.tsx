import type { ReactElement, ReactNode } from "react";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";

/** A tooltip on an existing control, without adding layout wrappers. */
export function Hint({ content, children }: { content: ReactNode; children: ReactElement }) {
  return <Tooltip>
    <TooltipTrigger render={children} />
    <TooltipContent role="tooltip">{content}</TooltipContent>
  </Tooltip>;
}
