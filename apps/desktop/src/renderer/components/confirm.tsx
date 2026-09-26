import { createContext, useCallback, useContext, useRef, useState, type PropsWithChildren } from "react";
import type { ConfirmationOptions, DesktopApi } from "../../shared/contracts";
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "./ui/alert-dialog";

export type { ConfirmationOptions };

type Confirm = (options: ConfirmationOptions) => Promise<boolean>;
const ConfirmContext = createContext<Confirm | null>(null);
const ConfirmOpenContext = createContext(false);

/**
 * Asks for a decision and resolves with the answer: in the system alert when
 * `ask` shows one (the desktop app), otherwise in an AlertDialog. Either way the
 * confirm button is the default (Return) button: the user chose the action,
 * and the question only makes sure (Apple HIG, Alerts).
 */
export function ConfirmProvider({ ask, children }: PropsWithChildren<{ ask?: DesktopApi["showConfirmation"] }>) {
  const [request, setRequest] = useState<ConfirmationOptions>();
  const [open, setOpen] = useState(false);
  const [asking, setAsking] = useState(0);
  const resolve = useRef<((confirmed: boolean) => void) | undefined>(undefined);
  // Focus returns to the element focused when the question was asked. When it is
  // gone by then (a deleted item's controls) or nothing had focus (a button that
  // disabled itself before asking), the action that follows decides where focus goes.
  const opener = useRef<Element | null>(null);
  const action = useRef<HTMLButtonElement>(null);
  const settle = useCallback((confirmed: boolean) => {
    resolve.current?.(confirmed);
    resolve.current = undefined;
    setOpen(false);
  }, []);
  const confirm = useCallback<Confirm>((options) => {
    if (ask) {
      setAsking((count) => count + 1);
      return ask(options).finally(() => setAsking((count) => count - 1));
    }
    return new Promise((next) => {
      // A newer question replaces one that is still open.
      resolve.current?.(false);
      resolve.current = next;
      opener.current = document.activeElement;
      setRequest(options);
      setOpen(true);
    });
  }, [ask]);
  return <ConfirmContext.Provider value={confirm}><ConfirmOpenContext.Provider value={open || asking > 0}>
    {children}
    <AlertDialog open={open} onOpenChange={(next) => { if (!next) settle(false); }}>
      <AlertDialogContent initialFocus={action} finalFocus={() => opener.current !== document.body && Boolean(opener.current?.isConnected)}>
        <AlertDialogHeader>
          <AlertDialogTitle>{request?.title}</AlertDialogTitle>
          <AlertDialogDescription className="whitespace-pre-line">{request?.message}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{request?.cancelLabel ?? "Cancel"}</AlertDialogCancel>
          <AlertDialogAction ref={action} variant={request?.destructive ? "destructive" : "default"} onClick={() => settle(true)}>{request?.confirmLabel}</AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </ConfirmOpenContext.Provider></ConfirmContext.Provider>;
}

export function useConfirm(): Confirm {
  const confirm = useContext(ConfirmContext);
  if (!confirm) throw new Error("ConfirmProvider is required");
  return confirm;
}

/** Whether a confirmation is waiting for an answer. */
export function useConfirmOpen(): boolean {
  return useContext(ConfirmOpenContext);
}
