import { createContext, useCallback, useContext, useEffect, useEffectEvent, useRef, useState, type PropsWithChildren } from "react";
import { createDialogQueue } from "../lib/dialog-queue";
import type { UseQueryResult } from "@tanstack/react-query";
import { errorMessage, SessionEndedError } from "../lib/error-message";
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
  // The sign-in page says the session ended; nothing is reported over it.
  return useCallback((title, error) => {
    if (!(error instanceof SessionEndedError)) void ask({ kind: "alert", title, message: errorMessage(error) });
  }, [ask]);
}

/** The reads whose failure was reported, by title, until one succeeds. */
const reportedReads = new Set<string>();

/**
 * Reports a read that fails, as `useReportFailure` does, once per failure:
 * not again when it is retried, read under another key or read by a page
 * shown again, until it succeeds. Its controls stay disabled or empty
 * meanwhile, and it is read again when the window is focused.
 */
export function useReportReadFailure(title: string, { error, isSuccess, isPlaceholderData }: Pick<UseQueryResult, "error" | "isSuccess" | "isPlaceholderData">): void {
  const reportFailure = useReportFailure();
  const failed = Boolean(error);
  const succeeded = isSuccess && !isPlaceholderData;
  const report = useEffectEvent(() => {
    if (error instanceof SessionEndedError || reportedReads.has(title)) return;
    reportedReads.add(title);
    reportFailure(title, error);
  });
  useEffect(() => {
    if (succeeded) reportedReads.delete(title);
    else if (failed) report();
  }, [title, failed, succeeded]);
}

/** Whether a confirmation or an alert is waiting for an answer. */
export function useConfirmOpen(): boolean {
  return useContext(ConfirmOpenContext);
}
