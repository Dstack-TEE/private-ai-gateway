// Apply the system appearance before styles and React load; the saved setting follows.
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches);
