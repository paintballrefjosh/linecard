"""Linecard scripting: drive the sessions open in Linecard from Python.

    import linecard as lc

    s = lc.active()                          # the tab you are looking at
    s.send_line("show version")
    s.expect("#", timeout=10)
    print(s.before)                          # what the device printed first

    r1 = lc.open("Lab/spine01")              # open a saved connection and wait for it
    r1.send_line("terminal length 0")
    out = r1.run("show ip route", prompt=r"\\S+#\\s*$")

Everything here talks to the running Linecard; run a script from Linecard's
Scripts window. Needs Python 3.8 or newer. See docs/scripting.md.
"""

import base64
import codecs
import json
import os
import re
import socket
import threading
import time

__all__ = [
    "Disconnected",
    "LinecardError",
    "Match",
    "Session",
    "Timeout",
    "active",
    "ask",
    "connections",
    "open",
    "sessions",
]

__version__ = "1"


class LinecardError(Exception):
    """Something went wrong talking to Linecard."""


class Timeout(LinecardError):
    """Nothing matched in time. `buffer` is what was received meanwhile."""

    def __init__(self, message, buffer=""):
        super().__init__(message)
        self.buffer = buffer


class Disconnected(LinecardError):
    """The session ended."""

    def __init__(self, message, reason=None, buffer=""):
        super().__init__(message)
        self.reason = reason
        self.buffer = buffer


# ---------------------------------------------------------------------------
# The link to Linecard

_lock = threading.RLock()
_link = None
_next_id = 0


def _connect():
    port = os.environ.get("LINECARD_SCRIPT_PORT")
    token = os.environ.get("LINECARD_SCRIPT_TOKEN")
    if not port or not token:
        raise LinecardError("this script wasn't started by Linecard; run it from Linecard's Scripts window")
    sock = socket.create_connection(("127.0.0.1", int(port)), timeout=10)
    sock.settimeout(None)
    link = (sock, sock.makefile("rw", encoding="utf-8", newline="\n"))
    _send(link, {"id": 0, "op": "hello", "token": token})
    reply = _receive(link)
    if "error" in reply:
        raise LinecardError(reply["error"])
    return link


def _send(link, message):
    link[1].write(json.dumps(message) + "\n")
    link[1].flush()


def _receive(link):
    line = link[1].readline()
    if not line:
        raise LinecardError("Linecard closed the connection")
    return json.loads(line)


def _call(op, **args):
    global _link, _next_id
    with _lock:
        if _link is None:
            _link = _connect()
        _next_id += 1
        args.update(op=op, id=_next_id)
        try:
            _send(_link, args)
            reply = _receive(_link)
        except OSError as e:
            _link = None
            raise LinecardError("lost the connection to Linecard: %s" % e)
    if "error" in reply:
        kind = reply.get("kind")
        if kind == "timeout":
            raise Timeout(reply["error"])
        if kind == "closed":
            raise Disconnected(reply["error"])
        raise LinecardError(reply["error"])
    return reply["ok"]


# ---------------------------------------------------------------------------
# Matching

# Colors, cursor movement, window titles and the like: what a terminal
# interprets and a script doesn't want to match against.
_ANSI = re.compile(
    r"\x1b(?:\[[0-?]*[ -/]*[@-~]"  # CSI
    r"|\][^\x07\x1b]*(?:\x07|\x1b\\)"  # OSC
    r"|[()*+][0-9A-Za-z]"  # character sets
    r"|[@-Z\\-_=>78])"  # other two-character sequences
)


class Match:
    """A successful expect()."""

    def __init__(self, index, match, before):
        self.index = index  # which pattern matched (0 for a single one)
        self.text = match.group(0)  # the text that matched
        self.before = before  # everything received before it
        self.groups = match.groups()
        self._match = match

    def group(self, *which):
        return self._match.group(*which)

    def __str__(self):
        return self.text

    def __repr__(self):
        return "Match(index=%d, text=%r)" % (self.index, self.text)


def _compile(patterns, regex):
    if isinstance(patterns, (str, re.Pattern)):
        patterns = [patterns]
    out = []
    for p in patterns:
        if isinstance(p, re.Pattern):
            out.append(p)
        elif regex:
            out.append(re.compile(p, re.MULTILINE))
        else:
            out.append(re.compile(re.escape(p)))
    if not out:
        raise ValueError("expect() needs at least one pattern")
    return out


class Session:
    """An open terminal session (SSH, Telnet or serial)."""

    def __init__(self, info, strip_ansi=True):
        self.id = info["id"]
        self.name = info["name"]
        self.kind = info["kind"]
        self.endpoint = info["endpoint"]
        self.connection = info.get("connection")
        self.strip_ansi = strip_ansi
        #: Text that marks "ready for the next command"; used by run().
        self.prompt = None
        #: What came before the last successful expect().
        self.before = ""
        #: The match of the last successful expect().
        self.match = None
        self._buf = ""
        self._hold = ""
        self._decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self._ended = None  # None, or (reason,)

    def __repr__(self):
        return "<Session %r %s>" % (self.name, self.kind)

    # -- sending ----------------------------------------------------------

    def send(self, text):
        """Type text exactly as given (no Enter is added)."""
        data = text if isinstance(text, bytes) else text.encode("utf-8")
        self._call("write", data=base64.b64encode(data).decode("ascii"))

    def send_line(self, text="", eol="\r"):
        """Type text and press Enter."""
        self.send(text + eol)

    def send_control(self, letter):
        """Press Ctrl+letter, e.g. send_control('c')."""
        self.send(bytes([ord(letter.upper()) - 64]))

    # -- receiving --------------------------------------------------------

    def _call(self, op, **args):
        try:
            return _call(op, session=self.id, **args)
        except Disconnected as e:
            self._ended = (None,)
            raise Disconnected(str(e), buffer=self._buf)

    def _fill(self, timeout):
        """Take in more output, waiting up to `timeout` seconds for some.
        True if any arrived (or the session ended)."""
        r = self._call("read", timeout=max(0.0, timeout))
        raw = base64.b64decode(r["data"])
        got = False
        if raw:
            text = self._hold + self._decoder.decode(raw)
            self._hold = ""
            if self.strip_ansi:
                text = self._clean(text)
            self._buf += text
            got = True
        if r.get("ended"):
            self._ended = (r.get("reason"),)
            return True
        return got

    def _clean(self, text):
        # An escape sequence may be cut off at the end of this chunk: keep the
        # start of it back until the rest arrives.
        i = text.rfind("\x1b")
        if i != -1 and len(text) - i < 64 and not _ANSI.match(text, i):
            self._hold = text[i:]
            text = text[:i]
        text = _ANSI.sub("", text)
        return re.sub(r"\r+\n", "\n", text)

    def _disconnected(self):
        reason = self._ended[0] if self._ended else None
        why = ": %s" % reason if reason else ""
        return Disconnected("the session %s ended%s" % (self.name, why), reason, self._buf)

    def expect(self, patterns, timeout=30.0, regex=False):
        """Wait until one of `patterns` appears in the output.

        `patterns` is a string, a compiled regex, or a list of them. Plain
        strings match literally unless regex=True. Returns a Match; what came
        before it is in .before (also kept as session.before). Raises Timeout
        if nothing matched in `timeout` seconds, Disconnected if the session
        ended first. Text up to the match is used up; what follows stays for
        the next call.
        """
        compiled = _compile(patterns, regex)
        deadline = time.monotonic() + timeout
        while True:
            best = None
            for i, rx in enumerate(compiled):
                m = rx.search(self._buf)
                if m and (best is None or m.start() < best[1].start()):
                    best = (i, m)
            if best:
                i, m = best
                before = self._buf[: m.start()]
                self._buf = self._buf[m.end() :]
                self.before = before
                self.match = Match(i, m, before)
                return self.match
            if self._ended is not None:
                raise self._disconnected()
            left = deadline - time.monotonic()
            if left <= 0:
                raise Timeout("timed out after %g s waiting for %s" % (timeout, _describe(patterns)), self._buf)
            self._fill(left)

    def read(self, timeout=0.5):
        """Whatever has arrived: waits up to `timeout` seconds for the first
        output, then returns all that is there (empty if nothing came)."""
        if not self._buf and self._ended is None:
            self._fill(timeout)
        while self._ended is None and self._fill(0):
            pass
        out, self._buf = self._buf, ""
        if not out and self._ended is not None:
            raise self._disconnected()
        return out

    def wait_quiet(self, quiet=1.0, timeout=60.0):
        """Wait until nothing has arrived for `quiet` seconds; returns all of it.
        For commands with no recognizable prompt."""
        deadline = time.monotonic() + timeout
        while self._ended is None:
            left = deadline - time.monotonic()
            if left <= 0:
                raise Timeout("output never went quiet in %g s" % timeout, self._buf)
            if not self._fill(min(quiet, left)):
                break
        out, self._buf = self._buf, ""
        return out

    def flush(self):
        """Throw away everything received so far."""
        self._call("flush")
        self._buf = ""
        self._hold = ""

    def run(self, command, prompt=None, timeout=30.0, regex=True):
        """Send a command, wait for the prompt, and return what it printed.

        `prompt` (or session.prompt) is a regex for the prompt line, e.g.
        r"\\S+[#>]\\s*$". The echoed command and the prompt are left out.
        """
        prompt = prompt or self.prompt
        if prompt is None:
            raise LinecardError("run() needs a prompt: pass prompt=... or set session.prompt")
        self.flush()
        self.send_line(command)
        m = self.expect(prompt, timeout=timeout, regex=regex)
        lines = m.before.split("\n")
        if lines and command.strip() and command.strip() in lines[0]:
            lines = lines[1:]  # the device echoing the command back
        return "\n".join(lines).rstrip("\n")

    # -- other ------------------------------------------------------------

    def screen(self):
        """The text on the terminal screen right now, as a string with one line
        per row (trailing blanks removed): what you would see, including a
        full-screen program, rather than the stream of output."""
        return self._call("screen")["text"]

    def close(self):
        """Close the session (its tab stays, disconnected)."""
        self._call("close")


def _describe(patterns):
    if isinstance(patterns, (str, re.Pattern)):
        patterns = [patterns]
    return ", ".join(repr(p.pattern if isinstance(p, re.Pattern) else p) for p in patterns)


# ---------------------------------------------------------------------------
# Finding and opening sessions


def sessions():
    """Every open terminal session."""
    return [Session(i) for i in _call("sessions")]


def active():
    """The session in the tab you are looking at. Raises LinecardError if
    that isn't a terminal."""
    return Session(_call("active"))


def connections():
    """The saved connections, as dicts with id, name, path, kind and endpoint."""
    return _call("connections")


def open(connection, timeout=60.0):
    """Open a saved connection (by name, folder path like "Lab/spine01", or
    id) in a new tab and wait until it is connected. Host-key and password
    prompts appear in the tab for you to answer. Returns the Session; its
    output includes the login banner."""
    return Session(_call("open", connection=connection, timeout=timeout))


def ask(prompt, secret=False):
    """Ask the person running the script for text. Returns None if they cancel."""
    return _call("ask", prompt=prompt, secret=bool(secret))["value"]
