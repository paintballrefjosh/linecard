# Scripting

Scripts are Python files that drive the sessions you have open: type a command, wait for the prompt, read what came back, open the next device. Think of SecureCRT scripts, in Python.

Open **Scripts** in the toolbar (the `</>` button), pick a script and press Run. Its output appears in the window, **Stop** ends it, and a script that asks you a question (a password, say) shows the question there.

## What you need

Python 3.8 or newer on the computer. Linecard doesn't ship one; it finds `python3` (or `py -3`/`python` on Windows) on your PATH. If yours lives elsewhere, enter its path in **Settings → Scripting**.

Each script runs in its own Python process. A mistake in one can't hurt Linecard, and **Stop** really stops it.

## A first script

Create one with **New…** in the Scripts window (it starts from a template) or save this as `versions.py` in the scripts folder (**Open folder** shows where):

```python
import linecard as lc

s = lc.active()              # the terminal tab you used last
s.send_line("show version")  # type it and press Enter
s.expect("#", timeout=10)    # wait for the prompt
print(s.before)              # what the device printed meanwhile
```

Scripts print with `print()`; that is what shows in the window. An error ends the script and shows the traceback.

## Reference

`import linecard as lc`. The module is supplied by Linecard when it runs a script; run any other way, the first call says the script wasn't started by Linecard.

### Finding sessions

| | |
|---|---|
| `lc.active()` | The terminal tab you used last. Raises `LinecardError` if none. |
| `lc.sessions()` | Every open terminal session (SSH, Telnet, serial). |
| `lc.connections()` | The saved connections, as dicts with `id`, `name`, `path`, `kind`, `endpoint`. |
| `lc.open(name, timeout=60)` | Open a saved connection in a new tab, wait until it is connected, and return the session. `name` is the connection's name, its folder path (`"Lab/spine01"`), or its id; case doesn't matter. Host-key and password prompts show in the tab for you to answer, as usual. The login banner is included in what `expect()` can see. |
| `lc.ask(prompt, secret=False)` | Ask you for text; returns `None` if you cancel. Use it for passwords. |

### A session

| | |
|---|---|
| `s.send(text)` | Type text exactly as given. |
| `s.send_line(text)` | Type text and press Enter (`\r`; pass `eol="\n"` to change). |
| `s.send_control("c")` | Press Ctrl+C (any letter). |
| `s.expect(patterns, timeout=30, regex=False)` | Wait for one of `patterns` to appear in the output. Returns a `Match`. |
| `s.read(timeout=0.5)` | Whatever has arrived (waits up to `timeout` for the first of it). |
| `s.wait_quiet(quiet=1.0, timeout=60)` | Wait until nothing has arrived for `quiet` seconds, and return it all. For commands with no recognizable prompt. |
| `s.run(command, prompt=None, timeout=30)` | Send a command, wait for the prompt, return what it printed (without the echoed command or the prompt). `prompt` is a regex; set `s.prompt = r"\S+[#>]\s*$"` once instead of passing it each time. |
| `s.flush()` | Throw away what has been received so far. |
| `s.screen()` | The text on the terminal screen right now, one line per row: what you see, including inside a full-screen program such as `top` or a menu, rather than the stream of output. |
| `s.close()` | Close the session. |
| `s.name`, `s.kind`, `s.endpoint` | What it is: the connection's name, `"ssh"`/`"telnet"`/`"serial"`, and its address. |
| `s.before`, `s.match` | What preceded the last successful `expect()`, and the match itself. |

`expect()` takes a string, a compiled regex (`re.compile(...)`), or a list of either. Plain strings match literally; pass `regex=True` to treat them as patterns. With several patterns the one that appears first in the output wins, and `match.index` says which. The text up to and including the match is used up; what follows stays for the next call.

A `Match` has `.text` (what matched), `.before` (everything before it), `.index`, `.groups` and `.group(n)`.

Matching sees plain text: colors, cursor movement and window titles are removed, and `\r\n` becomes `\n`. Set `s.strip_ansi = False` to see the raw bytes' text instead.

Output is seen from the moment your script first uses a session (or opens it): a prompt printed before that isn't waiting for you. Two things follow. `send_line()` followed by `expect()` always sees the answer, however quickly it comes; and to wait for something that is *already* on the screen, send a harmless Enter and expect the prompt again.

### Errors

| | |
|---|---|
| `lc.Timeout` | `expect()` or `wait_quiet()` ran out of time. `.buffer` holds what arrived meanwhile. |
| `lc.Disconnected` | The session ended. `.reason` says why, if the device or network said. Output the session printed before ending can still be matched first. |
| `lc.LinecardError` | Base of both, and what you get for everything else (no such connection, no active tab…). |

## Examples

Collect the version from several devices:

```python
import linecard as lc

for name in ["Lab/spine01", "Lab/spine02", "Lab/leaf01"]:
    s = lc.open(name)
    s.prompt = r"\S+#\s*$"
    s.run("terminal length 0")
    version = s.run("show version | include uptime")
    print(f"{name}: {version.strip()}")
    s.close()
```

Log in to a device that needs an extra password:

```python
import linecard as lc

s = lc.active()
s.send_line("enable")
s.expect("Password:")
s.send_line(lc.ask("Enable password", secret=True))
s.expect("#")
```

Do something unless the device says no:

```python
try:
    s.expect(["Done", "Error"], timeout=120)
except lc.Timeout as e:
    print("still going after two minutes; last output:", e.buffer[-200:])
```

## How it works, and what it can reach

Linecard starts `python` on your script with a private connection back to the app (a loopback port with a one-time token, closed when the script ends). The script gets the sessions' text and can type into them. It doesn't get your saved passwords, keys or the connection library, and it can't see RDP, VNC or SFTP tabs.

A script is a program that runs as you: it can do anything you can on this computer. Only run scripts you wrote or have read, as with any other.

The latest 512 KB of each session's output is kept for scripts; one that falls further behind than that loses the oldest text.

## Starting scripts without the window

- **Command line.** `linecard run versions.py` (a path, relative to where you typed it) or `linecard run versions` (a script in the scripts folder) opens Linecard's Scripts window and runs it there; if Linecard is already running it uses that window. The command returns at once; the output is in the window.
- **On connect.** In the Scripts window, under **Run on connect**, choose a connection and a script. Each time that connection logs in, the script starts with that tab as `lc.active()`. If another script is still running, a note appears in the tab and nothing starts. Rules are kept per computer (script paths differ between computers) and aren't synced.

## Not (yet) there

- Running a script on a schedule, or without Linecard's window.
- A script editor or debugger inside Linecard; use your own editor.
- More than one script at a time.
