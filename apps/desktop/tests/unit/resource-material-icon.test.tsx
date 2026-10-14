import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { act } from "react";
import lightIcon from "material-icon-theme/icons/toml_light.svg?url";
import darkIcon from "material-icon-theme/icons/toml.svg?url";
import { AppearanceProvider } from "../../src/stores/AppearanceContext";
import { AppearanceStore } from "../../src/stores/AppearanceStore";
import { ResourceMaterialIcon } from "../../src/features/resource-workspace/ResourceMaterialIcon";

afterEach(cleanup);

it("changes the existing resource icon to its library dark variant without replacing the image", async () => {
  const appearance = new AppearanceStore({ load: () => ({ theme_id: "default_light", recovered_unknown: false }), save: () => undefined }, { apply: () => undefined, applyNative: async () => undefined });
  await appearance.initialize();
  render(<AppearanceProvider store={appearance}><div data-testid="file"><ResourceMaterialIcon name="config.toml" kind="file" className="icon" /></div></AppearanceProvider>);
  const image = screen.getByTestId("file").querySelector("img")!;
  expect(image.getAttribute("src")).toBe(lightIcon);
  act(() => appearance.selectTheme("monokai"));
  await waitFor(() => expect(image.getAttribute("src")).toBe(darkIcon));
  expect(screen.getByTestId("file").querySelector("img")).toBe(image);
  act(() => appearance.selectTheme("solarized_light"));
  await waitFor(() => expect(image.getAttribute("src")).toBe(lightIcon));
  appearance.dispose();
});
