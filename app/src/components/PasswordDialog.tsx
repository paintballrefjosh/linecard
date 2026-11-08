import { useEffect, useState } from "react";
import { getBackend } from "../api";
import type { PasswordTarget } from "../types";
import { Modal } from "./Dialog";

interface Props {
  connectionId: string;
  name: string;
  onClose(): void;
}

/** Save (or replace) a connection's password without logging in first. The
 *  password goes straight to the OS keychain; it is never shown again. */
export function PasswordDialog({ connectionId, name, onClose }: Props) {
  const [target, setTarget] = useState<PasswordTarget | null>(null);
  const [user, setUser] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void getBackend()
      .then((b) => b.passwordTarget(connectionId))
      .then((t) => {
        if (!live) return;
        setTarget(t);
        setUser(t.user ?? "");
      })
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
    };
  }, [connectionId]);

  const save = async () => {
    try {
      const b = await getBackend();
      await b.setConnectionPassword(connectionId, target?.needsUser ? user : null, password);
      onClose();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <Modal
      title={`Saved password for ${name}`}
      className="modal-confirm"
      onSubmit={save}
      onClose={onClose}
      actions={
        <>
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={!target || !password}>
            Save
          </button>
        </>
      }
    >
      {target?.saved && <p>A password is already saved; this replaces it.</p>}
      <div className="form-grid">
        {target?.needsUser && (
          <>
            <label htmlFor="pw-user">User name</label>
            <input id="pw-user" value={user} onChange={(e) => setUser(e.target.value)} autoComplete="off" />
          </>
        )}
        <label htmlFor="pw-pass">Password</label>
        <input
          id="pw-pass"
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          autoComplete="new-password"
          data-autofocus
        />
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
