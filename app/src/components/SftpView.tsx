import { useCallback, useEffect, useRef, useState, type DragEvent, type KeyboardEvent, type MouseEvent } from "react";
import {
  ArrowUp,
  Download,
  File as FileIcon,
  Folder,
  FolderPlus,
  Link2,
  Lock,
  Pencil,
  RefreshCw,
  Trash2,
  Upload,
} from "lucide-react";
import { getBackend, type SftpHandle } from "../api";
import type { AuthEvent, ConnectionNode, HostKeyEvent, PromptReply, SftpEntry } from "../types";
import { ConfirmDialog, Modal } from "./Dialog";
import { relativeTime } from "./ConnectionTree";
import { SessionPrompt } from "./SessionPrompt";
import { collectDrop, hasFiles, stageDrop } from "../staging";
import { formatBytes, type SessionStatus } from "./TerminalView";

interface Props {
  conn: ConnectionNode;
  onStatus(status: SessionStatus): void;
}

const join = (dir: string, name: string) => (dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`);
const parentOf = (path: string) => path.replace(/\/[^/]*\/?$/, "") || "/";

/** "rwxr-xr-x" from the permission bits. */
export function modeText(mode: number | null): string {
  if (mode === null) return "";
  return "rwxrwxrwx"
    .split("")
    .map((c, i) => (mode & (1 << (8 - i)) ? c : "-"))
    .join("");
}

type Busy = { label: string; bytes: number } | null;

/** A remote folder browser over SFTP: browse, upload, download, rename,
 *  create folders and delete. */
export function SftpView({ conn, onStatus }: Props) {
  const handle = useRef<SftpHandle | null>(null);
  const [prompt, setPrompt] = useState<HostKeyEvent | AuthEvent | null>(null);
  const [state, setState] = useState<"connecting" | "ready" | "failed">("connecting");
  const [failure, setFailure] = useState("");
  const [attempt, setAttempt] = useState(0);
  const [path, setPath] = useState("/");
  const [typed, setTyped] = useState("/");
  const [entries, setEntries] = useState<SftpEntry[]>([]);
  const [selected, setSelected] = useState<string[]>([]);
  const anchor = useRef<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<Busy>(null);
  const [asking, setAsking] = useState<"mkdir" | "rename" | "delete" | "chmod" | null>(null);
  /** Repeats the transfer that stopped short, continuing what was copied. */
  const [retry, setRetry] = useState<(() => void) | null>(null);
  const statusRef = useRef(onStatus);
  statusRef.current = onStatus;

  const show = useCallback(async (to: string) => {
    const h = handle.current;
    if (!h) return;
    try {
      const abs = await h.resolve(to);
      const list = await h.list(abs);
      setPath(abs);
      setTyped(abs);
      setEntries(list);
      setSelected([]);
      setError(null);
    } catch (e) {
      setTyped(path);
      setError(String(e));
    }
  }, [path]);

  useEffect(() => {
    let closed = false;
    setState("connecting");
    statusRef.current("connecting");
    void getBackend().then((b) => {
      if (closed) return;
      const { handle: h, opened } = b.openSftp(conn, (e) => {
        if (closed) return;
        if (e.event === "hostKey" || e.event === "auth") setPrompt(e);
      });
      handle.current = h;
      opened
        .then(async (home) => {
          if (closed) return;
          setPrompt(null);
          const list = await h.list(home);
          setPath(home);
          setTyped(home);
          setEntries(list);
          setState("ready");
          statusRef.current("connected");
        })
        .catch((e) => {
          if (closed) return;
          setPrompt(null);
          setFailure(String(e));
          setState("failed");
          statusRef.current("disconnected");
        });
    });
    return () => {
      closed = true;
      handle.current?.close();
      handle.current = null;
    };
  }, [conn, attempt]);

  const reply = (r: PromptReply) => {
    setPrompt(null);
    handle.current?.reply(r);
  };

  const byName = (name: string) => entries.find((e) => e.name === name);
  const picked = selected.map(byName).filter((e): e is SftpEntry => !!e);

  /** Run one operation; null if it worked, else what went wrong. */
  const run = async (label: string, f: (h: SftpHandle, progress: (n: number) => void) => Promise<void>) => {
    const h = handle.current;
    if (!h) return "not connected";
    setBusy({ label, bytes: 0 });
    setError(null);
    try {
      await f(h, (bytes) => setBusy({ label, bytes }));
      return null;
    } catch (e) {
      setError(String(e));
      return String(e);
    } finally {
      setBusy(null);
    }
  };

  const refresh = () => void show(path);
  const open = (e: SftpEntry) => {
    if (e.kind === "dir" || e.kind === "link") void show(join(path, e.name));
  };

  /** A transfer that can be canceled; when it stops short, "Resume" repeats it
   *  continuing the files already partly copied. */
  const transfer = async (
    label: string,
    f: (h: SftpHandle, resume: boolean, progress: (n: number) => void) => Promise<void>,
    resume = false,
  ): Promise<void> => {
    setRetry(null);
    const failure = await run(label, (h, p) => f(h, resume, p));
    if (failure === null) return refresh();
    setRetry(() => () => void transfer(label, f, true));
    // Show what was copied, then say why it stopped (a refresh clears messages).
    await show(path);
    setError(failure);
  };

  const download = async () => {
    if (!picked.length) return;
    const b = await getBackend();
    const dest = await b.pickFolder("Save to folder");
    if (!dest) return;
    const remote = picked.map((e) => join(path, e.name));
    await transfer(`Downloading ${picked.length === 1 ? picked[0].name : `${picked.length} items`}`, (h, resume, p) =>
      h.download(remote, dest, resume, p),
    );
  };
  const upload = async (folder: boolean) => {
    const b = await getBackend();
    const local = folder
      ? await b.pickFolder("Folder to upload").then((f) => (f ? [f] : []))
      : await b.pickFiles("Files to upload");
    if (!local.length) return;
    const to = path;
    await transfer(
      `Uploading ${local.length === 1 ? (local[0].split(/[\\/]/).pop() ?? "") : `${local.length} items`}`,
      (h, resume, p) => h.upload(local, to, resume, p),
    );
  };

  /** Files dragged in from the desktop: staged on disk, then uploaded here. */
  const [dropping, setDropping] = useState(false);
  const onDrop = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    setDropping(false);
    if (busy || state !== "ready") return;
    const to = path;
    // The browser invalidates the drop once this handler returns: read it now.
    const dropped = collectDrop(e.dataTransfer);
    void dropped.then(async (items) => {
      const tops = new Set(items.map((d) => d.rel.split("/")[0]));
      if (!tops.size) return;
      const label = `Uploading ${tops.size === 1 ? [...tops][0] : `${tops.size} items`}`;
      await transfer(label, async (h, resume, p) => {
        const staged = await stageDrop(items, p);
        try {
          await h.upload(staged.paths, to, resume, p);
        } finally {
          await staged.end().catch(() => {});
        }
      });
    });
  };

  const select = (name: string, e: MouseEvent) => {
    if (e.shiftKey && anchor.current) {
      const names = entries.map((x) => x.name);
      const [a, z] = [names.indexOf(anchor.current), names.indexOf(name)].sort((x, y) => x - y);
      setSelected(names.slice(a, z + 1));
    } else if (e.ctrlKey || e.metaKey) {
      setSelected((s) => (s.includes(name) ? s.filter((x) => x !== name) : [...s, name]));
      anchor.current = name;
    } else {
      setSelected([name]);
      anchor.current = name;
    }
  };
  const onKey = (e: KeyboardEvent) => {
    if ((e.target as Element).closest("input, dialog, button")) return;
    if (e.key === "Delete" && picked.length) setAsking("delete");
    else if (e.key === "F2" && picked.length === 1) setAsking("rename");
    else if (e.key === "Backspace") void show(parentOf(path));
    else if (e.key === "Enter" && picked.length === 1) open(picked[0]);
    else if (e.key === "F5") refresh();
    else return;
    e.preventDefault();
  };

  if (state === "connecting")
    return (
      <div className="sftp-pane">
        <div className="remote-status">Opening SFTP to {conn.endpoint}…</div>
        {prompt && <SessionPrompt prompt={prompt} onReply={reply} />}
      </div>
    );
  if (state === "failed")
    return (
      <div className="sftp-pane">
        <div className="remote-status">
          <p>Could not open SFTP: {failure}</p>
          <button className="btn" onClick={() => setAttempt((n) => n + 1)}>
            Try again
          </button>
        </div>
      </div>
    );

  return (
    <div className="sftp-pane" onKeyDown={onKey}>
      <div className="sftp-bar">
        <button className="icon-btn" onClick={() => void show(parentOf(path))} aria-label="Up one folder" title="Up">
          <ArrowUp size={15} />
        </button>
        <button className="icon-btn" onClick={refresh} aria-label="Refresh" title="Refresh (F5)">
          <RefreshCw size={14} />
        </button>
        <form
          className="sftp-path"
          onSubmit={(e) => {
            e.preventDefault();
            void show(typed);
          }}
        >
          <input value={typed} onChange={(e) => setTyped(e.target.value)} aria-label="Remote folder" spellCheck={false} />
        </form>
        <button className="btn" onClick={() => void upload(false)} disabled={!!busy}>
          <Upload size={13} /> Upload…
        </button>
        <button className="btn" onClick={() => void upload(true)} disabled={!!busy} title="Upload a whole folder">
          Folder…
        </button>
        <button className="btn" onClick={() => void download()} disabled={!!busy || !picked.length}>
          <Download size={13} /> Download
        </button>
        <button className="icon-btn" onClick={() => setAsking("mkdir")} aria-label="New folder" title="New folder">
          <FolderPlus size={15} />
        </button>
        <button
          className="icon-btn"
          onClick={() => setAsking("rename")}
          disabled={picked.length !== 1}
          aria-label="Rename"
          title="Rename (F2)"
        >
          <Pencil size={14} />
        </button>
        <button
          className="icon-btn"
          onClick={() => setAsking("chmod")}
          disabled={!picked.length || !!busy}
          aria-label="Permissions"
          title="Permissions"
        >
          <Lock size={14} />
        </button>
        <button
          className="icon-btn"
          onClick={() => setAsking("delete")}
          disabled={!picked.length}
          aria-label="Delete"
          title="Delete"
        >
          <Trash2 size={14} />
        </button>
      </div>

      <div
        className={`sftp-list ${dropping ? "is-dropping" : ""}`}
        role="grid"
        aria-label={`Files in ${path}`}
        tabIndex={0}
        onDragOver={(e) => {
          if (!hasFiles(e)) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = busy ? "none" : "copy";
          setDropping(true);
        }}
        onDragLeave={(e) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropping(false);
        }}
        onDrop={onDrop}
      >
        <div className="sftp-row sftp-head" role="row">
          <span>Name</span>
          <span>Size</span>
          <span>Modified</span>
          <span>Permissions</span>
        </div>
        {entries.length === 0 && <p className="sftp-empty">This folder is empty.</p>}
        {entries.map((e) => (
          <div
            key={e.name}
            role="row"
            aria-selected={selected.includes(e.name)}
            className={`sftp-row ${selected.includes(e.name) ? "is-selected" : ""}`}
            onClick={(ev) => select(e.name, ev)}
            onDoubleClick={() => open(e)}
          >
            <span className="sftp-name">
              {e.kind === "dir" ? <Folder size={14} /> : e.kind === "link" ? <Link2 size={14} /> : <FileIcon size={14} />}
              {e.name}
            </span>
            <span>{e.kind === "dir" ? "" : formatBytes(e.size)}</span>
            <span>{e.modified ? relativeTime(e.modified * 1000) : ""}</span>
            <span className="sftp-mode">{modeText(e.mode)}</span>
          </div>
        ))}
      </div>

      <div className="sftp-status" role="status">
        {busy ? (
          <>
            {busy.label}… {formatBytes(busy.bytes)}
            <button className="btn sftp-status-btn" onClick={() => handle.current?.cancel()}>
              Cancel
            </button>
          </>
        ) : error ? (
          <>
            <span className="sftp-error">{error}</span>
            {retry && (
              <button className="btn sftp-status-btn" onClick={retry} title="Continue the files already partly copied">
                Resume
              </button>
            )}
          </>
        ) : (
          `${entries.length} ${entries.length === 1 ? "item" : "items"}${picked.length ? ` · ${picked.length} selected` : ""}`
        )}
      </div>

      {asking === "mkdir" && (
        <NameDialog
          title="New folder"
          label="Folder name"
          confirm="Create"
          initial=""
          onClose={() => setAsking(null)}
          onDone={(name) => run("Creating folder", (h) => h.mkdir(join(path, name))).then(refresh)}
        />
      )}
      {asking === "rename" && picked.length === 1 && (
        <NameDialog
          title={`Rename ${picked[0].name}`}
          label="New name"
          confirm="Rename"
          initial={picked[0].name}
          onClose={() => setAsking(null)}
          onDone={(name) => run("Renaming", (h) => h.rename(join(path, picked[0].name), join(path, name))).then(refresh)}
        />
      )}
      {asking === "chmod" && picked.length > 0 && (
        <PermissionsDialog
          items={picked}
          onClose={() => setAsking(null)}
          onDone={(mode, recursive) =>
            run("Changing permissions", async (h) => {
              for (const e of picked) await h.chmod(join(path, e.name), mode, recursive);
            }).then(refresh)
          }
        />
      )}
      {asking === "delete" && (
        <ConfirmDialog
          title={`Delete ${picked.length === 1 ? picked[0].name : `${picked.length} items`}?`}
          message="This removes them from the server (folders with everything inside). It can't be undone."
          confirmLabel="Delete"
          danger
          onConfirm={() =>
            run("Deleting", (h) =>
              h.remove(picked.map((e) => join(path, e.name))),
            ).then(refresh)
          }
          onClose={() => setAsking(null)}
        />
      )}
    </div>
  );
}

/** Ask for one name (new folder, rename). */
function NameDialog({
  title,
  label,
  confirm,
  initial,
  onClose,
  onDone,
}: {
  title: string;
  label: string;
  confirm: string;
  initial: string;
  onClose(): void;
  onDone(name: string): void;
}) {
  const [name, setName] = useState(initial);
  const bad = !name.trim() || name.includes("/");
  return (
    <Modal
      title={title}
      className="modal-confirm"
      onClose={onClose}
      onSubmit={() => {
        if (bad) return;
        onDone(name.trim());
        onClose();
      }}
      actions={
        <>
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={bad}>
            {confirm}
          </button>
        </>
      }
    >
      <div className="form-grid">
        <label htmlFor="sftp-name">{label}</label>
        <input id="sftp-name" value={name} onChange={(e) => setName(e.target.value)} data-autofocus spellCheck={false} />
      </div>
    </Modal>
  );
}

const BITS = [
  ["Owner", 6],
  ["Group", 3],
  ["Others", 0],
] as const;
const KINDS = [
  ["Read", 4],
  ["Write", 2],
  ["Execute", 1],
] as const;

/** Change permissions: a grid of read/write/execute boxes and the same thing as an octal number. */
function PermissionsDialog({
  items,
  onClose,
  onDone,
}: {
  items: SftpEntry[];
  onClose(): void;
  onDone(mode: number, recursive: boolean): void;
}) {
  const first = items[0];
  const [mode, setMode] = useState((first.mode ?? 0o644) & 0o7777);
  const [text, setText] = useState((((first.mode ?? 0o644) & 0o7777).toString(8)).padStart(3, "0"));
  const [recursive, setRecursive] = useState(false);
  const hasDir = items.some((e) => e.kind === "dir");
  const set = (m: number) => {
    setMode(m);
    setText(m.toString(8).padStart(3, "0"));
  };
  const valid = /^[0-7]{3,4}$/.test(text);
  return (
    <Modal
      title={items.length === 1 ? `Permissions of ${first.name}` : `Permissions of ${items.length} items`}
      className="modal-confirm"
      onClose={onClose}
      onSubmit={() => {
        if (!valid) return;
        onDone(parseInt(text, 8), recursive);
        onClose();
      }}
      actions={
        <>
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={!valid}>
            Apply
          </button>
        </>
      }
    >
      <table className="perm-grid">
        <thead>
          <tr>
            <th />
            {KINDS.map(([k]) => (
              <th key={k}>{k}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {BITS.map(([who, shift]) => (
            <tr key={who}>
              <th scope="row">{who}</th>
              {KINDS.map(([k, bit]) => (
                <td key={k}>
                  <input
                    type="checkbox"
                    aria-label={`${who} ${k.toLowerCase()}`}
                    checked={!!(mode & (bit << shift))}
                    onChange={(e) => set(e.target.checked ? mode | (bit << shift) : mode & ~(bit << shift))}
                  />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      <div className="form-grid">
        <label htmlFor="perm-octal">Number</label>
        <input
          id="perm-octal"
          value={text}
          inputMode="numeric"
          maxLength={4}
          spellCheck={false}
          aria-invalid={!valid}
          data-autofocus
          onChange={(e) => {
            setText(e.target.value);
            if (/^[0-7]{3,4}$/.test(e.target.value)) setMode(parseInt(e.target.value, 8));
          }}
        />
        {hasDir && (
          <>
            <label htmlFor="perm-rec">Inside folders</label>
            <label className="perm-rec">
              <input id="perm-rec" type="checkbox" checked={recursive} onChange={(e) => setRecursive(e.target.checked)} />
              Apply to everything inside too (files get the same bits, including execute)
            </label>
          </>
        )}
      </div>
    </Modal>
  );
}
