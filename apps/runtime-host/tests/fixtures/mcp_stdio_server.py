import json
import os
import sys
import time

legacy = len(sys.argv) > 1 and sys.argv[1] == "legacy"

# 仅由隔离验收配置显式开启：验证 stderr 被持续排空且不会转发 credential。
if os.environ.get("MCP_FIXTURE_STDERR_FLOOD"):
    sys.stderr.write(os.environ.get("TOKEN", ""))
    for _ in range(32):
        sys.stderr.write("x" * 65536)
    sys.stderr.flush()


def response(request_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "server/discover":
        if legacy:
            sys.stdout.write(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "error": {"code": -32601, "message": "Method not found"},
                    }
                )
                + "\n"
            )
            sys.stdout.flush()
        else:
            response(
                request["id"],
                {
                    "resultType": "complete",
                    "supportedVersions": ["2026-07-28"],
                    "capabilities": {"tools": {}},
                    "ttlMs": 0,
                    "cacheScope": "private",
                },
            )
    elif method == "initialize":
        response(
            request["id"],
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "stdio-fixture", "version": "1.0"},
            },
        )
    elif method == "tools/list":
        cursor = (request.get("params") or {}).get("cursor")
        if cursor is None:
            response(
                request["id"],
                {
                    "resultType": "complete",
                    "nextCursor": "page-2",
                    "ttlMs": 0,
                    "cacheScope": "private",
                    "tools": [
                        {
                            "name": "first_tool",
                            "description": "first fixture tool",
                            "inputSchema": {"type": "object"},
                        }
                    ],
                },
            )
        else:
            response(
                request["id"],
                {
                    "resultType": "complete",
                    "ttlMs": 0,
                    "cacheScope": "private",
                    "tools": [
                        {
                            "name": "second_tool",
                            "description": "second fixture tool",
                            "inputSchema": {"type": "object"},
                        }
                    ],
                },
            )
    elif method == "tools/call":
        params = request.get("params") or {}
        image = os.environ.get("MCP_FIXTURE_IMAGE")
        audit = os.environ.get("MCP_FIXTURE_AUDIT")
        if audit:
            with open(audit, "a", encoding="utf-8") as record:
                record.write(params.get("name", "") + "\n")
        mode = os.environ.get("MCP_FIXTURE_CALL_MODE")
        if mode == "disconnect":
            sys.exit(0)
        if mode == "hang":
            time.sleep(60)
            continue
        response(
            request["id"],
            {
                "resultType": "complete",
                "content": [
                    {"type": "text", "text": "called:" + params.get("name", "")},
                    {
                        "type": "resource_link",
                        "uri": "https://example.com/result",
                        "name": "fixture-result",
                    },
                ] + ([{"type": "image", "mimeType": "image/png", "data": image}] if image else []),
                "structuredContent": {"arguments": params.get("arguments") or {}},
                "isError": False,
            },
        )
