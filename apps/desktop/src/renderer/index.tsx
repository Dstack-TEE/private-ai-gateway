import React, { useEffect, useState } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { queryClient } from "./lib/query-client";
import { TooltipProvider } from "./components/ui/tooltip";
import { AppearanceProvider } from "./components/appearance";
import { NotificationsProvider } from "./components/notifications";
import { installNativeInteractions } from "./lib/native-interactions";
import { DialogCloseProvider } from "./components/dialog-close";
import { desktopApi, query } from "./lib/environment";
import { NativeWindowContent } from "./windows/native";
import { App } from "./app";

function WindowContent({ reset }: { reset: boolean }): React.JSX.Element {
  return query.has("native-dialog") ? <NativeWindowContent /> : <NotificationsProvider api={desktopApi}><App initialView={reset ? "settings" : "overview"} /></NotificationsProvider>;
}

export function Renderer(): React.JSX.Element {
  const [interactionError, setInteractionError] = useState("");
  const [settingsRevision, setSettingsRevision] = useState(0);
  useEffect(() => desktopApi.onSettingsReset(() => { queryClient.clear(); setSettingsRevision((value) => value + 1); }), []);
  useEffect(() => installNativeInteractions(desktopApi, setInteractionError), []);
  return <QueryClientProvider client={queryClient}><TooltipProvider><AppearanceProvider key={settingsRevision} api={desktopApi}><DialogCloseProvider api={desktopApi}><WindowContent reset={settingsRevision > 0} /></DialogCloseProvider>{interactionError && <span className="sr-only" role="alert">{interactionError}</span>}</AppearanceProvider></TooltipProvider></QueryClientProvider>;
}
