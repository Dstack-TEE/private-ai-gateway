import type { ComponentProps } from "react";
import { Item } from "./ui/item";
import { cn } from "../lib/utils";

export function ActionItem({ children, className, size = "default", ...props }: ComponentProps<"button"> & {
  size?: "default" | "sm" | "xs";
}): React.JSX.Element {
  return <Item size={size} className={cn("rounded-none border-0 text-left hover:bg-muted focus-visible:ring-0 focus-visible:inset-ring-3 focus-visible:inset-ring-ring/50", className)} render={<button type="button" {...props} />}>
    {children}
  </Item>;
}
