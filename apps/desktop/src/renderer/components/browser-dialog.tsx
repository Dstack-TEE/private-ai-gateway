import { createRoot } from "react-dom/client";
import { Button } from "./ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "./ui/dialog";

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

function BrowserDialog({ options, onClose }: { options: BrowserDialogOptions; onClose(confirmed: boolean): void }) {
  return <Dialog open onOpenChange={(open) => { if (!open) onClose(false); }}>
    <DialogContent showCloseButton={false}>
      <DialogHeader>
        <DialogTitle>{options.title}</DialogTitle>
        <DialogDescription className="whitespace-pre-line">{options.message}</DialogDescription>
      </DialogHeader>
      <DialogFooter>
        {options.cancelLabel && <Button variant="outline" onClick={() => onClose(false)}>{options.cancelLabel}</Button>}
        <Button onClick={() => onClose(true)}>{options.confirmLabel}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
