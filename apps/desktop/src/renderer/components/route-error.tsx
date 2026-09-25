import { useEffect } from "react";
import { useRouter, type ErrorComponentProps } from "@tanstack/react-router";
import { errorMessage } from "../lib/error-message";
import { desktopApi } from "../lib/environment";
import { useAppearanceTheme } from "./appearance";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";

/**
 * A page that failed to render (the router's `defaultErrorComponent`). It
 * replaces the layout that shows the hidden desktop window, so it shows the
 * window itself.
 */
export function RouteError({ error }: ErrorComponentProps) {
  const router = useRouter();
  useAppearanceTheme("system");
  useEffect(() => {
    void desktopApi.mainWindowReady().catch((failure: unknown) => console.error("Could not show the window", failure));
  }, []);
  return <main className="grid min-h-svh place-items-center bg-background p-4 text-foreground">
    <Card role="alert" className="w-full max-w-sm">
      <CardHeader>
        <CardTitle className="text-lg">Something went wrong</CardTitle>
        <CardDescription>{errorMessage(error)}</CardDescription>
      </CardHeader>
      <CardContent>
        {/* Invalidating the router reloads its routes and resets the error boundary. */}
        <Button onClick={() => void router.invalidate()}>Try Again</Button>
      </CardContent>
    </Card>
  </main>;
}
