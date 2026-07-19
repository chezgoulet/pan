"""pan — the only sanctioned channel a Pan skill has to the outside world.

A skill is a plain Python program. Everything it does to *touch the world* goes
through ``pan.invoke(capability, args)``, which the Pan host runs through its
governed pipeline (resolve -> validate -> govern -> execute) under the scope this
skill was granted. The host hands the subprocess no capability object, so calling
``open()`` / ``socket()`` / ``requests`` directly reaches nothing Pan sanctioned:
``invoke`` is the whole surface. (OS-level denial of ambient fs/network is a
hardening layer the host may add around the process; see the runner docs.)

Protocol note: **stdout is the wire** (newline-delimited JSON between skill and
host). Never ``print()`` to it — use ``pan.log(...)`` (stderr) for debugging.
"""

import json
import os
import sys


class PanError(Exception):
    """An invoke failed. ``kind`` is one of not_found/invalid_args/denied/failed."""

    def __init__(self, kind, message):
        super().__init__(f"{kind}: {message}")
        self.kind = kind
        self.message = message


class PanDenied(PanError):
    """Governance refused this invocation for the skill's scope."""


_counter = 0


def _send(obj):
    sys.stdout.write(json.dumps(obj))
    sys.stdout.write("\n")
    sys.stdout.flush()


def _recv():
    line = sys.stdin.readline()
    if not line:
        raise EOFError("pan host closed the connection")
    return json.loads(line)


def input():
    """The skill's input, passed by the host as JSON (may be ``None``)."""
    return json.loads(os.environ.get("PAN_SKILL_INPUT", "null"))


def invoke(capability, args=None):
    """Ask the host to run a capability; block for its governed result.

    Returns the capability's result value, or raises ``PanDenied`` if governance
    refused it / ``PanError`` for any other stage failure.
    """
    global _counter
    _counter += 1
    rid = _counter
    _send({"type": "invoke", "id": rid, "capability": capability, "args": args or {}})
    resp = _recv()
    if resp.get("type") != "result" or resp.get("id") != rid:
        raise PanError("protocol", f"desynchronized response: {resp!r}")
    if resp.get("ok"):
        return resp.get("value")
    err = resp.get("error") or {}
    kind = err.get("kind", "failed")
    message = err.get("message", "")
    if kind == "denied":
        raise PanDenied(kind, message)
    raise PanError(kind, message)


def done(value=None):
    """Return a final value to the host and finish the skill."""
    _send({"type": "return", "value": value})


def log(*parts):
    """Diagnostic output to stderr (stdout is reserved for the protocol)."""
    sys.stderr.write(" ".join(str(p) for p in parts) + "\n")
    sys.stderr.flush()
