import React from "react";
import ReactDOM from "react-dom/client";

// Tokens and base styles must be emitted before any component module, or the
// bundler orders global `.input`/`.btn` rules after them and they win on
// equal specificity — silently undoing per-component padding and colours.
import "./styles/global.css";
import App from "./App";
import Palette from "./dev/Palette";

// `?palette` renders the theme on its own. The app cannot be browsed without
// the Tauri backend, so this is the only way to look at a colour change while
// making one. Dev builds only — Vite drops the branch from a release bundle.
const showPalette =
  import.meta.env.DEV && window.location.search.includes("palette");

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{showPalette ? <Palette /> : <App />}</React.StrictMode>,
);
