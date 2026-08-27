import React from "react";
import ReactDOM from "react-dom/client";

// Tokens and base styles must be emitted before any component module, or the
// bundler orders global `.input`/`.btn` rules after them and they win on
// equal specificity — silently undoing per-component padding and colours.
import "./styles/global.css";
import App from "./App";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
