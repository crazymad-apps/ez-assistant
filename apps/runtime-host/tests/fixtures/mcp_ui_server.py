"""通用 MCP Apps 协议夹具；不访问业务服务、用户数据或互联网。"""
import json
from pathlib import Path
import sys

HTML = Path(__file__).with_name("mcp_ui_app.html").read_text(encoding="utf-8")
MIME = "text/html;profile=mcp-app"
APP_CALLS = {}

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    params = request.get("params") or {}
    result = None
    if method == "server/discover":
        print(json.dumps({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32601,"message":"legacy fixture"}}), flush=True)
        continue
    if method == "initialize":
        result = {"protocolVersion":"2025-11-25", "capabilities":{"tools":{},"resources":{}}, "serverInfo":{"name":"ui-fixture","version":"1"}}
    elif method == "tools/list":
        result = {"tools":[
            {"name":"show_card","inputSchema":{"type":"object"},"_meta":{"ui":{"resourceUri":"ui://fixture/card"}}},
            {"name":"app_only","inputSchema":{"type":"object"},"_meta":{"ui":{"visibility":["app"]}}},
            {"name":"model_only","inputSchema":{"type":"object"},"_meta":{"ui":{"visibility":["model"]}}},
        ]}
    elif method == "resources/list":
        result = {"resources":[{"uri":"ui://fixture/card", "name":"通用界面", "mimeType":MIME,"_meta":{"ez-assistant/entries":[{"id":"app","title":"MCP fixture app","kind":"app","resource":{"type":"mcp","server":"ui_fixture","uri":"ui://fixture/card"},"input_schema":{"type":"object"},"capabilities":["post-message","set-composer-draft","call-tool"]}]}}]}
    elif method == "resources/read":
        uri = params.get("uri")
        if uri != "ui://fixture/card":
            print(json.dumps({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32602,"message":"unknown resource"}}), flush=True)
            continue
        result = {"contents":[{"uri":uri,"mimeType":MIME,"text":HTML,"_meta":{"ui":{}}}]}
    elif method == "tools/call":
        if params.get("name") == "app_only":
            case = str((params.get("arguments") or {}).get("case", "default"))
            APP_CALLS[case] = APP_CALLS.get(case, 0) + 1
            result = {"content":[{"type":"text","text":f"App calls {APP_CALLS[case]}"}],"_meta":{"private":"tool-private"}}
        else:
            result = {"content":[{"type":"text","text":"safe model result"}],"_meta":{"private":"ui-only"}}
    elif "id" not in request:
        continue
    else:
        print(json.dumps({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"unsupported"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}), flush=True)
