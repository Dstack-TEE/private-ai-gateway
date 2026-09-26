// The one place that resolves the theme. The `dark` class follows the
// appearance in <html data-appearance> ("light" or "dark"), or the system
// setting without one. A classic script before the styles, so a page never
// paints in the wrong theme (the CSP allows only 'self' scripts, so it is a
// file); the React app only writes the attribute. In the desktop app it also
// puts the platform and window material the shell injects (see
// distribution.rs) on <html data-platform data-backdrop>, for the platform
// metrics in semantic.css.
{
  const root = document.documentElement;
  const shell = window.__PAP_WINDOW__;
  if (shell) {
    root.dataset.platform = shell.platform;
    if (shell.backdrop) root.dataset.backdrop = shell.backdrop;
  }
  const media = matchMedia("(prefers-color-scheme: dark)");
  const apply = () => {
    const appearance = root.dataset.appearance;
    root.classList.toggle("dark", appearance ? appearance === "dark" : media.matches);
  };
  media.addEventListener("change", apply);
  new MutationObserver(apply).observe(root, { attributeFilter: ["data-appearance"] });
  apply();
}
