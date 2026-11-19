"""Minimal third-party Sphinx-like extension worker for scribe Phase 27 tests.

Speaks the line-delimited JSON-RPC protocol documented in
`scribe_core::python_shim`. Registers a `source-read` handler and a `pybadge`
directive so integration tests can observe both callbacks in built output.
"""

import json
import sys

EVENTS = ["source-read", "builder-inited"]
DIRECTIVES = ["pybadge"]


def on_event(params):
    event = params.get("event")
    payload = params.get("payload", {})
    if event == "source-read":
        source = payload.get("source", "")
        return {"source": source.replace("<<MARKER>>", "PYTHON-WAS-HERE")}
    if event == "builder-inited":
        return {"builder": payload.get("builder_name", "")}
    return None


def directive(params):
    name = params.get("name")
    if name == "pybadge":
        args = params.get("arguments", [])
        label = args[0] if args else "badge"
        return {"text": "PYTHON_DIRECTIVE_OUTPUT[" + label + "]", "classes": []}
    return {"text": ""}


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        request = json.loads(line)
    except Exception:
        continue
    method = request.get("method")
    rid = request.get("id")
    params = request.get("params", {})
    try:
        if method == "initialize":
            result = {"events": EVENTS, "directives": DIRECTIVES, "roles": []}
        elif method == "on_event":
            result = on_event(params)
        elif method == "directive":
            result = directive(params)
        elif method == "role":
            result = {"text": ""}
        elif method == "shutdown":
            result = None
        else:
            result = None
        response = {"jsonrpc": "2.0", "id": rid, "result": result}
    except Exception as exc:
        response = {"jsonrpc": "2.0", "id": rid, "error": {"message": str(exc)}}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
    if method == "shutdown":
        break
