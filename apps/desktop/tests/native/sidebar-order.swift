// 非持久 WKWebView，只加载显式指定的新建隔离 Host；使用原生鼠标事件验证产品侧栏。
import AppKit
import WebKit

guard CommandLine.arguments.count == 4, let url = URL(string: CommandLine.arguments[1]),
      url.scheme == "http", url.host == "127.0.0.1" else { exit(2) }
let sourceId = CommandLine.arguments[2]
let referenceId = CommandLine.arguments[3]
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let config = WKWebViewConfiguration()
config.websiteDataStore = .nonPersistent()
let web = WKWebView(frame: NSRect(x: 0, y: 0, width: 1440, height: 1000), configuration: config)
let window = NSWindow(contentRect: web.frame, styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "F04 isolated sidebar probe"
window.contentView = web
window.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)

@MainActor func js(_ code: String) async throws -> Any {
    try await web.callAsyncJavaScript(code, arguments: ["sourceId": sourceId, "referenceId": referenceId], in: nil, contentWorld: .page)
}

@MainActor func mouse(_ type: NSEvent.EventType, _ x: Double, _ y: Double) {
    let point = web.convert(NSPoint(x: x, y: web.isFlipped ? y : web.bounds.height - y), to: nil)
    let event = NSEvent.mouseEvent(with: type, location: point, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: type == .leftMouseUp ? 0 : 1)!
    window.sendEvent(event)
}

@MainActor func pause(_ ms: UInt64 = 80) async throws { try await Task.sleep(nanoseconds: ms * 1_000_000) }

@MainActor func beginDrag(_ x: Double, _ y: Double) async throws {
    mouse(.leftMouseDown, x, y)
    try await pause(150)
    mouse(.leftMouseDragged, x, y + 10)
    try await pause()
    guard (try await js("return !!document.querySelector('[data-sidebar-item][data-sidebar-dragging=\"true\"]')")) as? Bool == true else {
        throw NSError(domain: "native drag did not start", code: 5)
    }
}

final class Probe: NSObject, WKNavigationDelegate {
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        Task { @MainActor in
            do {
                for _ in 0..<100 {
                    if (try await js("return !!document.querySelector('[data-sidebar-item=\"'+sourceId+'\"]')")) as? Bool == true { break }
                    try await pause(100)
                }
                // Web 内容加载后再取得焦点；启动时激活窗口不保证 WebView 已能接收首次点击。
                app.activate(ignoringOtherApps: true)
                window.makeKeyAndOrderFront(nil)
                window.makeFirstResponder(web)
                for _ in 0..<30 {
                    if (try await js("return document.hasFocus()")) as? Bool == true { break }
                    try await pause(100)
                }
                guard (try await js("return document.hasFocus()")) as? Bool == true else {
                    throw NSError(domain: "native probe could not acquire WebView focus", code: 7)
                }
                let points = try await js("""
                  const a = document.querySelector('[data-sidebar-item="'+sourceId+'"]');
                  const b = document.querySelector('[data-sidebar-item="'+referenceId+'"]');
                  if (!a || !b) throw Error('missing test rows');
                  a.scrollIntoView({block:'nearest'});
                  const x = a.getBoundingClientRect(), y = b.getBoundingClientRect();
                  window.f04Moves = 0;
                  const original = window.fetch;
                  window.fetch = (...args) => { if (String(args[1]?.body).includes('"move_sidebar_item"')) window.f04Moves++; return original(...args); };
                  return [x.left+65, x.top+x.height/2, y.left+65, y.top+3];
                """) as! [Double]
                // 小位移仍是原生点击，不保存顺序。
                mouse(.leftMouseDown, points[0], points[1]); mouse(.leftMouseDragged, points[0]+2, points[1]); mouse(.leftMouseUp, points[0]+2, points[1]);
                try await pause(250)
                guard (try await js("return window.f04Moves === 0 && document.querySelector('[data-sidebar-item=\"'+sourceId+'\"]').dataset.selected === 'true'")) as? Bool == true else {
                    print(try await js("return JSON.stringify({ moves: window.f04Moves, selected: document.querySelector('[data-sidebar-item=\"'+sourceId+'\"]').dataset.selected, focused: document.hasFocus(), dragging: !!document.querySelector('[data-sidebar-item][data-sidebar-dragging=\"true\"]') })"))
                    throw NSError(domain:"short click did not select", code:1)
                }
                // 短按时即使超过位移阈值，也不应立即进入拖动。
                mouse(.leftMouseDown, points[0], points[1]); mouse(.leftMouseDragged, points[0], points[1]+10); try await pause()
                guard (try await js("return !document.querySelector('[data-sidebar-item][data-sidebar-dragging=\"true\"]')")) as? Bool == true else { throw NSError(domain:"quick movement started drag", code:6) }
                mouse(.leftMouseUp, points[0], points[1]+10); try await pause()
                // Esc 取消原生拖动。
                try await beginDrag(points[0], points[1])
                let esc = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, characters: "\u{1b}", charactersIgnoringModifiers: "\u{1b}", isARepeat: false, keyCode: 53)!
                window.sendEvent(esc); mouse(.leftMouseUp, points[0], points[1]+10); try await pause()
                guard (try await js("return window.f04Moves === 0 && !document.querySelector('[data-sidebar-dragging=\"true\"]')")) as? Bool == true else { throw NSError(domain:"escape did not cancel", code:2) }
                // 失焦取消；第二个真实窗口接管 key 状态。
                try await beginDrag(points[0], points[1])
                let other = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 100, height: 100), styleMask: [.titled], backing: .buffered, defer: false)
                other.makeKeyAndOrderFront(nil); try await pause(); other.orderOut(nil); window.makeKeyAndOrderFront(nil)
                mouse(.leftMouseUp, points[0], points[1]+10); try await pause()
                guard (try await js("return window.f04Moves === 0 && !document.querySelector('[data-sidebar-dragging=\"true\"]')")) as? Bool == true else { throw NSError(domain:"blur did not cancel", code:3) }
                // 正式拖动提交一次，快照回填后保留选中会话。
                try await beginDrag(points[0], points[1])
                mouse(.leftMouseDragged, points[2], points[3]); try await pause(); mouse(.leftMouseUp, points[2], points[3]);
                for _ in 0..<100 {
                    if (try await js("const row=document.querySelector('[data-sidebar-item=\"'+sourceId+'\"]'); return window.f04Moves === 1 && row.parentElement.firstElementChild === row && row.getAttribute('aria-busy') === 'false'")) as? Bool == true {
                        print("PASS: WKWebView native click, drag delay, Escape, window blur and persisted drag"); exit(0)
                    }
                    try await pause(100)
                }
                print(try await js("return JSON.stringify({moves:window.f04Moves, dragging:!!document.querySelector('[data-sidebar-dragging=\"true\"]'), rows:[...document.querySelectorAll('[data-sidebar-item]')].map(r=>({id:r.dataset.sidebarItem,busy:r.getAttribute('aria-busy')}))})"))
                throw NSError(domain:"native drag was not committed", code:4)
            } catch { fputs("FAIL: \(error)\n", stderr); exit(1) }
        }
    }
}
let probe = Probe()
web.navigationDelegate = probe
web.load(URLRequest(url: url))
DispatchQueue.main.asyncAfter(deadline: .now()+45) { fputs("FAIL: native timeout\n", stderr); exit(1) }
app.run()
