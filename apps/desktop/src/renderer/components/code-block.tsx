import type { ComponentProps } from "react";
import { cn } from "../lib/utils";

/** Preformatted code, commands or documents, selectable for copying. */
export function CodeBlock({ className, children, ...props }: ComponentProps<"pre">): React.JSX.Element {
  return <pre className={cn("overflow-auto rounded-md bg-muted px-3 py-2 text-xs leading-relaxed select-text", className)} {...props}><code>{children}</code></pre>;
}
