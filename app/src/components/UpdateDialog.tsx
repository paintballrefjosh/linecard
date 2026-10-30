import { useState, type ReactElement } from "react";
import type { UpdateInfo } from "../updater";
import { Modal } from "./Dialog";

/** The release notes as written in CHANGELOG.md: "**Heading**" lines and
 *  "- item" lines; `code` and **bold** inline. */
function Notes({ text }: { text: string }) {
  const inline = (s: string) =>
    s
      .split(/(\*\*[^*]+\*\*|`[^`]+`)/g)
      .map((part, i) =>
        part.startsWith("**") ? (
          <b key={i}>{part.slice(2, -2)}</b>
        ) : part.startsWith("`") ? (
          <code key={i}>{part.slice(1, -1)}</code>
        ) : (
          part
        ),
      );
  const blocks: ReactElement[] = [];
  let items: string[] = [];
  const flush = () => {
    if (items.length)
      blocks.push(
        <ul key={blocks.length}>
          {items.map((it, i) => (
            <li key={i}>{inline(it)}</li>
          ))}
        </ul>,
      );
    items = [];
  };
  for (const line of text.split(/\r?\n/)) {
    const t = line.trim();
    if (t.startsWith("- ")) items.push(t.slice(2));
    else {
      flush();
      if (/^\*\*[^*]+\*\*$/.test(t)) blocks.push(<h3 key={blocks.length}>{t.slice(2, -2)}</h3>);
      else if (t) blocks.push(<p key={blocks.length}>{inline(t)}</p>);
    }
  }
  flush();
  return <div className="update-notes">{blocks}</div>;
}

/** Asks before downloading; restarts only when the user says so. */
export function UpdateDialog({ update, current, onClose }: { update: UpdateInfo; current: string; onClose(): void }) {
  const [stage, setStage] = useState<"ask" | "downloading" | "ready">("ask");
  const [progress, setProgress] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const install = async () => {
    setStage("downloading");
    setError(null);
    try {
      await update.install(setProgress);
      setStage("ready");
    } catch (e) {
      setError(String(e));
      setStage("ask");
    }
  };

  return (
    <Modal
      title={`Linecard ${update.version} is available`}
      className="modal-update"
      onSubmit={() => void (stage === "ready" ? update.restart() : install())}
      onClose={onClose}
      actions={
        <>
          {stage === "downloading" && (
            <span className="modal-foot-note">
              Downloading{progress !== null ? ` ${Math.round(progress * 100)}%` : "…"}
            </span>
          )}
          <button type="button" className="btn" onClick={onClose}>
            {stage === "ready" ? "Restart later" : "Not now"}
          </button>
          <button type="submit" className="btn btn-primary" disabled={stage === "downloading"} data-autofocus>
            {stage === "ready" ? "Restart now" : "Download and install"}
          </button>
        </>
      }
    >
      <p className="update-from">
        You have {current}.{" "}
        {stage === "ready"
          ? "The update is installed and takes effect when Linecard restarts. Open sessions will close."
          : "Nothing is downloaded until you choose to install."}
      </p>
      {update.notes.trim() ? <Notes text={update.notes} /> : <p>No release notes.</p>}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
