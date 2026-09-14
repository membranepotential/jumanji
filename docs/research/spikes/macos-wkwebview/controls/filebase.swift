// Independent control for the jumanji macOS spike: does a raw WKWebView with a
// default WKWebViewConfiguration load local images from a file:// base URL via
// loadHTMLString:baseURL:? No wry, no objc2, nothing set on the configuration.
import Cocoa
import WebKit

let png = FileManager.default.contents(atPath: CommandLine.arguments[1])!
let tmp = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
let baseDir = tmp.appendingPathComponent("jmnj-swift-base", isDirectory: true)
let otherDir = tmp.appendingPathComponent("jmnj-swift-other", isDirectory: true)
for d in [baseDir, otherDir] {
    try! FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
}
try! png.write(to: baseDir.appendingPathComponent("rel.png"))
try! png.write(to: otherDir.appendingPathComponent("abs.png"))
let absURL = otherDir.appendingPathComponent("abs.png").absoluteString

let html = """
<!doctype html><html><body>
<img id="rel" src="rel.png">
<img id="abs" src="\(absURL)">
<img id="missing" src="missing.png">
</body></html>
"""

// Wait for every image to settle (load or error), then report naturalWidth.
let probe = """
new Promise(resolve => {
  const imgs = [...document.images];
  const settle = i => i.complete ? Promise.resolve() : new Promise(r => { i.onload = r; i.onerror = r; });
  Promise.all(imgs.map(settle)).then(() =>
    resolve(imgs.map(i => i.id + '=' + i.naturalWidth).join(' ') + ' base=' + document.baseURI));
})
"""

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let config = WKWebViewConfiguration()  // untouched defaults
let webview = WKWebView(frame: NSRect(x: 0, y: 0, width: 600, height: 400), configuration: config)
let window = NSWindow(contentRect: webview.frame, styleMask: [.titled], backing: .buffered, defer: false)
window.contentView = webview
window.makeKeyAndOrderFront(nil)

var phase = 0

final class Nav: NSObject, WKNavigationDelegate {
    func webView(_ wv: WKWebView, didFinish navigation: WKNavigation!) {
        wv.callAsyncJavaScript("return await (\(probe));", arguments: [:], in: nil, in: .page) { result in
            switch result {
            case .success(let v): print("phase \(phase) (\(phase == 1 ? "baseURL = file:// dir" : "baseURL = nil")): \(v ?? "nil")")
            case .failure(let e): print("phase \(phase) JS error: \(e)")
            }
            if phase == 1 {
                phase = 2
                wv.loadHTMLString(html, baseURL: nil)
            } else {
                exit(0)
            }
        }
    }
}
let nav = Nav()
webview.navigationDelegate = nav
phase = 1
webview.loadHTMLString(html, baseURL: baseDir)

// Safety net: never hang.
DispatchQueue.main.asyncAfter(deadline: .now() + 15) { print("timeout"); exit(1) }
app.run()
