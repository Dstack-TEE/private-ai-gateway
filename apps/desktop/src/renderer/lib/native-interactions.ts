import type { DesktopApi } from "../../shared/contracts";

/** Replace browser navigation surfaces, without intercepting text editing. */
export function installNativeInteractions(api: Pick<DesktopApi, "showEditMenu">, reportError: (message: string) => void): () => void {
  const contextMenu = (event: MouseEvent) => {
    event.preventDefault();
    const field = event.target instanceof Element ? event.target.closest("input, textarea, [contenteditable=true]") : null;
    const input = field instanceof HTMLInputElement || field instanceof HTMLTextAreaElement ? field : undefined;
    const editable = input ? !input.disabled && !input.readOnly : field instanceof HTMLElement && field.isContentEditable;
    const selectedText = Boolean(window.getSelection()?.toString());
    if (!field && !selectedText) return;
    if (field instanceof HTMLElement && document.activeElement !== field) field.focus({ preventScroll: true });
    void api.showEditMenu(editable).catch(() => reportError("Could not open the text editing menu."));
  };
  const keyDown = (event: KeyboardEvent) => {
    if (event.key === "F5" || ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "r" && !event.altKey)) event.preventDefault();
  };
  const fileDrop = (event: DragEvent) => {
    if (event.dataTransfer?.types.includes("Files")) event.preventDefault();
  };
  window.addEventListener("contextmenu", contextMenu);
  window.addEventListener("keydown", keyDown);
  window.addEventListener("dragover", fileDrop);
  window.addEventListener("drop", fileDrop);
  return () => {
    window.removeEventListener("contextmenu", contextMenu);
    window.removeEventListener("keydown", keyDown);
    window.removeEventListener("dragover", fileDrop);
    window.removeEventListener("drop", fileDrop);
  };
}
