// The one place that resolves the theme. The `dark` class follows the
// appearance in <html data-appearance> ("light" or "dark"), or the system
// setting without one. A classic script before the styles, so a page never
// paints in the wrong theme (the CSP allows only 'self' scripts, so it is a
// file); the React app only writes the attribute.
{
  const root = document.documentElement;
  const media = matchMedia("(prefers-color-scheme: dark)");
  const apply = () => {
    const appearance = root.dataset.appearance;
    root.classList.toggle("dark", appearance ? appearance === "dark" : media.matches);
  };
  media.addEventListener("change", apply);
  new MutationObserver(apply).observe(root, { attributeFilter: ["data-appearance"] });
  apply();
}
