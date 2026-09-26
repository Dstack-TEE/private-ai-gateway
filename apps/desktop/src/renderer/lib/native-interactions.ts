import type { DesktopApi } from "../../shared/contracts";
import type { Platform } from "./environment";

/** Replace browser navigation surfaces, without intercepting text editing. */
export function installNativeInteractions(api: Pick<DesktopApi, "showEditMenu" | "closeWindow" | "quit">, platform: Platform | undefined): () => void {
  const contextMenu = (event: MouseEvent) => {
    event.preventDefault();
    const field = event.target instanceof Element ? event.target.closest("input, textarea, [contenteditable=true]") : null;
    const input = field instanceof HTMLInputElement || field instanceof HTMLTextAreaElement ? field : undefined;
    const editable = input ? !input.disabled && !input.readOnly : field instanceof HTMLElement && field.isContentEditable;
    const selectedText = Boolean(window.getSelection()?.toString());
    if (!field && !selectedText) return;
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
    // Ctrl+Q quits (GNOME HIG, Keyboard). Like GTK, a layout without Latin
    // letters uses the key's position.
    if (platform !== "linux" || !modified) return;
    const key = /^[a-z]$/i.test(event.key) ? event.key.toLowerCase() : event.code;
    const action = key === "w" || key === "KeyW" ? api.closeWindow : key === "q" || key === "KeyQ" ? api.quit : undefined;
    if (!action) return;
    event.preventDefault();
    void action().catch((error) => console.error("Could not run the window shortcut", error));
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
  window.addEventListener("dragover", fileDrop);
  window.addEventListener("drop", fileDrop);
  window.addEventListener("dragstart", contentDrag);
  return () => {
    window.removeEventListener("contextmenu", contextMenu);
    window.removeEventListener("keydown", keyDown);
    window.removeEventListener("dragover", fileDrop);
    window.removeEventListener("drop", fileDrop);
    window.removeEventListener("dragstart", contentDrag);
  };
}
