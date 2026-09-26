import { useCallback, useState, type ComponentProps, type PropsWithChildren, type ReactNode } from "react";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "./ui/dialog";

/** How the owner of an `AppDialog` shows it; `useDialog` makes one. */
export interface DialogControl {
  open: boolean;
  /** Closes the dialog; it animates out, then `onOpenChangeComplete(false)` follows. */
  onClose(): void;
  onOpenChangeComplete(open: boolean): void;
  /** Where focus goes on closing when the element that opened the dialog is gone. */
  finalFocus?: ComponentProps<typeof DialogContent>["finalFocus"];
}

/**
 * Shows a dialog with a payload. Closing keeps the payload while the dialog
 * animates out; the owner renders the dialog while `payload` is set, so it
 * unmounts once closed and its drafts, queries and effects end with it. A
 * request while the dialog is open is ignored; one while it animates out opens
 * a new dialog under a new `key`.
 */
export function useDialog<T>(initial?: T) {
  const [shown, setShown] = useState<{ payload: T; open: boolean; key: number } | undefined>(() => initial === undefined ? undefined : { payload: initial, open: true, key: 0 });
  const show = useCallback((payload: T) => setShown((current) => current?.open ? current : { payload, open: true, key: (current?.key ?? 0) + 1 }), []);
  const onClose = useCallback(() => setShown((current) => current && { ...current, open: false }), []);
  const onOpenChangeComplete = useCallback((open: boolean) => {
    if (!open) setShown((current) => current?.open ? current : undefined);
  }, []);
  return { payload: shown?.payload, key: shown?.key, show, control: { open: shown?.open ?? false, onClose, onOpenChangeComplete } };
}

/**
 * A modal dialog in the app window, shown through a `useDialog` control.
 * Escape and the Close button call `onClose` unless an operation makes the
 * dialog not `dismissible`.
 */
export function AppDialog({ open, title, description, className, dismissible = true, onClose, onOpenChangeComplete, finalFocus, children }: PropsWithChildren<DialogControl & {
  title: string;
  description?: ReactNode;
  className?: string;
  dismissible?: boolean;
}>): React.JSX.Element {
  return <Dialog open={open} disablePointerDismissal onOpenChange={(next) => { if (!next && dismissible) onClose(); }} onOpenChangeComplete={onOpenChangeComplete}>
    <DialogContent showCloseButton={false} finalFocus={finalFocus} className={cn("flex max-h-[calc(100%-2rem)] flex-col gap-4", className)}>
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        {description && <DialogDescription>{description}</DialogDescription>}
      </DialogHeader>
      {children}
    </DialogContent>
  </Dialog>;
}

/** The footer of a dialog that only presents information. */
export function DoneFooter(): React.JSX.Element {
  return <DialogFooter><DialogClose render={<Button variant="outline" />}>Done</DialogClose></DialogFooter>;
}
