import "@testing-library/jest-dom/vitest";

// Layout is verified in WebView; jsdom has no element-size observation.
globalThis.ResizeObserver = class {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
};
