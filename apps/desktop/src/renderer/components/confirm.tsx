import { createContext, useCallback, useContext, useEffect, useEffectEvent, useRef, useState, type PropsWithChildren } from "react";
import { createDialogQueue } from "../lib/dialog-queue";
import { errorMessage } from "../lib/error-message";
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "./ui/alert-dialog";

/** A question asked before an action (`useConfirm`). */
export type Confirmation = {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel?: string;
  /** The action deletes, revokes or resets something that can't be restored. */
  destructive?: boolean;
};
type Confirm = (options: Confirmation) => Promise<boolean>;
type Request = ({ kind: "confirm" } & Confirmation) | ({ kind: "alert" } & Pick<Confirmation, "title" | "message">);
const ConfirmContext = createContext<((request: Request) => Promise<boolean>) | null>(null);
const ConfirmOpenContext = createContext(false);

/**
 * Asks for decisions (`useConfirm`) and reports failed actions
 * (`useReportFailure`) in an AlertDialog, one request at a time in the order
 * they were made (`createDialogQueue`). A destructive question focuses
 * Cancel, the safe choice, so Return never starts the action; any other
 * question focuses its action, and an alert its only button, OK.
 */
export function ConfirmProvider({ children }: PropsWithChildren) {
  const [request, setRequest] = useState<Request>();
  const [open, setOpen] = useState(false);
  const [asking, setAsking] = useState(0);
  // Focus returns to the element focused when the request showed, or, when
  // nothing outside the dialog had focus then (a button that disabled itself
  // before asking, or the answer to the previous request), to the one before.
  // When that is gone too (a deleted item's controls), the action that follows
  // decides where focus goes. A request shown after the queue drained never
  // falls back to the previous opener.
  const opener = useRef<HTMLElement | null>(null);
  const drained = useRef(true);
  const popup = useRef<HTMLDivElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const action = useRef<HTMLButtonElement>(null);
  const [queue] = useState(() => createDialogQueue<Request>((next) => {
    const focused = document.activeElement;
    const outside = focused instanceof HTMLElement && focused !== document.body && !popup.current?.contains(focused) ? focused : null;
    if (outside || drained.current) opener.current = outside;
    drained.current = false;
    setRequest(next);
    setOpen(true);
  }, () => setOpen(false)));
  /** Counts a request as open until it is answered, while shown or queued. */
  const ask = useCallback((next: Request) => {
    setAsking((count) => count + 1);
    return queue.ask(next).finally(() => setAsking((count) => count - 1));
  }, [queue]);
  return <ConfirmContext.Provider value={ask}><ConfirmOpenContext.Provider value={asking > 0}>
    {children}
    <AlertDialog open={open} onOpenChange={(next) => { if (!next) queue.answer(false); }} onOpenChangeComplete={(next) => { if (!next) drained.current = !queue.closed(); }}>
      <AlertDialogContent ref={popup} initialFocus={request?.kind === "confirm" && request.destructive ? cancel : action} finalFocus={() => opener.current?.isConnected ? opener.current : false}>
        <AlertDialogHeader>
          <AlertDialogTitle>{request?.title}</AlertDialogTitle>
          <AlertDialogDescription className="whitespace-pre-line">{request?.message}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          {request?.kind === "alert"
            ? <AlertDialogAction ref={action} onClick={() => queue.answer(true)}>OK</AlertDialogAction>
            : <>
              <AlertDialogCancel ref={cancel}>{request?.cancelLabel ?? "Cancel"}</AlertDialogCancel>
              <AlertDialogAction ref={action} variant={request?.destructive ? "destructive" : "default"} onClick={() => queue.answer(true)}>{request?.confirmLabel}</AlertDialogAction>
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
  const ask = useConfirmContext();
  return useCallback((options) => ask({ ...options, kind: "confirm" }), [ask]);
}

/**
 * Reports a failed action that has no dialog, form or row to explain it, as
 * an alert titled with what failed and the reason as its message.
 */
export function useReportFailure(): (title: string, error: unknown) => void {
  const ask = useConfirmContext();
  return useCallback((title, error) => void ask({ kind: "alert", title, message: errorMessage(error) }), [ask]);
}

/**
 * Reports a failure that lasts, such as a read that keeps failing, as
 * `useReportFailure` does, once when it starts rather than on every retry.
 * The controls it feeds stay disabled or empty until a later read, such as
 * when the window is focused again, succeeds.
 */
export function useReportFailureOnce(title: string, error: unknown): void {
  const reportFailure = useReportFailure();
  const failed = Boolean(error);
  const report = useEffectEvent(() => reportFailure(title, error));
  useEffect(() => {
    if (failed) report();
  }, [failed]);
}

/** Whether a confirmation or an alert is waiting for an answer. */
export function useConfirmOpen(): boolean {
  return useContext(ConfirmOpenContext);
}
