import { createContext, useCallback, useContext, useMemo, useRef, useState, type PropsWithChildren } from "react";
import type { AlertMessage, Confirmation, DesktopApi } from "../../shared/contracts";
import { createDialogQueue, failureAlert, presentAlert } from "../lib/alert";
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "./ui/alert-dialog";

type Confirm = (options: Confirmation) => Promise<boolean>;
type Alert = (alert: AlertMessage) => Promise<void>;
type Request = { kind: "confirm"; options: Confirmation } | { kind: "alert"; options: AlertMessage };
const ConfirmContext = createContext<{ confirm: Confirm; alert: Alert } | null>(null);
const ConfirmOpenContext = createContext(false);

/**
 * Asks for a decision and resolves with the answer: in an alert sheet when
 * `ask` shows one (the macOS app, where the confirm button is the default),
 * otherwise in an AlertDialog. A destructive action always asks in the
 * dialog, since its button must never be the default. The dialog focuses
 * Cancel, the safe choice, so Return never starts the action. Questions and
 * alerts for the dialog wait their turn (`createDialogQueue`).
 *
 * It reports a failed action the same way (`useReportFailure`): in an alert
 * sheet when `tell` shows one, otherwise in the dialog with only an OK button.
 */
export function ConfirmProvider({ ask, tell, children }: PropsWithChildren<{ ask?: DesktopApi["showConfirmation"]; tell?: DesktopApi["showAlert"] }>) {
  const [request, setRequest] = useState<Request>();
  const [open, setOpen] = useState(false);
  const [asking, setAsking] = useState(0);
  // Focus returns to the element focused when the request showed, or, when
  // nothing outside the dialog had focus then (a button that disabled itself
  // before asking, or the answer to the previous request), to the one before.
  // When that is gone too (a deleted item's controls), the action that follows
  // decides where focus goes.
  const opener = useRef<HTMLElement | null>(null);
  const popup = useRef<HTMLDivElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const acknowledge = useRef<HTMLButtonElement>(null);
  const [queue] = useState(() => createDialogQueue<Request>((next) => {
    const focused = document.activeElement;
    if (focused instanceof HTMLElement && focused !== document.body && !popup.current?.contains(focused)) opener.current = focused;
    setRequest(next);
    setOpen(true);
  }, () => setOpen(false)));
  const inSheet = useCallback(<T,>(present: () => Promise<T>) => {
    setAsking((count) => count + 1);
    return present().finally(() => setAsking((count) => count - 1));
  }, []);
  const confirm = useCallback<Confirm>((options) => {
    if (ask && !options.destructive) return inSheet(() => ask(options));
    return queue.ask({ kind: "confirm", options });
  }, [ask, inSheet, queue]);
  const alert = useCallback<Alert>((options) => presentAlert(
    options,
    tell && ((next) => inSheet(() => tell(next))),
    (next) => queue.ask({ kind: "alert", options: next }),
  ), [tell, inSheet, queue]);
  const alerting = request?.kind === "alert";
  const value = useMemo(() => ({ confirm, alert }), [confirm, alert]);
  return <ConfirmContext.Provider value={value}><ConfirmOpenContext.Provider value={open || asking > 0}>
    {children}
    <AlertDialog open={open} onOpenChange={(next) => { if (!next) queue.answer(false); }} onOpenChangeComplete={(next) => { if (!next) queue.closed(); }}>
      <AlertDialogContent ref={popup} initialFocus={alerting ? acknowledge : cancel} finalFocus={() => opener.current?.isConnected ? opener.current : false}>
        <AlertDialogHeader>
          <AlertDialogTitle>{request?.options.title}</AlertDialogTitle>
          <AlertDialogDescription className="whitespace-pre-line">{request?.options.message}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          {alerting
            ? <AlertDialogAction ref={acknowledge} onClick={() => queue.answer(true)}>OK</AlertDialogAction>
            : <>
              <AlertDialogCancel ref={cancel}>{request?.options.cancelLabel ?? "Cancel"}</AlertDialogCancel>
              <AlertDialogAction variant={request?.options.destructive ? "destructive" : "default"} onClick={() => queue.answer(true)}>{request?.options.confirmLabel}</AlertDialogAction>
            </>}
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </ConfirmOpenContext.Provider></ConfirmContext.Provider>;
}

function useConfirmContext() {
  const context = useContext(ConfirmContext);
  if (!context) throw new Error("ConfirmProvider is required");
  return context;
}

export function useConfirm(): Confirm {
  return useConfirmContext().confirm;
}

/**
 * Reports a failed action that has no dialog, form or row to explain it, as
 * an alert titled with what failed and the reason as its message.
 */
export function useReportFailure(): (title: string, error: unknown) => void {
  const { alert } = useConfirmContext();
  return useCallback((title, error) => void alert(failureAlert(title, error)), [alert]);
}

/** Whether a confirmation or an alert is waiting for an answer. */
export function useConfirmOpen(): boolean {
  return useContext(ConfirmOpenContext);
}
