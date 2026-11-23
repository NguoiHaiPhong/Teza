import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { NotifyToastWindow } from "./components/NotifyToastWindow";
import { I18nProvider } from "./i18n";
import { EmojiStyleProvider } from "./utils/emojiStyleContext";
import "./styles.css";

const isNotifyView =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("view") === "notify";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <I18nProvider>
      <EmojiStyleProvider>
        {isNotifyView ? <NotifyToastWindow /> : <App />}
      </EmojiStyleProvider>
    </I18nProvider>
  </React.StrictMode>,
);
