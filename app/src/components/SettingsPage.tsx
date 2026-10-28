import { useEffect, useState, type ReactNode } from "react";
import { X } from "lucide-react";
import { getBackend } from "../api";
import { colorFor, defaultRules, type HighlightColor, type HighlightRule } from "../highlight";
import type { ThemePref } from "../theme";
import { usePrefs } from "../prefs";
import type { LoggingSettings, SavedPassword } from "../types";
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
    </div>
  );
}

const highlightColors: { value: HighlightColor; label: string }[] = [
  { value: "red", label: "Red" },
  { value: "amber", label: "Amber" },
  { value: "green", label: "Green" },
  { value: "blue", label: "Blue" },
  { value: "purple", label: "Purple" },
];

/** Keywords colored in terminal output (display only: logs and copies are untouched). */
function KeywordHighlighting() {
  const { prefs, update } = usePrefs();
  const rules = prefs.highlightRules;
  const setRules = (highlightRules: HighlightRule[]) => update({ highlightRules });
  const theme = document.documentElement.dataset.theme === "light" ? "light" : "dark";
  return (
    <Section title="Keyword highlighting">
      <Row
        label="Color keywords in output"
        hint="Whole words, any case. Only the display changes; copies and logs keep the original text."
        htmlFor="set-hl"
      >
        <Toggle id="set-hl" checked={prefs.highlightKeywords} onChange={(v) => update({ highlightKeywords: v })} />
      </Row>
      {prefs.highlightKeywords && (
        <div className="settings-row settings-row-wide">
          <div className="settings-text">
            <ul className="hl-rules" aria-label="Keywords">
              {rules.map((r, i) => (
                <li key={i}>
                  <input
                    className="settings-input"
                    value={r.text}
                    aria-label="Keyword"
                    spellCheck={false}
                    style={{ color: colorFor(r.color, theme) }}
                    onChange={(e) => setRules(rules.map((x, j) => (j === i ? { ...x, text: e.target.value } : x)))}
                  />
                  <select
                    aria-label={`Color for ${r.text || "keyword"}`}
                    value={r.color}
                    onChange={(e) =>
                      setRules(rules.map((x, j) => (j === i ? { ...x, color: e.target.value as HighlightColor } : x)))
                    }
                  >
                    {highlightColors.map((c) => (
                      <option key={c.value} value={c.value}>
                        {c.label}
                      </option>
                    ))}
                  </select>
                  <button
                    className="icon-btn"
                    aria-label={`Remove ${r.text || "keyword"}`}
                    onClick={() => setRules(rules.filter((_, j) => j !== i))}
                  >
                    <X size={15} />
                  </button>
                </li>
              ))}
            </ul>
            <div className="hl-actions">
              <button className="btn" onClick={() => setRules([...rules, { text: "", color: "amber" }])}>
                Add keyword
              </button>
              <button className="btn" onClick={() => setRules(defaultRules)}>
                Reset to defaults
              </button>
            </div>
          </div>
        </div>
      )}
    </Section>
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
