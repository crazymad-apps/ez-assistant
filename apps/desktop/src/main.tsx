import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { configure } from "mobx";
import { App } from "./app/App";
import { RootStore } from "./stores/RootStore";
import "./styles/index.scss";

configure({ enforceActions: "always" });

const root_element = document.querySelector<HTMLDivElement>("#app");
if (!root_element) {
  throw new Error("Desktop app root is missing");
}

document.documentElement.dataset.platform = navigator.platform.toLocaleLowerCase().includes("mac")
  ? "macos"
  : "other";

const store = new RootStore();
createRoot(root_element).render(
  <StrictMode>
    <App store={store} />
  </StrictMode>,
);
