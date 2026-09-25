import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

// Follow the OS light/dark preference. The theme tokens live under a `.dark`
// class (see index.css `@variant dark`), so mirror `prefers-color-scheme`
// onto <html> and keep it in sync when the system theme changes.
const prefersDark = window.matchMedia("(prefers-color-scheme: dark)");
const applySystemTheme = (dark: boolean) =>
  document.documentElement.classList.toggle("dark", dark);
applySystemTheme(prefersDark.matches);
prefersDark.addEventListener("change", (e) => applySystemTheme(e.matches));

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
