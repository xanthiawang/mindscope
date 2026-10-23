import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { initApi } from "./lib/api";
import "./styles/globals.css";

// Fetch the local API token first so image URLs can be built synchronously.
initApi().finally(() => {
  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>
  );
});
