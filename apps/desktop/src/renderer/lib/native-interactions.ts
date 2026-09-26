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
  // Linux has no menu bar to hold them, so Ctrl+W closes the window and Ctrl+Q
  // quits (GNOME HIG, Keyboard). The macOS menu bar has its own; WebView2's
  // browser keys (reload, find, print) are turned off in the shell.
  const keyDown = (event: KeyboardEvent) => {
    if (platform !== "linux" || !event.ctrlKey || event.altKey || event.shiftKey || event.metaKey) return;
    const key = event.key.toLowerCase();
    const action = key === "w" ? api.closeWindow : key === "q" ? api.quit : undefined;
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
