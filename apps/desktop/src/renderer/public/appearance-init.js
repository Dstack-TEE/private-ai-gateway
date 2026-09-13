// Apply the saved native appearance before styles and React load.
const initialTheme = window.__GATEWAY_INITIAL_APPEARANCE__;
document.documentElement.classList.toggle("dark", initialTheme === "dark"
  || (initialTheme !== "light" && matchMedia("(prefers-color-scheme: dark)").matches));
