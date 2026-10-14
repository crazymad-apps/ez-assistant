import { render as renderComponent, type RenderOptions } from "@testing-library/react";
import { useState, type PropsWithChildren, type ReactNode } from "react";
import { AppearanceProvider } from "../../src/stores/AppearanceContext";
import { AppearanceStore } from "../../src/stores/AppearanceStore";

function TestAppearance({ children }: PropsWithChildren) {
  const [store] = useState(() => new AppearanceStore({ load: () => ({ theme_id: "default_light", recovered_unknown: false }), save: () => undefined }, { apply: () => undefined, applyNative: async () => undefined }));
  return <AppearanceProvider store={store}>{children}</AppearanceProvider>;
}

/** 业务组件测试使用与 App 相同的外观 owner，偏好仅在内存中。 */
export function render(ui: ReactNode, options?: Omit<RenderOptions, "wrapper">) {
  return renderComponent(ui, { ...options, wrapper: TestAppearance });
}
