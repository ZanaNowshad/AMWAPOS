import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource-variable/inter";
import "@fontsource-variable/noto-sans-arabic";
import "./styles/app.css";
import "./styles/layout.css";
import App from "./App";
import { applyDocumentLang } from "./i18n";

applyDocumentLang();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
