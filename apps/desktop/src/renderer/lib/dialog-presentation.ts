/** Wait for resources used by the committed dialog, not an arbitrary delay. */
export async function prepareDialogPresentation() {
  await document.fonts.ready;
  await Promise.all(Array.from(document.querySelectorAll<HTMLImageElement>(".native-dialog-host img"), async (image) => {
    try { await image.decode(); }
    catch { /* An unavailable decorative image must not prevent closing the dialog. */ }
  }));
}
