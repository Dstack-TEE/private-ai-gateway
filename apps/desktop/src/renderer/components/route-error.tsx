import { useQueryErrorResetBoundary } from "@tanstack/react-query";
import { Link, useRouter } from "@tanstack/react-router";
import { TriangleAlert } from "lucide-react";
import { titleBarDragRegion } from "../lib/environment";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "./ui/alert";
import { Button, buttonVariants } from "./ui/button";

/**
 * A page that failed to render, in place of the page (the router's
 * `defaultErrorComponent`). React reports the error to the console; the page
 * never shows its text.
 */
export function PageError() {
  return <div className="max-w-230 mx-auto"><ErrorNotice /></div>;
}

/**
 * An address with no page (the root route's `notFoundComponent`), laid out
 * like `WindowError`; the web UI signs in on the way to Overview.
 */
export function PageNotFound() {
  return <main className="grid min-h-svh place-items-center bg-background px-4 pt-12 pb-4 text-foreground">
    <div className="fixed inset-x-0 top-0 h-10" {...titleBarDragRegion} />
    <div className="w-full max-w-lg"><Alert>
      <TriangleAlert aria-hidden="true" />
      <AlertTitle>This page doesn’t exist</AlertTitle>
      <AlertDescription>Check the address, or go to Overview.</AlertDescription>
      <AlertAction><Link to="/" className={buttonVariants({ variant: "outline", size: "sm" })}>Go to Overview</Link></AlertAction>
    </Alert></div>
  </main>;
}

/**
 * The window's layout or the sign-in page failed to render. It fills the
 * window, clear of the macOS title bar controls, and keeps a drag region
 * where the title bar is overlaid.
 */
export function WindowError() {
  return <main className="grid min-h-svh place-items-center bg-background px-4 pt-12 pb-4 text-foreground">
    <div className="fixed inset-x-0 top-0 h-10" {...titleBarDragRegion} />
    <div className="w-full max-w-lg"><ErrorNotice /></div>
  </main>;
}

function ErrorNotice() {
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
    </AlertDescription>
    <AlertAction><Button variant="outline" size="sm" onClick={retry}>Try Again</Button></AlertAction>
  </Alert>;
}
