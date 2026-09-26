import type { ReactElement, ReactNode } from "react";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";

/** A tooltip on an existing control, without adding layout wrappers. */
export function Hint({ content, children }: { content: ReactNode; children: ReactElement }) {
  return <Tooltip>
    <TooltipTrigger render={children} />
    <TooltipContent role="tooltip">{content}</TooltipContent>
  </Tooltip>;
}

/**
 * A value with details that open on hover or when it is pressed. Unlike a
 * `Hint`, a popover reaches keyboard, touch and screen reader users, so the
 * details may be information the page needs.
 */
export function HoverDetails({ value, children }: { value: ReactNode; children: ReactNode }) {
  return <Popover>
    <PopoverTrigger openOnHover render={<button type="button" className="cursor-default underline decoration-dotted underline-offset-4" />}>{value}</PopoverTrigger>
    <PopoverContent className="w-auto max-w-72 rounded-2xl p-3 text-xs">{children}</PopoverContent>
  </Popover>;
}
