import React, { useEffect } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { queryClient } from "./lib/query-client";
import { TooltipProvider } from "./components/ui/tooltip";
import { ConfirmProvider } from "./components/confirm";
import { installNativeInteractions } from "./lib/native-interactions";
import { desktopApi, platform, web } from "./lib/environment";
import { router } from "./router";

/**
 * How long the pointer rests before a tooltip shows: about a second for macOS
 * help tags (AppKit's NSInitialToolTipDelay), and Windows' and GTK's 500 ms.
 * A browser keeps Base UI's default.
 */
const tooltipDelay = platform === "macos" ? 1_000 : platform ? 500 : undefined;

export function Renderer(): React.JSX.Element {
  // Browsers keep their own context menu, shortcuts and drop behavior.
  useEffect(() => web ? undefined : installNativeInteractions(desktopApi, platform), []);
  // The desktop app asks in the system alert; the web UI in a dialog.
  return <QueryClientProvider client={queryClient}><TooltipProvider delay={tooltipDelay}><ConfirmProvider ask={web ? undefined : desktopApi.showConfirmation}>
    <RouterProvider router={router} />
  </ConfirmProvider></TooltipProvider></QueryClientProvider>;
}
