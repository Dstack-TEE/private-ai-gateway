import type { PropsWithChildren, ReactNode } from "react";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "./ui/dialog";

/**
 * A modal dialog in the app window. Callers mount it to open it and unmount it
 * to close it, so drafts never outlive the dialog. Escape and the Close button
 * call `onClose` unless an operation makes the dialog not `dismissible`.
 */
export function AppDialog({ title, description, className, dismissible = true, onClose, children }: PropsWithChildren<{
  title: string;
  description?: ReactNode;
  className?: string;
  dismissible?: boolean;
  onClose(): void;
}>): React.JSX.Element {
  return <Dialog open disablePointerDismissal onOpenChange={(open) => { if (!open && dismissible) onClose(); }}>
    <DialogContent showCloseButton={false} className={cn("flex max-h-[calc(100%-2rem)] flex-col gap-4", className)}>
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
