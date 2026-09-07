/** Wait for resources used by the committed dialog, not an arbitrary delay. */
export async function prepareDialogPresentation() {
  await document.fonts.ready;
  await Promise.all(Array.from(document.querySelectorAll<HTMLImageElement>(".native-dialog-host img"), async (image) => {
    try { await image.decode(); }
    catch { /* An unavailable decorative image must not prevent closing the dialog. */ }
  }));
  // Hidden WebViews may suspend animation frames. Resolve layout synchronously
  // instead of waiting on requestAnimationFrame before the native window is shown.
  document.querySelector(".native-dialog-host")?.getBoundingClientRect();
}

/** Only called after the native host permits visible rendering. */
export function waitForDialogFrames(signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    let frame = 0;
    const done = () => {
      cancelAnimationFrame(frame);
      document.removeEventListener("visibilitychange", visible);
      signal.removeEventListener("abort", done);
      resolve();
    };
    const visible = () => {
      if (document.visibilityState !== "visible" || frame) return;
      frame = requestAnimationFrame(() => { frame = requestAnimationFrame(done); });
    };
    signal.addEventListener("abort", done, { once: true });
    document.addEventListener("visibilitychange", visible);
    if (signal.aborted) done(); else visible();
  });
}
