import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { LocaleProvider } from "./i18n/locale";
import "./styles/app.css";
import { OfficeDemo } from "./features/ai-office/OfficeDemo";

const officeDemo = new URLSearchParams(window.location.search).get("ai-office") === "demo";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <LocaleProvider>
      {officeDemo ? <OfficeDemo /> : <App />}
    </LocaleProvider>
  </StrictMode>,
);
