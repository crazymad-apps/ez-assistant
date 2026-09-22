// 独立非持久 WKWebView；仅访问显式传入的 loopback Vite 夹具。
import AppKit
import WebKit
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let configuration = WKWebViewConfiguration()
configuration.websiteDataStore = .nonPersistent()
final class Probe: NSObject, WKNavigationDelegate {
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
            Task { @MainActor in
                do {
                    let value = try await webView.callAsyncJavaScript("return await window.runFollowProbe()", arguments: [:], in: nil, contentWorld: .page)
                    print(value); exit(0)
                } catch { fputs("FAIL: \(error)\n", stderr); exit(1) }
            }
        }
    }
}
let probe = Probe()
let web = WKWebView(frame: NSRect(x: 0, y: 0, width: 800, height: 600), configuration: configuration)
web.navigationDelegate = probe
let window = NSWindow(contentRect: web.frame, styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "C05 isolated scroll probe"
window.contentView = web
window.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)
guard CommandLine.arguments.count == 2, let url = URL(string: CommandLine.arguments[1]), url.host == "127.0.0.1" else { exit(2) }
web.load(URLRequest(url: url))
DispatchQueue.main.asyncAfter(deadline: .now() + 30) { fputs("FAIL: timeout\n", stderr); exit(1) }
app.run()
