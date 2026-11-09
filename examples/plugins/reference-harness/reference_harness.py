#!/usr/bin/env python3
"""Deterministic view-only harness fixture; it never starts an agent."""
import json
import sys

prepared = {}

def reply(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)

def error(request_id, message):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "error": {"message": message}}), flush=True)

for raw in sys.stdin:
    request = json.loads(raw)
    request_id = request.get("id")
    method = request.get("method")
    params = request.get("params") or {}
    if request_id is None:
        continue
    if method in {"initialize", "snapshot", "event"}:
        reply(request_id, {"accepted": True})
    elif method == "agent/prepare":
        template = params.get("template") or {}
        values = template.get("values") or {}
        target = values.get("reference_target")
        ownership = values.get("reference_ownership")
        key = (params.get("allocationId"), params.get("dispatchId"))
        if not isinstance(target, str) or not target or not isinstance(ownership, str) or ownership not in {"existing", "managed"}:
            error(request_id, "template must name an existing or managed target")
            continue
        prepared[key] = {"target": target, "ownership": ownership}
        reply(request_id, prepared[key])
    elif method == "agent/release":
        key = (params.get("allocationId"), params.get("dispatchId"))
        state = prepared.get(key)
        if state is None or state["target"] != params.get("target"):
            error(request_id, "release does not match the prepared target")
            continue
        del prepared[key]
        reply(request_id, {"released": True})
    elif method == "shutdown":
        reply(request_id, {"accepted": True})
        break
    else:
        error(request_id, "unsupported harness method")
