import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";
import { getBackend } from "../api";
import type { Credential, InheritedValue, LogonStep } from "../types";
import { Modal } from "./Dialog";

/** Named credentials, reloaded when one is added. */
export function useCredentials(): [Credential[], () => void] {
  const [list, setList] = useState<Credential[]>([]);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let live = true;
    void getBackend().then((b) => b.listCredentials().then((l) => live && setList(l)));
    return () => {
      live = false;
    };
  }, [tick]);
  return [list, () => setTick((t) => t + 1)];
}

const NEW = "__new__";

/** Pick a named credential, or inherit one. */
export function CredentialSelect({
  id,
  value,
  inherited,
  credentials,
  onChange,
  onCreated,
}: {
  id: string;
  value: string | null;
  inherited: InheritedValue | null;
  credentials: Credential[];
  onChange(v: string | null): void;
  /** A credential was added from here (reload the list). */
  onCreated(): void;
}) {
  const [creating, setCreating] = useState(false);
  return (
    <>
      <select
        id={id}
        value={value ?? ""}
        onChange={(e) => (e.target.value === NEW ? setCreating(true) : onChange(e.target.value || null))}
      >
        <option value="">
          {inherited ? `Inherited: ${inherited.value} (from ${inherited.from})` : "None: the user name below"}
        </option>
        {credentials.map((c) => (
          <option key={c.id} value={c.id}>
            {c.username ? `${c.name} (${c.username})` : c.name}
          </option>
        ))}
        <option value={NEW}>New credential…</option>
      </select>
      {/* Outside the editor's form: a form can't sit inside another. */}
      {creating &&
        createPortal(
          <CredentialDialog
            initial={null}
            onClose={() => setCreating(false)}
            onSaved={(newId) => {
              setCreating(false);
              onCreated();
              onChange(newId);
            }}
          />,
          document.body,
        )}
    </>
  );
}

/** Create or change a named credential. The password is never read back. */
export function CredentialDialog({
  initial,
  onClose,
  onSaved,
}: {
  initial: Credential | null;
  onClose(): void;
  onSaved(id: string): void;
}) {
  const [name, setName] = useState(initial?.name ?? "");
  const [username, setUsername] = useState(initial?.username ?? "");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    try {
      const id = await (
        await getBackend()
      ).saveCredential({ id: initial?.id ?? "", name, username, password: password || null });
      onSaved(id);
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <Modal
      title={initial ? `Edit ${initial.name}` : "New credential"}
      onSubmit={save}
      onClose={onClose}
      actions={
        <>
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary">
            Save
          </button>
        </>
      }
    >
      <div className="form-grid">
        <label htmlFor="cred-name">Name</label>
        <input
          id="cred-name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="NetOps admin"
          data-autofocus
          autoComplete="off"
        />
        <label htmlFor="cred-user">Username</label>
        <input
          id="cred-user"
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          placeholder="Each connection's own"
          spellCheck={false}
          autoComplete="off"
        />
        <label htmlFor="cred-pass">Password</label>
        <input
          id="cred-pass"
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          placeholder={initial?.saved ? "Saved: type to change it" : "Asked at the first login if left empty"}
          autoComplete="new-password"
        />
      </div>
      <p className="form-hint">
        Every connection and folder that uses this credential logs in with it, so changing the password here changes it
        for all of them. The password stays in this computer's keychain, and syncs only if password sync is on.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Steps run after connecting, or inherit the folder's. `null` = inherit. */
export function LogonActionsEditor({
  value,
  inherited,
  credentials,
  onChange,
}: {
  value: LogonStep[] | null;
  inherited: InheritedValue | null;
  credentials: Credential[];
  onChange(v: LogonStep[] | null): void;
}) {
  if (value === null) {
    return (
      <div className="logon">
        <span className="form-hint logon-inherited">
          {inherited ? `Inherited: ${inherited.value} (from ${inherited.from})` : "None"}
        </span>
        <button
          type="button"
          className="btn btn-small"
          onClick={() => onChange([{ expect: "", send: "", passwordOf: null }])}
        >
          {inherited ? "Use different steps here" : "Add a step"}
        </button>
      </div>
    );
  }
  const set = (i: number, patch: Partial<LogonStep>) =>
    onChange(value.map((s, j) => (j === i ? { ...s, ...patch } : s)));
  return (
    <div className="logon">
      {value.map((s, i) => (
        <div className="logon-row" key={i}>
          <input
            aria-label={`Step ${i + 1}: text to wait for`}
            placeholder="Wait for (e.g. >), or leave empty"
            value={s.expect}
            spellCheck={false}
            onChange={(e) => set(i, { expect: e.target.value })}
          />
          <select
            aria-label={`Step ${i + 1}: what to send`}
            value={s.passwordOf ?? ""}
            onChange={(e) => set(i, { passwordOf: e.target.value || null })}
          >
            <option value="">Then send</option>
            {credentials.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}'s password
              </option>
            ))}
          </select>
          {s.passwordOf ? (
            <span className="logon-secret">Sent from the keychain</span>
          ) : (
            <input
              aria-label={`Step ${i + 1}: line to send`}
              placeholder="e.g. enable"
              value={s.send}
              spellCheck={false}
              onChange={(e) => set(i, { send: e.target.value })}
            />
          )}
          <button
            type="button"
            className="icon-btn"
            aria-label={`Remove step ${i + 1}`}
            onClick={() => onChange(value.filter((_, j) => j !== i))}
          >
            <X size={15} />
          </button>
        </div>
      ))}
      <div className="logon-foot">
        <button
          type="button"
          className="btn btn-small"
          onClick={() => onChange([...value, { expect: "", send: "", passwordOf: null }])}
        >
          Add a step
        </button>
        {inherited && (
          <button type="button" className="btn btn-small" onClick={() => onChange(null)}>
            Inherit again
          </button>
        )}
        <span className="form-hint">Each step waits for its text (if any), then sends the line and Enter.</span>
      </div>
    </div>
  );
}
