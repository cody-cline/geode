import React from "react";
import ReactDOM from "react-dom/client";

// Tokens and base styles must be emitted before any component module, or the
// bundler orders global `.input`/`.btn` rules after them and they win on
// equal specificity — silently undoing per-component padding and colours.
import "./styles/global.css";
import App from "./App";
import ErrorBoundary from "./components/ErrorBoundary";
import Palette from "./dev/Palette";

// `?palette` renders the theme on its own. The app cannot be browsed without
// the Tauri backend, so this is the only way to look at a colour change while
// making one. Dev builds only — Vite drops the branch from a release bundle.
const showPalette =
  import.meta.env.DEV && window.location.search.includes("palette");

// The outer boundary, for a throw in the frame rather than in a screen — the
// sidebar, the dock, a toast. The one inside `<main>` catches a screen and
// leaves the sidebar to navigate away with; there is nothing left to navigate
// with out here, so this one only explains and says to reopen Geode. Without
// either of them an uncaught throw unmounts everything and leaves a Tauri
// window with no address bar, no reload and no menu: a blank rectangle whose
// only exit is killing the process.
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary whole>{showPalette ? <Palette /> : <App />}</ErrorBoundary>
  </React.StrictMode>,
);
