// 只访问新建隔离 Host 的匿名页面；使用真实响应 CSP，不注入登录凭据或开放 Host command。
import AppKit
import WebKit

guard CommandLine.arguments.count == 2,
      let url = URL(string: CommandLine.arguments[1]),
      url.scheme == "http", url.host == "127.0.0.1" else {
    fputs("需要显式提供隔离 Host 的 loopback HTTP 地址\n", stderr)
    exit(2)
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let configuration = WKWebViewConfiguration()
configuration.websiteDataStore = .nonPersistent()

final class CspProbe: NSObject, WKNavigationDelegate {
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        Task { @MainActor in
            do {
                let result = try await webView.callAsyncJavaScript("""
                    return await new Promise((resolve) => {
                      const frame = document.createElement('iframe');
                      frame.sandbox = 'allow-scripts allow-same-origin';
                      const timeout = setTimeout(() => resolve({error: '没有观察到策略结果'}), 3000);
                      document.addEventListener('securitypolicyviolation', (event) => {
                        if (event.effectiveDirective === 'frame-src') {
                          clearTimeout(timeout);
                          resolve({blocked: true, directive: event.effectiveDirective});
                        }
                      });
                      window.addEventListener('message', (event) => {
                        if (event.source === frame.contentWindow && event.data?.f03Ready) {
                          clearTimeout(timeout);
                          resolve({blocked: false});
                        }
                      });
                      frame.src = 'data:text/html;base64,' + btoa('<script>parent.postMessage({f03Ready:true},"*")<\\/script>');
                      document.body.append(frame);
                    });
                    """, arguments: [:], in: nil, contentWorld: .page)
                guard let value = result as? [String: Any], value["blocked"] as? Bool == true,
                      value["directive"] as? String == "frame-src" else {
                    fputs("FAIL: 原生 CSP 结果不符合预期：\(String(describing: result))\n", stderr)
                    exit(1)
                }
                print("PASS: macOS WKWebView 在正式 Host 响应 CSP 下拒绝 data 代理")
                exit(0)
            } catch {
                fputs("FAIL: \(error)\n", stderr)
                exit(1)
            }
        }
    }
}

let probe = CspProbe()
let web = WKWebView(frame: NSRect(x: 0, y: 0, width: 800, height: 600), configuration: configuration)
web.navigationDelegate = probe
web.load(URLRequest(url: url))
DispatchQueue.main.asyncAfter(deadline: .now() + 20) {
    fputs("FAIL: 原生 CSP 验证超时\n", stderr)
    exit(1)
}
app.run()
