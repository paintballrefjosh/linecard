import type { ReactNode } from "react";
import type { ThemePref } from "../theme";
import { usePrefs } from "../prefs";

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
    </div>
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
