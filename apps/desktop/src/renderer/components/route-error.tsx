import { useRouter, type ErrorComponentProps } from "@tanstack/react-router";
import { TriangleAlert } from "lucide-react";
import { errorMessage } from "../lib/error-message";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "./ui/alert";
import { Button } from "./ui/button";

/**
 * A page that failed to render (the router's `defaultErrorComponent`), in
 * place of the page. The router logs the error; the details stay collapsed.
 */
export function RouteError({ error }: ErrorComponentProps) {
  const router = useRouter();
  return <div className="max-w-230 mx-auto">
    <Alert variant="destructive">
      <TriangleAlert aria-hidden="true" />
      <AlertTitle>This page couldn’t be shown</AlertTitle>
      <AlertDescription>
        <p>Try again. If it keeps happening, export diagnostics from Settings and report the problem.</p>
        <details className="mt-2">
          <summary className="cursor-pointer">Details</summary>
          <p className="mt-1 font-mono text-xs wrap-anywhere">{errorMessage(error)}</p>
        </details>
      </AlertDescription>
      {/* Invalidating the router reloads its routes and resets the error boundary. */}
      <AlertAction><Button variant="outline" size="sm" onClick={() => void router.invalidate()}>Try Again</Button></AlertAction>
    </Alert>
  </div>;
}
