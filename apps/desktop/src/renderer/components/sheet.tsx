import { useLayoutEffect, useRef, useState, type PropsWithChildren, type ReactNode } from "react";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";
import { Separator } from "./ui/separator";
import { useDialogClose } from "./dialog-close";

type SheetProps = PropsWithChildren<{
  title: string;
  label?: string;
  description?: ReactNode;
  className?: string;
  headingClassName?: string;
  dismissible?: boolean;
  initialFocus?: "heading" | "field";
  onClose(): void;
}>;

/** Shared content surface for browser modals and native child-window webviews. */
export function Sheet({ title, label = title, description, className, headingClassName, dismissible = true, initialFocus = "heading", onClose, children }: SheetProps): React.JSX.Element {
  useDialogClose(onClose, dismissible);
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
    const field = initialFocus === "field" ? node.querySelector<HTMLElement>('input:not(:disabled):not([readonly]):not([type=hidden]), textarea:not(:disabled):not([readonly]), select:not(:disabled)') : null;
    (field ?? node.querySelector<HTMLElement>("h2") ?? node).focus({ preventScroll: true });
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
  }, [opener, initialFocus]);

  return <dialog ref={dialog} tabIndex={-1} className={cn("sheet", className)} aria-label={label}>
    <div className={cn("sheet-heading", headingClassName)}>
      <span><h2 tabIndex={-1}>{title}</h2>{description && <small>{description}</small>}</span>
    </div>
    {children}
  </dialog>;
}

export function SheetActions({ leading, children }: PropsWithChildren<{ leading?: ReactNode }>): React.JSX.Element {
  return <div className="sheet-footer">
    <Separator />
    <div className="sheet-actions">
      {leading && <div className="sheet-actions-leading">{leading}</div>}
      {children}
    </div>
  </div>;
}

export function DismissSheetAction({ onClose }: { onClose(): void }): React.JSX.Element {
  return <SheetActions><Button variant="outline" onClick={onClose}>Done</Button></SheetActions>;
}
