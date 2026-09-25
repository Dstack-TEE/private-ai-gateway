import React, { useEffect } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { queryClient } from "./lib/query-client";
import { TooltipProvider } from "./components/ui/tooltip";
import { ConfirmProvider } from "./components/confirm";
import { installNativeInteractions } from "./lib/native-interactions";
import { desktopApi, web } from "./lib/environment";
import { router } from "./router";

export function Renderer(): React.JSX.Element {
  // Browsers keep their own context menu, reload and drop behavior.
  useEffect(() => web ? undefined : installNativeInteractions(desktopApi), []);
  return <QueryClientProvider client={queryClient}><TooltipProvider><ConfirmProvider>
    <RouterProvider router={router} />
  </ConfirmProvider></TooltipProvider></QueryClientProvider>;
}
