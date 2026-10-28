import { useEffect, useRef, useState, type FormEvent } from "react";
import { KeyRound, ShieldAlert, ShieldQuestion } from "lucide-react";
import type { AuthEvent, HostKeyEvent, PromptReply } from "../types";

/** Shown over a terminal while its session waits for the user. Lives in the
 *  tab (not a global modal) so several sessions can each wait on their own. */
export function SessionPrompt({
  prompt,
  onReply,
}: {
  prompt: HostKeyEvent | AuthEvent;
  onReply(r: PromptReply): void;
}) {
  return (
    <div className="session-prompt" role="presentation">
      {prompt.event === "hostKey" ? (
        <HostKeyCard prompt={prompt} onReply={onReply} />
      ) : (
        <AuthCard prompt={prompt} onReply={onReply} />
      )}
    </div>
  );
}

function HostKeyCard({ prompt, onReply }: { prompt: HostKeyEvent; onReply(r: PromptReply): void }) {
  const first = useRef<HTMLButtonElement>(null);
  useEffect(() => first.current?.focus(), []);
  const decide = (decision: "acceptAndSave" | "acceptOnce" | "reject") => onReply({ type: "hostKey", decision });
  const where = prompt.port === 22 ? prompt.host : `${prompt.host}:${prompt.port}`;
  const changed = prompt.changed;

  return (
    <div
      className={`prompt-card ${changed ? "is-danger" : ""}`}
      role="alertdialog"
      aria-labelledby="hk-title"
      aria-describedby="hk-desc"
      onKeyDown={(e) => e.key === "Escape" && decide("reject")}
    >
      <h2 id="hk-title">
        {changed ? <ShieldAlert size={18} /> : <ShieldQuestion size={18} />}
        {changed ? "Host key changed" : "Verify host key"}
      </h2>
      {changed ? (
        <p id="hk-desc">
          The key <b>{where}</b> presented doesn't match the one saved in <code title={changed.path}>known_hosts</code>{" "}
          (line {changed.line}). The device may have been replaced or re-keyed — or someone may be intercepting the
          connection.
        </p>
      ) : (
        <p id="hk-desc">
          First connection to <b>{where}</b>. Check the fingerprint matches the device before trusting it.
        </p>
      )}
      <dl className="fingerprint">
        <dt>{prompt.algorithm}</dt>
        <dd>
          <code>{prompt.fingerprint}</code>
        </dd>
      </dl>
      <div className="prompt-actions">
        {changed ? (
          <>
            <button className="btn btn-danger" onClick={() => decide("acceptAndSave")}>
              Replace saved key
            </button>
            <button className="btn" onClick={() => decide("acceptOnce")}>
              Connect once
            </button>
            <button ref={first} className="btn btn-primary" onClick={() => decide("reject")}>
              Disconnect
            </button>
          </>
        ) : (
          <>
            <button className="btn" onClick={() => decide("reject")}>
              Cancel
            </button>
            <button className="btn" onClick={() => decide("acceptOnce")}>
              Connect once
            </button>
            <button ref={first} className="btn btn-primary" onClick={() => decide("acceptAndSave")}>
              Trust and connect
            </button>
          </>
        )}
      </div>
    </div>
  );
}

function AuthCard({ prompt, onReply }: { prompt: AuthEvent; onReply(r: PromptReply): void }) {
  const [values, setValues] = useState(() => prompt.fields.map(() => ""));
  const first = useRef<HTMLInputElement>(null);
  useEffect(() => first.current?.focus(), []);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    onReply({ type: "fields", values });
  };
  return (
    <form
      className="prompt-card"
      aria-labelledby="auth-title"
      onSubmit={submit}
      onKeyDown={(e) => e.key === "Escape" && onReply({ type: "cancel" })}
    >
      <h2 id="auth-title">
        <KeyRound size={18} />
        {prompt.title}
      </h2>
      {prompt.instructions && <p className="prompt-instructions">{prompt.instructions}</p>}
      {prompt.fields.map((f, i) => (
        <label key={i} className="prompt-field">
          <span>{f.label}</span>
          <input
            ref={i === 0 ? first : undefined}
            type={f.echo ? "text" : "password"}
            value={values[i]}
            onChange={(e) => setValues((v) => v.map((x, j) => (j === i ? e.target.value : x)))}
            autoComplete="off"
            spellCheck={false}
          />
        </label>
      ))}
      <div className="prompt-actions">
        <button type="button" className="btn" onClick={() => onReply({ type: "cancel" })}>
          Cancel
        </button>
        <button type="submit" className="btn btn-primary">
          Log in
        </button>
      </div>
    </form>
  );
}
