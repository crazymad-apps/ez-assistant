import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { configure } from "mobx";
import { App } from "./app/App";
import { AppearanceStore } from "./stores/AppearanceStore";
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

// 展示偏好先于业务连接与首屏挂载，不随账号或 Host 更换。
const appearance = new AppearanceStore();
void appearance.initialize().then(() => {
  const store = new RootStore({ flush_application_preferences: () => appearance.flushForExit() });
  createRoot(root_element).render(
    <StrictMode>
      <App store={store} appearance={appearance} />
    </StrictMode>,
  );
});
