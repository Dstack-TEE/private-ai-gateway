import { useEffect, useRef } from "react";
import { useAppearanceReady } from "../components/appearance";

/** Finish layout assets before asking the OS to reveal a loaded webview. */
export function useWindowReady(ready: boolean, present: () => Promise<void>, onError: (error: string) => void) {
  const presented = useRef(false);
  const appearanceReady = useAppearanceReady();
  useEffect(() => {
    if (!ready || !appearanceReady || presented.current) return;
    let active = true;
    const prepare = async () => {
      await document.fonts.ready;
      await Promise.allSettled(Array.from(document.images)
        .filter((image) => image.loading !== "lazy")
        .map((image) => image.decode()));
      if (!active) return;
      await present();
      presented.current = true;
    };
    void prepare().catch(() => {
      if (active) onError("The window could not be displayed. Please try reopening it.");
    });
    return () => { active = false; };
  }, [ready, appearanceReady, present, onError]);
}
