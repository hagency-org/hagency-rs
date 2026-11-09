#!/usr/bin/env python3
"""Deterministic fixture plugin for local Hagency acceptance runs."""
import json
import sys


def reply(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)


def notification(method, params):
    print(json.dumps({"jsonrpc": "2.0", "method": method, "params": params}), flush=True)


def runtime():
    dispatch_id = ""
    turn_id = ""
    for raw in sys.stdin:
        request = json.loads(raw)
        request_id = request.get("id")
        method = request.get("method")
        params = request.get("params") or {}
        if method == "initialize":
            reply(request_id, {"protocol": "hagency.runtime/v1", "ready": True})
        elif method == "turn/start":
            dispatch_id = params["dispatchId"]
            turn_id = params["turnId"]
            reply(request_id, {"accepted": True})
            notification("turn/result", {
                "dispatchId": dispatch_id,
                "turnId": turn_id,
                "outcome": "completed",
                "finalText": "PLUGIN_OK",
                "error": None,
                "usage": None,
            })
        elif method == "turn/cancel":
            reply(request_id, {"accepted": True})
        elif method == "shutdown":
            reply(request_id, {"accepted": True})
            return
        else:
            reply(request_id, {"accepted": True})


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    if mode == "startup":
        return
    if mode == "action":
        print(json.dumps({"ok": True, "plugin": "reference.runtime"}))
        return
    if mode == "event":
        return
    if mode == "tool":
        value = json.load(sys.stdin)
        print(json.dumps({"content": [{"type": "text", "text": json.dumps(value, sort_keys=True)}], "isError": False}))
        return
    if mode == "runtime":
        runtime()
        return
    raise SystemExit(f"unsupported mode: {mode}")


if __name__ == "__main__":
    main()
