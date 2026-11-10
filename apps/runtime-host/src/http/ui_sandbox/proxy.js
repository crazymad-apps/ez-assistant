// 受信任静态中转：只转发绑定的父/子 Window，并为单次页面工具调用标记真实点击。
(() => {
  "use strict";
  const max_html_bytes = 2 * 1024 * 1024;
  const max_message_bytes = 256 * 1024;
  const encoder = new TextEncoder();
  let child = null;
  let retired = false;
  let loaded = false;
  let gesture_channel = null;
  let click_nonce = null;
  let active_click = null;

  function domains(values, websocket = false) {
    if (values === undefined) return [];
    if (!Array.isArray(values) || values.length > 32) throw new Error("invalid CSP domains");
    return values.map((value) => {
      if (typeof value !== "string" || value.length > 2048 || /[\s'"<>;&\\]/.test(value)) throw new Error("invalid origin");
      const url = new URL(value);
      if ((url.protocol !== "https:" && !(websocket && url.protocol === "wss:")) || url.username || url.password || url.search || url.hash || (url.pathname !== "/" && url.pathname !== "")) throw new Error("invalid origin");
      return url.origin;
    });
  }

  function policy(csp = {}) {
    if (!csp || typeof csp !== "object" || Array.isArray(csp)) throw new Error("invalid CSP");
    const resources = domains(csp.resourceDomains).join(" ");
    const connections = domains(csp.connectDomains, true).join(" ") || "'none'";
    const frames = domains(csp.frameDomains).join(" ") || "'none'";
    const base = domains(csp.baseUriDomains).join(" ") || "'none'";
    return `default-src 'none'; script-src 'unsafe-inline' ${resources}; style-src 'unsafe-inline' ${resources}; img-src data: blob: ${resources}; media-src data: blob: ${resources}; font-src data: ${resources}; connect-src ${connections}; frame-src ${frames}; object-src 'none'; base-uri ${base}; form-action 'none'`;
  }

  function retire() {
    if (retired) return;
    retired = true;
    window.parent.postMessage({ jsonrpc: "2.0", method: "ez-assistant/notifications/retired", params: {} }, "*");
    child?.remove();
    window.removeEventListener("message", receive);
  }

  function receive(event) {
    if (retired || (event.source !== window.parent && event.source !== child?.contentWindow)) return;
    const message = event.data;
    if (!message || message.jsonrpc !== "2.0") return;
    const from_parent = event.source === window.parent;
    if (message.method === "ui/notifications/sandbox-resource-ready") {
      if (!from_parent || child) return;
      try {
        const html = message.params?.html;
        if (typeof html !== "string" || encoder.encode(html).length > max_html_bytes) throw new Error("HTML limit");
        const csp = policy(message.params.csp);
        child = document.createElement("iframe");
        child.title = "应用内容";
        child.sandbox = "allow-scripts";
        child.referrerPolicy = "no-referrer";
        child.allow = "camera 'none'; microphone 'none'; geolocation 'none'; clipboard-write 'none'";
        // 第一加载后任何导航都撤销实例；不把新文档接到原 AppBridge。
        child.addEventListener("load", () => { if (loaded) retire(); else loaded = true; });
        const channel = message.params.ez_assistant_gesture_channel;
        if (typeof channel === "string" && /^\d+(?:-\d+){3}$/.test(channel) && channel.length <= 43) {
          gesture_channel = channel;
          click_nonce = crypto.getRandomValues(new Uint32Array(4)).join("-");
        }
        // 先安装监听再移除脚本，nonce 留在闭包中，业务脚本无法伪造点击证明。
        const lifecycle = `<script>(() => {
          const send = window.parent.postMessage.bind(window.parent);
          const nonce = ${JSON.stringify(click_nonce)};
          document.currentScript.remove();
          window.addEventListener('pagehide', () => send({jsonrpc:'2.0',method:'ez-assistant/notifications/retired',params:{}}, '*'), {capture:true,once:true});
          if (nonce === null) return;
          const activation = navigator.userActivation;
          // WebKit 的跨源祖先在收到 postMessage 时可能没有激活态。在点击发生的 Window 取证，
          // 并在运行插件代码之前绑定浏览器原生 getter。
          const isActive = activation && Object.getOwnPropertyDescriptor(Object.getPrototypeOf(activation), 'isActive')?.get?.bind(activation);
          const defer = window.setTimeout.bind(window);
          let sequence = 0;
          window.addEventListener('click', event => {
            if (!event.isTrusted || !isActive?.()) return;
            const id = ++sequence;
            const notify = active => send({jsonrpc:'2.0',method:'ez-assistant/notifications/sandbox-click',params:{nonce,id,active}}, '*');
            notify(true);
            defer(() => notify(false), 0);
          }, true);
        })()<` + "/script>";
        // 自动高度由应用上报。预留滚动条宽度，避免临界高度下滚动条显隐改变排版，
        // 又触发 ResizeObserver 上报新高度；超出宿主高度上限时仍正常滚动。
        const layout = "<style>html{scrollbar-gutter:stable}</style>";
        child.srcdoc = `<!doctype html><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${csp.replaceAll('"', '&quot;')}">${layout}${lifecycle}${html}`;
        document.body.append(child);
      } catch { retire(); }
      return;
    }
    if (message.method === "ez-assistant/notifications/sandbox-click") {
      if (from_parent || click_nonce === null || message.params?.nonce !== click_nonce) return;
      const { id, active } = message.params;
      if (active === true) {
        active_click = { id, expires: performance.now() + 1000 };
      } else if (active_click?.id === id) {
        active_click = null;
      }
      return;
    }
    if (!child || (typeof message.method === "string" && message.method.startsWith("ui/notifications/sandbox-"))) return;
    try { if (encoder.encode(JSON.stringify(message)).length > max_message_bytes) { retire(); return; } } catch { retire(); return; }
    if (from_parent) {
      child.contentWindow.postMessage(message, "*");
      return;
    }
    if (message.method === "tools/call" && "id" in message && active_click) {
      if (performance.now() <= active_click.expires) {
        message.ez_assistant_gesture = gesture_channel;
      }
      active_click = null;
    }
    window.parent.postMessage(message, "*");
  }

  window.addEventListener("message", receive);
  window.addEventListener("pagehide", retire, { once: true });
  // 解析阶段 document.body 尚不存在；模块先安装监听，再在 DOM 可用时公布 ready。
  window.addEventListener("DOMContentLoaded", () => window.parent.postMessage({ jsonrpc: "2.0", method: "ui/notifications/sandbox-proxy-ready", params: {} }, "*"), { once: true });
})();
