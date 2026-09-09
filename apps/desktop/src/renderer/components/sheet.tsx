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
  onClose(): void;
}>;

/** Shared content surface for browser modals and native child-window webviews. */
export function Sheet({ title, label = title, description, className, headingClassName, dismissible = true, onClose, children }: SheetProps): React.JSX.Element {
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
    (node.querySelector<HTMLElement>("h2") ?? node).focus({ preventScroll: true });
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

  return <dialog ref={dialog} tabIndex={-1} className={cn("sheet [&.form-sheet]:px-0 fixed left-[var(--window-center-x,_50%)] top-[var(--window-center-y,_50%)] w-[min(410px,_calc(var(--window-dialog-width,_100vw)_-_32px))] max-h-[calc(var(--window-dialog-height,_100vh)_-_32px)] m-0 overflow-auto p-5 text-foreground bg-background border border-border rounded-4xl shadow-xl -translate-x-1/2 -translate-y-1/2 focus:outline-none focus:shadow-xl focus-visible:outline-none focus-visible:shadow-xl [&::backdrop]:bg-[var(--backdrop)] max-[440px]:w-[calc(100vw_-_16px)] max-[440px]:max-h-[calc(100vh_-_16px)] max-[440px]:p-4", className)} aria-label={label}>
    <div className={cn("sheet-heading flex items-center gap-3", headingClassName)}>
      <span><h2 tabIndex={-1} className="text-lg font-semibold text-foreground focus:outline-none focus:ring-0 focus:shadow-none">{title}</h2>{description && <small>{description}</small>}</span>
    </div>
    {children}
  </dialog>;
}

export function SheetActions({ leading, children }: PropsWithChildren<{ leading?: ReactNode }>): React.JSX.Element {
  return <div className="sheet-footer mt-5">
    <Separator />
    <div className="sheet-actions pt-4 flex items-center justify-end gap-3">
      {leading && <div className="sheet-actions-leading mr-auto flex items-center gap-3">{leading}</div>}
      {children}
    </div>
  </div>;
}

export function DismissSheetAction({ onClose }: { onClose(): void }): React.JSX.Element {
  return <SheetActions><Button variant="outline" onClick={onClose}>Done</Button></SheetActions>;
}

export function NativeDialogHost({ className, ...props }: React.ComponentProps<"main">) {
  return <main className={cn("native-dialog-host w-full h-full overflow-hidden bg-background [&_.sheet]:fixed [&_.sheet]:inset-0 [&_.sheet]:size-full [&_.sheet]:max-w-none [&_.sheet]:max-h-none [&_.sheet]:p-5 [&_.sheet]:transform-none [&_.sheet]:translate-none [&_.sheet]:bg-background [&_.sheet]:border-0 [&_.sheet]:rounded-none [&_.sheet]:shadow-none [&_.sheet::backdrop]:bg-transparent [&_.sheet[open]]:flex [&_.sheet[open]]:flex-col [&_.sheet[open]]:overflow-hidden [&_.privacy-content]:min-h-0 [&_.privacy-content]:flex-auto [&_.privacy-content]:overflow-auto [&_.sheet_form]:min-h-0 [&_.sheet_form]:flex-auto [&_.sheet_form]:flex [&_.sheet_form]:flex-col [&_.sheet-scroll]:min-h-0 [&_.sheet-scroll]:flex-auto [&_.sheet-scroll]:overflow-auto [&_.sheet-footer]:flex-none [&_.usage-evidence-sheet_.proof-card]:min-h-0 [&_.usage-evidence-sheet_.proof-card]:flex-auto [&_.usage-evidence-sheet_.proof-card]:overflow-auto [&_.usage-evidence-sheet_.proof-card]:pb-3 [&_.privacy-content]:pb-3", className)} {...props} />;
}
