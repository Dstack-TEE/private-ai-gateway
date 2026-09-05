import { useLayoutEffect, useRef, useState, type PropsWithChildren, type ReactNode } from "react";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";

type SheetProps = PropsWithChildren<{
  title: string;
  label?: string;
  description?: ReactNode;
  className?: string;
  headingClassName?: string;
  dismissible?: boolean;
  onClose(): void;
}>;

/** Shared content surface for browser modals and native child-window webviews. */
export function Sheet({ title, label = title, description, className, headingClassName, dismissible = true, onClose, children }: SheetProps): React.JSX.Element {
  const dialog = useRef<HTMLDialogElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const dismissibleRef = useRef(dismissible);
  dismissibleRef.current = dismissible;
  const [opener] = useState(() => document.activeElement instanceof HTMLElement ? document.activeElement : null);

  useLayoutEffect(() => {
    const node = dialog.current;
    if (!node) return;
    node.showModal();
    node.focus();
    const close = () => closeRef.current();
    const cancel = (event: Event) => {
      if (!dismissibleRef.current) event.preventDefault();
    };
    node.addEventListener("close", close);
    node.addEventListener("cancel", cancel);
    return () => {
      node.removeEventListener("close", close);
      node.removeEventListener("cancel", cancel);
      // The opener can be re-enabled by the same commit that removes the sheet.
      window.setTimeout(() => opener?.focus(), 0);
    };
  }, [opener]);

  return <dialog ref={dialog} tabIndex={-1} className={cn("sheet", className)} aria-label={label}>
    <div className={cn("sheet-heading", headingClassName)}>
      <span><h2>{title}</h2>{description && <small>{description}</small>}</span>
    </div>
    {children}
  </dialog>;
}

export function SheetActions({ leading, children }: PropsWithChildren<{ leading?: ReactNode }>): React.JSX.Element {
  return <div className="sheet-actions">
    {leading && <div className="sheet-actions-leading">{leading}</div>}
    {children}
  </div>;
}

export function DismissSheetAction({ onClose }: { onClose(): void }): React.JSX.Element {
  return <SheetActions><Button variant="outline" onClick={onClose}>Done</Button></SheetActions>;
}
