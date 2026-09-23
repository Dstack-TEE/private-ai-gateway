import { useId, useLayoutEffect, useRef } from "react";
import { createRoot } from "react-dom/client";
import { Button } from "./ui/button";

type BrowserDialogOptions = {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel?: string;
};

export function showBrowserDialog(options: BrowserDialogOptions): Promise<boolean> {
  return new Promise((resolve) => {
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    let settled = false;
    const finish = (confirmed: boolean) => {
      if (settled) return;
      settled = true;
      root.unmount();
      container.remove();
      resolve(confirmed);
    };
    root.render(<BrowserDialog options={options} onClose={finish} />);
  });
}

// Sheets are native modal dialogs, so only another native modal can appear above them.
function BrowserDialog({ options, onClose }: { options: BrowserDialogOptions; onClose(confirmed: boolean): void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const messageId = useId();
  useLayoutEffect(() => {
    const node = dialog.current;
    if (!node) return;
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    node.showModal();
    const cancel = (event: Event) => {
      event.preventDefault();
      onClose(false);
    };
    node.addEventListener("cancel", cancel);
    return () => {
      node.removeEventListener("cancel", cancel);
      opener?.focus({ preventScroll: true });
    };
  }, [onClose]);
  return <dialog ref={dialog} aria-labelledby={titleId} aria-describedby={messageId} className="fixed left-1/2 top-1/2 m-0 w-[min(410px,_calc(100vw_-_32px))] -translate-x-1/2 -translate-y-1/2 rounded-2xl border border-border bg-background p-5 text-foreground shadow-xl focus:outline-none [&::backdrop]:bg-[var(--backdrop)]">
    <h2 id={titleId} className="text-lg font-semibold">{options.title}</h2>
    <p id={messageId} className="mt-2 whitespace-pre-line text-sm text-muted-foreground">{options.message}</p>
    <div className="mt-5 flex items-center justify-end gap-3">
      {options.cancelLabel && <Button variant="outline" onClick={() => onClose(false)}>{options.cancelLabel}</Button>}
      <Button onClick={() => onClose(true)}>{options.confirmLabel}</Button>
    </div>
  </dialog>;
}
