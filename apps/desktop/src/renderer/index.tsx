import React, { useEffect, useState } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { queryClient } from "./lib/query-client";
import { TooltipProvider } from "./components/ui/tooltip";
import { Toaster } from "./components/ui/sonner";
import { AppearanceProvider } from "./components/appearance";
import { ConfirmProvider } from "./components/confirm";
import { NotificationsProvider } from "./components/notifications";
import { installNativeInteractions } from "./lib/native-interactions";
import { desktopApi, web } from "./lib/environment";
import { router } from "./router";

export function Renderer(): React.JSX.Element {
  const [settingsRevision, setSettingsRevision] = useState(0);
  useEffect(() => desktopApi.onSettingsReset(() => {
    queryClient.clear();
    setSettingsRevision((value) => value + 1);
    void router.navigate({ to: "/settings", replace: true, state: { notice: "Settings reset" } });
  }), []);
  // Browsers keep their own context menu, reload and drop behavior.
  useEffect(() => web ? undefined : installNativeInteractions(desktopApi), []);
  return <QueryClientProvider client={queryClient}><TooltipProvider><AppearanceProvider key={settingsRevision} api={desktopApi}><ConfirmProvider>
    <NotificationsProvider api={desktopApi}><RouterProvider router={router} /></NotificationsProvider>
    <Toaster />
  </ConfirmProvider></AppearanceProvider></TooltipProvider></QueryClientProvider>;
}
