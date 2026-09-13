import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useRef, type PropsWithChildren } from "react";
import type { DesktopApi } from "../../shared/contracts";

type CloseHandler = () => void;
const DialogCloseContext = createContext<(handler: CloseHandler) => () => void>(() => () => {});

/** The topmost sheet owns every native and keyboard close request. */
export function DialogCloseProvider({ api, children }: PropsWithChildren<{ api: Pick<DesktopApi, "onNativeCloseRequest"> }>) {
  const handlers = useRef<CloseHandler[]>([]);
  const register = useCallback((handler: CloseHandler) => {
    handlers.current.push(handler);
    return () => { handlers.current = handlers.current.filter((entry) => entry !== handler); };
  }, []);
  useEffect(() => api.onNativeCloseRequest(() => handlers.current.at(-1)?.()), [api]);
  useEffect(() => {
    const keyDown = (event: KeyboardEvent) => {
      const closeShortcut = event.key.toLowerCase() === "w" && (event.metaKey || event.ctrlKey) && !event.altKey && !event.shiftKey;
      const escapeWithoutDialog = event.key === "Escape" && !document.querySelector("dialog[open]");
      const cancelShortcut = event.key === "." && event.metaKey && !event.shiftKey && !event.altKey;
      if (!closeShortcut && !escapeWithoutDialog && !cancelShortcut) return;
      const handler = handlers.current.at(-1);
      if (!handler) return;
      event.preventDefault();
      handler();
    };
    window.addEventListener("keydown", keyDown);
    return () => window.removeEventListener("keydown", keyDown);
  }, []);
  return <DialogCloseContext.Provider value={register}>{children}</DialogCloseContext.Provider>;
}

export function useDialogClose(onClose: CloseHandler, dismissible: boolean, present = true) {
  const register = useContext(DialogCloseContext);
  const current = useRef({ onClose, dismissible });
  current.current = { onClose, dismissible };
  useLayoutEffect(() => {
    if (!present) return;
    return register(() => { if (current.current.dismissible) current.current.onClose(); });
  }, [register, present]);
}
