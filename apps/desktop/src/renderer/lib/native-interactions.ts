import type { DesktopApi } from "../../shared/contracts";
import type { Platform } from "./environment";

/** Whether a point in the window, such as a right-click's, is on the selected text. */
export function pointInSelection(selection: Pick<Selection, "isCollapsed" | "rangeCount" | "getRangeAt"> | null, x: number, y: number): boolean {
  if (!selection || selection.isCollapsed) return false;
  for (let index = 0; index < selection.rangeCount; index += 1) {
    for (const rect of selection.getRangeAt(index).getClientRects()) {
      if (x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom) return true;
    }
  }
  return false;
}

/** Replace browser navigation surfaces, without intercepting text editing. */
export function installNativeInteractions(api: Pick<DesktopApi, "showEditMenu" | "closeWindow" | "quit">, platform: Platform | undefined): () => void {
  const contextMenu = (event: MouseEvent) => {
    event.preventDefault();
    const field = event.target instanceof Element ? event.target.closest("input, textarea, [contenteditable=true]") : null;
    const input = field instanceof HTMLInputElement || field instanceof HTMLTextAreaElement ? field : undefined;
    const editable = input ? !input.disabled && !input.readOnly : field instanceof HTMLElement && field.isContentEditable;
    // Like a native app, the menu belongs to a text field or the selected text under the pointer.
    if (!field && !pointInSelection(window.getSelection(), event.clientX, event.clientY)) return;
    if (field instanceof HTMLElement && document.activeElement !== field) field.focus({ preventScroll: true });
    void api.showEditMenu(editable).catch((error) => console.error("Could not open the text editing menu", error));
  };
  const keyDown = (event: KeyboardEvent) => {
    const modified = event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey;
    // WebView2 may still reload on F5 and Ctrl+R: the shell turns its browser
    // keys off only after the first navigation began (`windows_window.rs`).
    // WKWebView and WebKitGTK bind no reload keys.
    if (platform === "windows" && (event.key === "F5" || (event.ctrlKey && !event.altKey && event.key.toLowerCase() === "r"))) {
      event.preventDefault();
      return;
    }
    // Linux has no menu bar to hold them, so Ctrl+W closes the window and
    // Ctrl+Q quits (GNOME HIG, Keyboard), once per press and never while an
    // input method composes. Like GTK, a layout without Latin letters uses the
    // key's position.
    if (platform !== "linux" || !modified || event.isComposing || event.repeat) return;
    const key = /^[a-z]$/i.test(event.key) ? event.key.toLowerCase() : event.code;
    const action = key === "w" || key === "KeyW" ? api.closeWindow : key === "q" || key === "KeyQ" ? api.quit : undefined;
    if (!action) return;
    event.preventDefault();
    void action().catch((error) => console.error("Could not run the window shortcut", error));
  };
  // A modified or middle click on an in-app link would open it in a new
  // webview window; the window has no tabs or second windows to open.
  const linkClick = (event: MouseEvent) => {
    if ((event.button === 1 || event.ctrlKey || event.shiftKey || event.metaKey || event.altKey)
      && event.target instanceof Element && event.target.closest("a[href]")) event.preventDefault();
  };
  const fileDrop = (event: DragEvent) => {
    if (event.dataTransfer?.types.includes("Files")) event.preventDefault();
  };
  // Dropping a dragged in-app link or image would load it as a new document.
  const contentDrag = (event: DragEvent) => {
    if (event.target instanceof Element && event.target.closest("a[href], img")) event.preventDefault();
  };
  window.addEventListener("contextmenu", contextMenu);
  window.addEventListener("keydown", keyDown);
  window.addEventListener("click", linkClick, true);
  window.addEventListener("auxclick", linkClick, true);
  window.addEventListener("dragover", fileDrop);
  window.addEventListener("drop", fileDrop);
  window.addEventListener("dragstart", contentDrag);
  return () => {
    window.removeEventListener("contextmenu", contextMenu);
    window.removeEventListener("keydown", keyDown);
    window.removeEventListener("click", linkClick, true);
    window.removeEventListener("auxclick", linkClick, true);
    window.removeEventListener("dragover", fileDrop);
    window.removeEventListener("drop", fileDrop);
    window.removeEventListener("dragstart", contentDrag);
  };
}
