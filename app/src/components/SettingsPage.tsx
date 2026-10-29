import { useEffect, useState, type ReactNode } from "react";
import { Copy, X } from "lucide-react";
import { getBackend } from "../api";
import {
  colorFor,
  defaultRules,
  highlightStore,
  isPalette,
  palette,
  ruleRegex,
  type HighlightProfile,
  type HighlightRule,
} from "../highlight";
import type { ThemePref } from "../theme";
import { usePrefs } from "../prefs";
import type { LoggingSettings, SavedPassword, SyncConfig, SyncTransport } from "../types";
import { ConfirmDialog, Modal } from "./Dialog";
import { relativeTime } from "./ConnectionTree";

interface Props {
  theme: ThemePref;
  onTheme(t: ThemePref): void;
}

/** Per-device settings. Changes apply immediately and are saved as you go. */
export function SettingsPage({ theme, onTheme }: Props) {
  const { prefs, update } = usePrefs();
  return (
    <div className="settings-page">
      <h1>Settings</h1>
      <p className="lede">These apply to this computer only and save as you change them.</p>

      <Section title="Appearance">
        <Row label="Theme" htmlFor="set-theme">
          <Segmented
            id="set-theme"
            value={theme}
            onChange={onTheme}
            options={[
              { value: "system", label: "Match Windows" },
              { value: "light", label: "Light" },
              { value: "dark", label: "Dark" },
            ]}
          />
        </Row>
      </Section>

      <SyncSettings />

      <Section title="Terminal mouse and clipboard">
        <Row
          label="Copy when you highlight text"
          hint="Selecting text in a terminal copies it right away, like SecureCRT and PuTTY."
          htmlFor="set-copy"
        >
          <Toggle id="set-copy" checked={prefs.copyOnSelect} onChange={(v) => update({ copyOnSelect: v })} />
        </Row>
        <Row label="Right-click in a terminal" htmlFor="set-right">
          <Segmented
            id="set-right"
            value={prefs.rightClick}
            onChange={(v) => update({ rightClick: v })}
            options={[
              { value: "paste", label: "Paste" },
              { value: "menu", label: "Show menu" },
            ]}
          />
        </Row>
        <Row
          label="Confirm multi-line pastes"
          hint="Shows the text before pasting more than one line, so a stray paste can't run a batch of commands on a device."
          htmlFor="set-confirm"
        >
          <Toggle
            id="set-confirm"
            checked={prefs.confirmMultilinePaste}
            onChange={(v) => update({ confirmMultilinePaste: v })}
          />
        </Row>
        <p className="settings-note">
          Keyboard: <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> copies, <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd>{" "}
          or <kbd>Shift</kbd>+<kbd>Insert</kbd> pastes.
        </p>
      </Section>

      <KeywordHighlighting />

      <SessionLogging />

      <SavedPasswords />

      <Workspaces />
    </div>
  );
}

/** Set up end-to-end encrypted sync of the connection tree. */
function SyncSettings() {
  const [config, setConfig] = useState<SyncConfig | null | undefined>(undefined);
  const [mode, setMode] = useState<"new" | "join">("new");
  const [transport, setTransport] = useState<SyncTransport>("folder");
  const [location, setLocation] = useState("");
  const [phraseInput, setPhraseInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Shown after starting a group, or on request. */
  const [phrase, setPhrase] = useState<string | null>(null);
  const [confirmOff, setConfirmOff] = useState(false);

  const load = () => void getBackend().then((b) => b.getSyncConfig().then(setConfig));
  useEffect(load, []);
  if (config === undefined) return null;

  const turnOn = async () => {
    setBusy(true);
    setError(null);
    try {
      const b = await getBackend();
      const p = await b.syncSetup(transport, location.trim(), mode === "join" ? phraseInput : null);
      if (mode === "new") setPhrase(p);
      setPhraseInput("");
      load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  if (config) {
    return (
      <Section title="Sync">
        <div className="settings-row">
          <div className="settings-text">
            <span>Syncing through {config.transport === "folder" ? "a shared folder" : "a relay"}</span>
            <p>
              <code>{config.location}</code>
            </p>
          </div>
          <div className="settings-control sync-actions">
            <button className="btn" onClick={() => void getBackend().then((b) => b.syncNow())}>
              Sync now
            </button>
            <button className="btn" onClick={() => setConfirmOff(true)}>
              Turn off
            </button>
          </div>
        </div>
        <div className="settings-row settings-row-wide">
          <div className="settings-text">
            <span>Sync phrase</span>
            <p>
              To add another computer, enter these 24 words there under Settings → Sync → Join with a phrase. Anyone
              with the phrase can read and change your connections, so keep it somewhere safe, like a password manager.
            </p>
            {phrase ? (
              <PhraseWords phrase={phrase} onHide={() => setPhrase(null)} />
            ) : (
              <button
                className="btn sync-show"
                onClick={() => void getBackend().then((b) => b.syncPhrase().then(setPhrase))}
              >
                Show phrase
              </button>
            )}
          </div>
        </div>
        {confirmOff && (
          <ConfirmDialog
            title="Turn off sync?"
            message="This computer stops sending and receiving changes and forgets the sync phrase. Your connections stay here, and your other computers keep syncing with each other."
            confirmLabel="Turn off sync"
            danger
            onConfirm={async () => {
              await (await getBackend()).syncDisable();
              setPhrase(null);
              load();
            }}
            onClose={() => setConfirmOff(false)}
          />
        )}
      </Section>
    );
  }

  return (
    <Section title="Sync">
      <p className="settings-note">
        Keep your connections the same on every computer. Changes are encrypted on this computer before they leave, so
        the folder or relay only ever stores ciphertext. Saved passwords stay in each computer's keychain.
      </p>
      <Row label="This computer" htmlFor="set-sync-mode">
        <Segmented<"new" | "join">
          id="set-sync-mode"
          value={mode}
          onChange={setMode}
          options={[
            { value: "new", label: "Start a new sync group" },
            { value: "join", label: "Join with a phrase" },
          ]}
        />
      </Row>
      <Row
        label="Sync through"
        hint={
          transport === "folder"
            ? "Any folder all your computers can reach: a NAS share, OneDrive, Dropbox, Syncthing."
            : "A Linecard relay server you run (see relay/ in the source). Changes arrive within seconds."
        }
        htmlFor="set-sync-transport"
      >
        <Segmented<SyncTransport>
          id="set-sync-transport"
          value={transport}
          onChange={setTransport}
          options={[
            { value: "folder", label: "Shared folder" },
            { value: "relay", label: "Relay" },
          ]}
        />
      </Row>
      <div className="settings-row settings-row-wide">
        <div className="settings-text">
          <label htmlFor="set-sync-location">{transport === "folder" ? "Folder" : "Relay address"}</label>
          <div className="sync-location">
            <input
              id="set-sync-location"
              className="settings-input"
              value={location}
              placeholder={transport === "folder" ? "\\\\nas\\share\\Linecard" : "https://relay.example.com"}
              spellCheck={false}
              onChange={(e) => setLocation(e.target.value)}
            />
            {transport === "folder" && (
              <button
                className="btn"
                onClick={async () => {
                  const picked = await (await getBackend()).pickFolder("Folder to sync through");
                  if (picked) setLocation(picked);
                }}
              >
                Browse…
              </button>
            )}
          </div>
        </div>
      </div>
      {mode === "join" && (
        <div className="settings-row settings-row-wide">
          <div className="settings-text">
            <label htmlFor="set-sync-phrase">Sync phrase</label>
            <p>The 24 words from a computer already in the group (Settings → Sync → Show phrase there).</p>
            <textarea
              id="set-sync-phrase"
              className="settings-input sync-phrase-input"
              value={phraseInput}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => setPhraseInput(e.target.value)}
            />
          </div>
        </div>
      )}
      <div className="settings-row">
        <div className="settings-text">{error && <p className="form-error">{error}</p>}</div>
        <div className="settings-control">
          <button
            className="btn btn-primary"
            disabled={busy || !location.trim() || (mode === "join" && !phraseInput.trim())}
            onClick={() => void turnOn()}
          >
            {busy ? "Connecting…" : mode === "new" ? "Start syncing" : "Join"}
          </button>
        </div>
      </div>
    </Section>
  );
}

function PhraseWords({ phrase, onHide }: { phrase: string; onHide(): void }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="sync-phrase">
      <ol>
        {phrase.split(" ").map((w, i) => (
          <li key={i}>{w}</li>
        ))}
      </ol>
      <div className="hl-actions">
        <button
          className="btn"
          onClick={() =>
            void getBackend()
              .then((b) => b.clipboardWrite(phrase))
              .then(() => setCopied(true))
          }
        >
          <Copy size={13} /> {copied ? "Copied" : "Copy"}
        </button>
        <button className="btn" onClick={onHide}>
          Hide
        </button>
      </div>
    </div>
  );
}

/** Keyword-highlight profiles (display only: logs and copies are untouched).
 *  Profiles sync; folders and connections pick one in their settings. */
function KeywordHighlighting() {
  const { prefs, update } = usePrefs();
  const [store, setStore] = useState(highlightStore.get());
  useEffect(() => highlightStore.subscribe(() => setStore(highlightStore.get())), []);
  const [selected, setSelected] = useState<string | null>(null);
  const current = store.profiles.find((p) => p.id === selected) ?? store.profiles.find((p) => p.id === store.defaultId);
  /** Edits in progress; saved shortly after typing stops. */
  const [draft, setDraft] = useState<HighlightProfile | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [importOpen, setImportOpen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const theme = document.documentElement.dataset.theme === "light" ? "light" : "dark";

  useEffect(() => setDraft(current ? structuredClone(current) : null), [current]);
  useEffect(() => {
    if (!draft || !current || JSON.stringify(draft) === JSON.stringify(current)) return;
    const t = window.setTimeout(() => {
      void getBackend()
        .then((b) => b.saveHighlightProfile(draft))
        .then(() => setError(null))
        .catch((e) => setError(String(e)));
    }, 600);
    return () => window.clearTimeout(t);
  }, [draft, current]);

  const run = (f: (b: Awaited<ReturnType<typeof getBackend>>) => Promise<unknown>) =>
    void getBackend()
      .then(f)
      .then(() => setError(null))
      .catch((e) => setError(String(e)));
  const create = (p: Omit<HighlightProfile, "id">) =>
    run((b) => b.saveHighlightProfile({ ...p, id: "" }).then(setSelected));
  const edit = (patch: Partial<HighlightProfile>) => setDraft((d) => d && { ...d, ...patch });
  const setRules = (rules: HighlightRule[]) => edit({ rules });

  return (
    <Section title="Keyword highlighting">
      <Row
        label="Color keywords in output"
        hint="Only the display changes; copies and logs keep the original text. Applies on this computer."
        htmlFor="set-hl"
      >
        <Toggle id="set-hl" checked={prefs.highlightKeywords} onChange={(v) => update({ highlightKeywords: v })} />
      </Row>
      <div className="settings-row settings-row-wide">
        <div className="settings-text">
          <label htmlFor="set-hl-profile">Profile</label>
          <p>
            Profiles sync to your other computers. Pick one per folder or connection in its settings (right-click it);
            the default applies everywhere else.
          </p>
          <div className="hl-profile-bar">
            <select id="set-hl-profile" value={current?.id ?? ""} onChange={(e) => setSelected(e.target.value)}>
              {store.profiles.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                  {p.id === store.defaultId ? " (default)" : ""}
                </option>
              ))}
            </select>
            <button className="btn" onClick={() => create({ name: "New profile", matchCase: false, rules: [] })}>
              New
            </button>
            {draft && (
              <>
                <button className="btn" onClick={() => create({ ...draft, name: `${draft.name} copy` })}>
                  Duplicate
                </button>
                <button
                  className="btn"
                  disabled={draft.id === store.defaultId}
                  onClick={() => run((b) => b.setDefaultHighlight(draft.id))}
                >
                  {draft.id === store.defaultId ? "Default" : "Make default"}
                </button>
                <button className="btn" disabled={store.profiles.length < 2} onClick={() => setConfirmDelete(true)}>
                  Delete
                </button>
              </>
            )}
            <button className="btn" onClick={() => setImportOpen(true)}>
              Import from SecureCRT…
            </button>
          </div>
        </div>
      </div>

      {draft && (
        <div className="settings-row settings-row-wide">
          <div className="settings-text">
            <div className="hl-profile-head">
              <input
                className="settings-input hl-name"
                aria-label="Profile name"
                value={draft.name}
                onChange={(e) => edit({ name: e.target.value })}
              />
              <label className="hl-check">
                <input
                  type="checkbox"
                  checked={draft.matchCase}
                  onChange={(e) => edit({ matchCase: e.target.checked })}
                />
                Match case
              </label>
            </div>
            <p>
              Words match whole. Tick <b>Regex</b> for a regular expression. When matches overlap, the rule higher in
              the list wins.
            </p>
            <ul className="hl-rules" aria-label="Keywords">
              {draft.rules.map((r, i) => {
                const broken = r.text.trim() !== "" && !ruleRegex(r, draft.matchCase);
                const change = (patch: Partial<HighlightRule>) =>
                  setRules(draft.rules.map((x, j) => (j === i ? { ...x, ...patch } : x)));
                return (
                  <li key={i}>
                    <input
                      className={`settings-input ${broken ? "is-invalid" : ""}`}
                      value={r.text}
                      aria-label={r.regex ? "Regular expression" : "Keyword"}
                      title={broken ? "This regular expression doesn't work here, so it's skipped" : undefined}
                      spellCheck={false}
                      style={{ color: colorFor(r.color, theme) }}
                      onChange={(e) => change({ text: e.target.value })}
                    />
                    <label className="hl-check" title="Regular expression">
                      <input type="checkbox" checked={r.regex} onChange={(e) => change({ regex: e.target.checked })} />
                      Regex
                    </label>
                    <select
                      aria-label={`Color for ${r.text || "keyword"}`}
                      value={isPalette(r.color) ? r.color : "custom"}
                      onChange={(e) =>
                        change({ color: e.target.value === "custom" ? colorFor(r.color, theme) : e.target.value })
                      }
                    >
                      {palette.map((c) => (
                        <option key={c.value} value={c.value}>
                          {c.label}
                        </option>
                      ))}
                      <option value="custom">Custom</option>
                    </select>
                    {!isPalette(r.color) && (
                      <input
                        type="color"
                        className="hl-color"
                        aria-label="Custom color"
                        value={/^#[0-9a-f]{6}$/i.test(r.color) ? r.color : "#ffffff"}
                        onChange={(e) => change({ color: e.target.value })}
                      />
                    )}
                    <button
                      className="icon-btn"
                      aria-label={`Remove ${r.text || "keyword"}`}
                      onClick={() => setRules(draft.rules.filter((_, j) => j !== i))}
                    >
                      <X size={15} />
                    </button>
                  </li>
                );
              })}
            </ul>
            <div className="hl-actions">
              <button
                className="btn"
                onClick={() => setRules([...draft.rules, { text: "", color: "amber", regex: false }])}
              >
                Add keyword
              </button>
              <button className="btn" onClick={() => setRules(defaultRules)}>
                Reset to defaults
              </button>
            </div>
            {error && <p className="form-error">{error}</p>}
          </div>
        </div>
      )}

      {importOpen && (
        <ImportKeywords
          onClose={() => setImportOpen(false)}
          onImported={(id) => {
            setSelected(id);
            setImportOpen(false);
          }}
        />
      )}
      {confirmDelete && draft && (
        <ConfirmDialog
          title={`Delete ${draft.name}?`}
          message="Folders and connections using it switch to the default profile, on every computer you sync with."
          confirmLabel="Delete profile"
          danger
          onConfirm={async () => {
            await (await getBackend()).deleteHighlightProfile(draft.id);
            setSelected(null);
          }}
          onClose={() => setConfirmDelete(false)}
        />
      )}
    </Section>
  );
}

/** Pick a SecureCRT keyword list (found in its config folder, or browsed to). */
function ImportKeywords({ onClose, onImported }: { onClose(): void; onImported(id: string): void }) {
  const [files, setFiles] = useState<{ name: string; path: string }[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    void getBackend().then((b) => b.securecrtKeywordFiles().then(setFiles));
  }, []);
  const importPath = async (path: string) => {
    try {
      onImported(await (await getBackend()).importHighlight(path));
    } catch (e) {
      setError(String(e));
    }
  };
  const browse = async () => {
    const path = await (await getBackend()).pickFile("open", "SecureCRT keyword list (.ini)");
    if (path) await importPath(path);
  };
  return (
    <Modal
      title="Import a SecureCRT keyword list"
      onSubmit={() => void browse()}
      onClose={onClose}
      actions={
        <>
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn">
            Browse for a file…
          </button>
        </>
      }
    >
      <p>
        Each list becomes a profile with its colors and regular expressions. SecureCRT keeps them in{" "}
        <code>Config\Keywords</code> under its configuration folder.
      </p>
      {files === null ? null : files.length === 0 ? (
        <p className="settings-note settings-empty">
          No keyword lists found in SecureCRT's usual folder on this computer.
        </p>
      ) : (
        <ul className="hl-import-list">
          {files.map((f) => (
            <li key={f.path}>
              <span>{f.name}</span>
              <button type="button" className="btn btn-small" onClick={() => void importPath(f.path)}>
                Import
              </button>
            </li>
          ))}
        </ul>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** App-wide logging defaults. These sync like connections do; folders and
 *  connections can override them. */
function SessionLogging() {
  const [l, setL] = useState<LoggingSettings | null>(null);
  const [folder, setFolder] = useState("");
  useEffect(() => {
    void getBackend().then((b) =>
      b.getLogging().then((v) => {
        setL(v);
        setFolder(v.folder ?? "");
      }),
    );
  }, []);
  if (!l) return null;
  const save = (patch: Partial<LoggingSettings>) => {
    const next = { ...l, ...patch };
    setL(next);
    void getBackend().then((b) => b.setLogging(next));
  };
  return (
    <Section title="Session logging">
      <Row
        label="Log every session"
        hint="Saves what each terminal shows as plain text. A folder or connection can override this."
        htmlFor="set-log"
      >
        <Toggle id="set-log" checked={l.enabled} onChange={(v) => save({ enabled: v })} />
      </Row>
      <Row label="Timestamp each line" htmlFor="set-log-ts">
        <Toggle id="set-log-ts" checked={l.timestamps} onChange={(v) => save({ timestamps: v })} />
      </Row>
      <div className="settings-row settings-row-wide">
        <div className="settings-text">
          <label htmlFor="set-log-dir">Log folder</label>
          <p>
            Files are named <code>name_date_time.log</code>. For your own naming, enter a full path using %name, %host,
            %Y, %m, %d, %H, %M, %S.
          </p>
          <input
            id="set-log-dir"
            className="settings-input"
            value={folder}
            placeholder={l.defaultFolder ?? ""}
            spellCheck={false}
            onChange={(e) => setFolder(e.target.value)}
            onBlur={() => folder !== (l.folder ?? "") && save({ folder: folder.trim() || null })}
          />
        </div>
      </div>
    </Section>
  );
}

/** Passwords saved from login prompts. Secrets stay in the OS keychain;
 *  only labels are listed here. */
function SavedPasswords() {
  const [items, setItems] = useState<SavedPassword[] | null>(null);
  const load = () => void getBackend().then((b) => b.listSavedPasswords().then(setItems));
  useEffect(load, []);
  const forget = (key: string) => void getBackend().then((b) => b.forgetSavedPassword(key).then(load));

  return (
    <Section title="Saved passwords">
      {items === null ? null : items.length === 0 ? (
        <p className="settings-note settings-empty">
          None yet. Tick <b>Save password</b> when logging in to keep one in Windows Credential Manager.
        </p>
      ) : (
        items.map((c) => (
          <div className="settings-row" key={c.key}>
            <div className="settings-text">
              <span className="saved-label">{c.label}</span>
              <p>Saved {relativeTime(c.savedAt)}</p>
            </div>
            <div className="settings-control">
              <button className="btn" onClick={() => forget(c.key)} aria-label={`Remove saved password for ${c.label}`}>
                Remove
              </button>
            </div>
          </div>
        ))
      )}
    </Section>
  );
}

/** Saved workspaces (open them from the toolbar). */
function Workspaces() {
  const { prefs, update } = usePrefs();
  return (
    <Section title="Workspaces">
      {prefs.workspaces.length === 0 ? (
        <p className="settings-note settings-empty">
          None yet. Use the workspaces button in the toolbar to save your open tabs and their layout.
        </p>
      ) : (
        prefs.workspaces.map((w) => (
          <div className="settings-row" key={w.name}>
            <div className="settings-text">
              <span>{w.name}</span>
              <p>
                {Object.keys(w.layout.panels).length} tabs, saved {relativeTime(w.savedAt)}
              </p>
            </div>
            <div className="settings-control">
              <button
                className="btn"
                aria-label={`Remove workspace ${w.name}`}
                onClick={() => update({ workspaces: prefs.workspaces.filter((x) => x.name !== w.name) })}
              >
                Remove
              </button>
            </div>
          </div>
        ))
      )}
    </Section>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="settings-section" aria-label={title}>
      <h2>{title}</h2>
      <div className="settings-rows">{children}</div>
    </section>
  );
}

function Row({
  label,
  hint,
  htmlFor,
  children,
}: {
  label: string;
  hint?: string;
  htmlFor: string;
  children: ReactNode;
}) {
  return (
    <div className="settings-row">
      <div className="settings-text">
        <label htmlFor={htmlFor} id={`${htmlFor}-label`}>
          {label}
        </label>
        {hint && <p>{hint}</p>}
      </div>
      <div className="settings-control">{children}</div>
    </div>
  );
}

function Toggle({ id, checked, onChange }: { id: string; checked: boolean; onChange(v: boolean): void }) {
  return (
    <button
      id={id}
      type="button"
      role="switch"
      aria-checked={checked}
      className="toggle"
      onClick={() => onChange(!checked)}
    >
      <span className="toggle-knob" />
    </button>
  );
}

function Segmented<T extends string>({
  id,
  value,
  options,
  onChange,
}: {
  id: string;
  value: T;
  options: { value: T; label: string }[];
  onChange(v: T): void;
}) {
  return (
    <div id={id} className="segmented" role="radiogroup" aria-labelledby={`${id}-label`}>
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={value === o.value}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
