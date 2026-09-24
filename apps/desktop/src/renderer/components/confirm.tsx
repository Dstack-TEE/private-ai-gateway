import { createContext, useCallback, useContext, useRef, useState, type PropsWithChildren } from "react";
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "./ui/alert-dialog";

export interface ConfirmationOptions {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel?: string;
}

type Confirm = (options: ConfirmationOptions) => Promise<boolean>;
const ConfirmContext = createContext<Confirm | null>(null);

/** Asks for a decision in an AlertDialog and resolves with the answer. */
export function ConfirmProvider({ children }: PropsWithChildren) {
  const [request, setRequest] = useState<ConfirmationOptions>();
  const [open, setOpen] = useState(false);
  const resolve = useRef<((confirmed: boolean) => void) | undefined>(undefined);
  const settle = useCallback((confirmed: boolean) => {
    resolve.current?.(confirmed);
    resolve.current = undefined;
    setOpen(false);
  }, []);
  const confirm = useCallback<Confirm>((options) => new Promise((next) => {
    // A newer question replaces one that is still open.
    resolve.current?.(false);
    resolve.current = next;
    setRequest(options);
    setOpen(true);
  }), []);
  return <ConfirmContext.Provider value={confirm}>
    {children}
    <AlertDialog open={open} onOpenChange={(next) => { if (!next) settle(false); }}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{request?.title}</AlertDialogTitle>
          <AlertDialogDescription className="whitespace-pre-line">{request?.message}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{request?.cancelLabel ?? "Cancel"}</AlertDialogCancel>
          <AlertDialogAction onClick={() => settle(true)}>{request?.confirmLabel}</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </ConfirmContext.Provider>;
}

export function useConfirm(): Confirm {
  const confirm = useContext(ConfirmContext);
  if (!confirm) throw new Error("ConfirmProvider is required");
  return confirm;
}
