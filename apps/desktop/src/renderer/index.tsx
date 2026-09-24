import React, { useEffect, useState } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { queryClient } from "./lib/query-client";
import { TooltipProvider } from "./components/ui/tooltip";
import { AppearanceProvider } from "./components/appearance";
import { NotificationsProvider } from "./components/notifications";
import { installNativeInteractions } from "./lib/native-interactions";
import { DialogCloseProvider } from "./components/dialog-close";
import { desktopApi, query, web } from "./lib/environment";
import { NativeWindowContent } from "./windows/native";
import { router } from "./router";

function WindowContent(): React.JSX.Element {
  return query.has("native-dialog") ? <NativeWindowContent /> : <NotificationsProvider api={desktopApi}><RouterProvider router={router} />{web && <NativeWindowContent />}</NotificationsProvider>;
}

export function Renderer(): React.JSX.Element {
  const [settingsRevision, setSettingsRevision] = useState(0);
  useEffect(() => desktopApi.onSettingsReset(() => {
    queryClient.clear();
    setSettingsRevision((value) => value + 1);
    void router.navigate({ to: "/settings", replace: true, state: { notice: "Settings reset" } });
  }), []);
  // Browsers keep their own context menu, reload and drop behavior.
  useEffect(() => web ? undefined : installNativeInteractions(desktopApi), []);
  return <QueryClientProvider client={queryClient}><TooltipProvider><AppearanceProvider key={settingsRevision} api={desktopApi}><DialogCloseProvider api={desktopApi}><WindowContent /></DialogCloseProvider></AppearanceProvider></TooltipProvider></QueryClientProvider>;
}
