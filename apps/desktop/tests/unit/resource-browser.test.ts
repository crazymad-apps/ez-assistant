import { afterEach, describe, expect, it, vi } from "vitest";
import { BrowserController, browserAddress } from "../../src/features/resource-workspace/BrowserController";
import { ResourceWorkspaceStore } from "../../src/features/resource-workspace/ResourceWorkspaceStore";
import * as bridge from "../../src/native-bridge/resourceBrowser";

vi.mock("../../src/native-bridge/resourceBrowser", () => ({
  createResourceBrowser: vi.fn(), navigateResourceBrowser: vi.fn().mockResolvedValue(undefined),
  closeResourceBrowser: vi.fn().mockResolvedValue(undefined), actOnResourceBrowser: vi.fn().mockResolvedValue(undefined),
  resourceBrowserUrl: vi.fn(),
}));
afterEach(() => { vi.clearAllMocks(); vi.useRealTimers(); });

describe("Desktop browser lifetime", () => {
  it("releases a native view whose creation finishes after its tab closes", async () => {
    let finish!: (id: string) => void;
    vi.mocked(bridge.createResourceBrowser).mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    const browser = new BrowserController(vi.fn(), vi.fn());
    browser.navigate("example.com");
    await Promise.resolve();
    browser.dispose();
    finish("native-delayed");
    await vi.waitFor(() => expect(bridge.closeResourceBrowser).toHaveBeenCalledWith("native-delayed"));
  });

  it("keeps browser state across session/tab changes and ignores late page events after close", async () => {
    vi.mocked(bridge.createResourceBrowser).mockResolvedValueOnce("native-one");
    const workspace = new ResourceWorkspaceStore();
    workspace.openBrowser("https://example.com");
    await vi.waitFor(() => expect(bridge.createResourceBrowser).toHaveBeenCalled());
    const events = vi.mocked(bridge.createResourceBrowser).mock.calls[0]![1];
    const browser = workspace.browsers.get("browser-1")!;
    events({ type: "title", title: "Example" });
    events({ type: "loaded", url: "https://example.com/home" });
    workspace.activateTab("context");
    workspace.selectScope("session:another");
    expect(workspace.browsers.get("browser-1")).toBe(browser);
    expect(browser.url).toBe("https://example.com/home");
    events({ type: "notice", message: "请在系统浏览器中下载文件。", url: "https://example.com/download" });
    vi.mocked(bridge.resourceBrowserUrl).mockResolvedValueOnce("https://example.com/home");
    await browser.refreshUrl();
    expect(browser.notice_url).toBe("https://example.com/download");
    workspace.closeTab("browser:browser-1");
    events({ type: "popup", url: "https://example.com/late-popup" });
    expect(workspace.browsers.size).toBe(0);
    await vi.waitFor(() => expect(bridge.closeResourceBrowser).toHaveBeenCalledWith("native-one"));
  });

  it("recovers from creation failure without hiding a slow page at the loading deadline", async () => {
    vi.useFakeTimers();
    vi.mocked(bridge.createResourceBrowser).mockRejectedValueOnce(new Error("creation failed")).mockResolvedValueOnce("native-retry");
    const browser = new BrowserController(vi.fn(), vi.fn());
    browser.navigate("localhost:8080");
    await vi.advanceTimersByTimeAsync(0);
    expect(browser.error).toBe("creation failed");
    browser.navigate("localhost:8080");
    await vi.advanceTimersByTimeAsync(29_999);
    expect(browser.loading).toBe(true);
    expect(browser.load_delayed).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(browser.loading).toBe(false);
    expect(browser.load_delayed).toBe(true);
    expect(browser.error).toBeNull();
    expect(browser.native_id).toBe("native-retry");
    expect(bridge.closeResourceBrowser).not.toHaveBeenCalled();
    vi.mocked(bridge.createResourceBrowser).mock.calls[1]![1]({ type: "loaded", url: "http://localhost:8080/" });
    expect(browser.error).toBeNull();
    expect(browser.load_delayed).toBe(false);
    browser.dispose();
    await vi.advanceTimersByTimeAsync(0);
  });

  it("starts loading on document navigation and cancels its deadline on stop or disposal", async () => {
    vi.useFakeTimers();
    vi.mocked(bridge.createResourceBrowser).mockResolvedValueOnce("native-navigation");
    const browser = new BrowserController(vi.fn(), vi.fn());
    browser.navigate("https://example.com");
    await vi.advanceTimersByTimeAsync(0);
    const receive = vi.mocked(bridge.createResourceBrowser).mock.calls[0]![1];
    receive({ type: "loaded", url: "https://example.com" });
    receive({ type: "title", title: "Updated title" });
    await vi.advanceTimersByTimeAsync(30_000);
    expect(browser.loading).toBe(false);
    expect(browser.load_delayed).toBe(false);
    receive({ type: "load_started", url: "https://example.com/next" });
    expect(browser.loading).toBe(true);
    expect(browser.url).toBe("https://example.com/next");
    browser.perform("stop");
    await vi.advanceTimersByTimeAsync(30_000);
    expect(browser.loading).toBe(false);
    expect(browser.load_delayed).toBe(false);
    browser.perform("reload");
    await vi.advanceTimersByTimeAsync(30_000);
    expect(browser.load_delayed).toBe(true);
    browser.dismissNotice();
    expect(browser.load_delayed).toBe(false);
    browser.perform("reload");
    await vi.advanceTimersByTimeAsync(0);
    browser.dispose();
    receive({ type: "load_started", url: "https://example.com/stale" });
    await vi.advanceTimersByTimeAsync(30_000);
    expect(browser.loading).toBe(false);
    expect(browser.load_delayed).toBe(false);
    expect(browser.url).toBe("https://example.com/next");
  });

  it("normalizes URLs but rejects scripts, local files and embedded credentials", () => {
    expect(browserAddress("example.com/a")).toBe("https://example.com/a");
    expect(browserAddress("localhost:3000")).toBe("http://localhost:3000/");
    for (const address of ["", "javascript:alert(1)", "file:///etc/passwd", "data:text/html,x", "https://user:password@example.com"]) {
      expect(() => browserAddress(address)).toThrow();
    }
  });

  it("keeps the current page and loading state when address input or system opening fails", async () => {
    vi.useFakeTimers();
    vi.mocked(bridge.createResourceBrowser).mockResolvedValueOnce("native-visible");
    const browser = new BrowserController(vi.fn(), vi.fn());
    browser.navigate("https://example.com");
    await vi.advanceTimersByTimeAsync(0);
    const receive = vi.mocked(bridge.createResourceBrowser).mock.calls[0]![1];
    receive({ type: "loaded", url: "https://example.com/" });

    expect(browser.navigate("https://[")).toBe(false);
    expect(browser.notice).toBe("请输入有效的网页地址。");
    expect(browser.error).toBeNull();
    expect(browser.url).toBe("https://example.com/");
    expect(browser.native_id).toBe("native-visible");
    expect(bridge.navigateResourceBrowser).not.toHaveBeenCalled();
    receive({ type: "load_started", url: "https://example.com/next" });
    browser.reportNotice(new Error("系统浏览器打开失败"));
    expect(browser.notice).toBe("系统浏览器打开失败");
    expect(browser.notice_url).toBeNull();
    expect(browser.error).toBeNull();
    expect(browser.loading).toBe(true);
    expect(bridge.closeResourceBrowser).not.toHaveBeenCalled();
    browser.dispose();
    await vi.advanceTimersByTimeAsync(0);
  });

  it("ignores old URL query errors after eviction but still reports errors from the current view", async () => {
    vi.useFakeTimers();
    vi.mocked(bridge.createResourceBrowser).mockResolvedValueOnce("native-old").mockResolvedValueOnce("native-new");
    const browser = new BrowserController(vi.fn(), vi.fn());
    browser.navigate("https://example.com");
    await vi.advanceTimersByTimeAsync(0);
    vi.mocked(bridge.createResourceBrowser).mock.calls[0]![1]({ type: "loaded", url: "https://example.com/" });
    let fail!: (error: Error) => void;
    vi.mocked(bridge.resourceBrowserUrl).mockReturnValueOnce(new Promise((_, reject) => { fail = reject; }));
    const old_query = browser.refreshUrl();
    browser.suspend();
    browser.resume();
    await vi.advanceTimersByTimeAsync(0);
    vi.mocked(bridge.createResourceBrowser).mock.calls[1]![1]({ type: "loaded", url: "https://example.com/" });

    fail(new Error("browser_not_found"));
    await old_query;
    expect(browser.native_id).toBe("native-new");
    expect(browser.error).toBeNull();
    vi.mocked(bridge.resourceBrowserUrl).mockRejectedValueOnce(new Error("current view failed"));
    await browser.refreshUrl();
    expect(browser.error).toBe("current view failed");
    browser.dispose();
    await vi.advanceTimersByTimeAsync(0);
  });

  it("discards pre-navigation URL replies and resumes URL synchronization after document navigation", async () => {
    vi.useFakeTimers();
    vi.mocked(bridge.createResourceBrowser).mockResolvedValueOnce("native-navigation");
    const browser = new BrowserController(vi.fn(), vi.fn());
    browser.navigate("https://example.com/old");
    await vi.advanceTimersByTimeAsync(0);
    const receive = vi.mocked(bridge.createResourceBrowser).mock.calls[0]![1];
    receive({ type: "loaded", url: "https://example.com/old" });
    let finish!: (url: string) => void;
    vi.mocked(bridge.resourceBrowserUrl).mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    const old_query = browser.refreshUrl();

    browser.navigate("https://example.com/new");
    await vi.advanceTimersByTimeAsync(0);
    await browser.refreshUrl();
    expect(bridge.resourceBrowserUrl).toHaveBeenCalledTimes(1);
    finish("https://example.com/old");
    await old_query;
    expect(browser.url).toBe("https://example.com/new");

    receive({ type: "load_started", url: "https://example.com/redirected" });
    receive({ type: "loaded", url: "https://example.com/redirected" });
    vi.mocked(bridge.resourceBrowserUrl).mockResolvedValueOnce("https://example.com/redirected#section");
    await browser.refreshUrl();
    expect(browser.url).toBe("https://example.com/redirected#section");
    browser.dispose();
    await vi.advanceTimersByTimeAsync(0);
  });
});

it("keeps popups with their original session and recreates an evicted browser from its last URL", async () => {
  vi.mocked(bridge.createResourceBrowser).mockResolvedValue("native-cache");
  const store = new ResourceWorkspaceStore();
  store.selectScope("session:a");
  store.openBrowser("https://example.com/a");
  await vi.waitFor(() => expect(bridge.createResourceBrowser).toHaveBeenCalledOnce());
  const receive = vi.mocked(bridge.createResourceBrowser).mock.calls[0]![1];
  const owner = store.active_group;
  const controller = store.browsers.get("browser-1")!;
  receive({ type: "loaded", url: "https://example.com/last" });
  store.selectScope("session:b");
  receive({ type: "popup", url: "https://example.com/popup" });
  expect(store.tabs).toHaveLength(1);
  expect(owner.tabs).toHaveLength(3);
  for (let i = 0; i < 20; i++) {
    store.selectScope(`session:budget-${i}`);
    store.openWorkspace(`session:budget-${i}`);
  }
  await vi.waitFor(() => expect(bridge.closeResourceBrowser).toHaveBeenCalledWith("native-cache"));
  receive({ type: "title", title: "stale" });
  expect(controller.title).not.toBe("stale");
  store.selectScope("session:a");
  store.activateTab("browser:browser-1");
  await vi.waitFor(() => expect(bridge.createResourceBrowser).toHaveBeenCalledWith("https://example.com/last", expect.any(Function)));
  store.dispose();
});
