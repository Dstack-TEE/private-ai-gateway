import { useQueryErrorResetBoundary } from "@tanstack/react-query";
import { useRouter, type ErrorComponentProps } from "@tanstack/react-router";
import { TriangleAlert } from "lucide-react";
import { errorMessage } from "../lib/error-message";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "./ui/alert";
import { Button } from "./ui/button";

/**
 * A page that failed to render, in place of the page (the router's
 * `defaultErrorComponent`). The router logs the error; the details stay
 * collapsed.
 */
export function PageError({ error }: ErrorComponentProps) {
  return <div className="max-w-230 mx-auto"><ErrorNotice error={error} /></div>;
}

/**
 * The window's layout or the sign-in page failed to render. It fills the
 * window, clear of the macOS title bar controls, and keeps a drag region.
 */
export function WindowError({ error }: ErrorComponentProps) {
  return <main className="grid min-h-svh place-items-center bg-background px-4 pt-12 pb-4 text-foreground">
    <div className="fixed inset-x-0 top-0 h-10" data-tauri-drag-region />
    <div className="w-full max-w-lg"><ErrorNotice error={error} /></div>
  </main>;
}

function ErrorNotice({ error }: { error: unknown }) {
  const router = useRouter();
  const queries = useQueryErrorResetBoundary();
  // TanStack's retry: reset query errors, then reload the routes, which also
  // resets the error boundary.
  const retry = () => {
    queries.reset();
    void router.invalidate();
  };
  return <Alert variant="destructive">
    <TriangleAlert aria-hidden="true" />
    <AlertTitle>This page couldn’t be shown</AlertTitle>
    <AlertDescription>
      <p>Try again. If it keeps happening, export diagnostics from Settings and report the problem.</p>
      <details className="mt-2">
        <summary className="cursor-pointer">Details</summary>
        <p className="mt-1 font-mono text-xs wrap-anywhere">{errorMessage(error)}</p>
      </details>
    </AlertDescription>
    <AlertAction><Button variant="outline" size="sm" onClick={retry}>Try Again</Button></AlertAction>
  </Alert>;
}
