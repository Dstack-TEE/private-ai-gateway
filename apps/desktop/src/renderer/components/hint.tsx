import { useCallback, useState, type ReactElement, type ReactNode } from "react";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";

/** Keep tooltips inside an owning HTML dialog's top layer without adding layout wrappers. */
export function Hint({ content, children }: { content: ReactNode; children: ReactElement }) {
  const [container, setContainer] = useState<HTMLDialogElement | null>(null);
  const attach = useCallback((element: HTMLElement | null) => {
    if (element) setContainer(element.closest("dialog"));
  }, []);
  return <Tooltip>
    <TooltipTrigger ref={attach} render={children} />
    <TooltipContent container={container ?? undefined} role="tooltip">{content}</TooltipContent>
  </Tooltip>;
}
