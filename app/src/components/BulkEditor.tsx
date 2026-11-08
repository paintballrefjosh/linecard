import { useState } from "react";
import type { ConnectionDraft, FlowControl, Parity, SerialSettings } from "../types";
import { Modal } from "./Dialog";
import { highlightStore } from "../highlight";
import { swatches } from "./ConnectionEditor";

/** The fields several connections can be given at once: the ones that make
 *  sense for all of them. Name, host and protocol stay as they are. */
interface Patch {
  username?: string | null;
  tabColor?: string | null;
  jumpHost?: string | null;
  log?: boolean | null;
  highlight?: string | null;
  port?: number;
  notes?: string;
  serial?: Partial<SerialSettings>;
}

interface Props {
  drafts: ConnectionDraft[];
  /** Saved SSH connections that can serve as a jump host. */
  jumpHosts: { id: string; name: string; endpoint: string }[];
  /** Save these (one call per connection); reject to show a message. */
  onSave(drafts: ConnectionDraft[]): Promise<void>;
  onClose(): void;
}

const MIXED = "__mixed";
const bauds = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200];

/** The value every item agrees on, or "mixed". */
function common<T, V>(items: T[], get: (t: T) => V): { mixed: boolean; value: V } {
  const first = get(items[0]);
  return { mixed: items.some((t) => get(t) !== first), value: first };
}

export function applyPatch(d: ConnectionDraft, p: Patch): ConnectionDraft {
  const out = { ...d };
  if ("username" in p) out.username = p.username || null;
  if ("tabColor" in p) out.tabColor = p.tabColor ?? null;
  if ("jumpHost" in p && d.kind === "ssh") out.jumpHost = p.jumpHost ?? null;
  if ("log" in p) out.log = p.log ?? null;
  if ("highlight" in p) out.highlight = p.highlight ?? null;
  if ("port" in p && p.port) out.port = p.port;
  if ("notes" in p) out.notes = p.notes ?? "";
  if (p.serial && d.serial) out.serial = { ...d.serial, ...p.serial };
  return out;
}

export function BulkEditor({ drafts, jumpHosts, onSave, onClose }: Props) {
  const [patch, setPatch] = useState<Patch>({});
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const set = (p: Patch) => setPatch((cur) => ({ ...cur, ...p }));
  const setSerial = (s: Partial<SerialSettings>) => setPatch((cur) => ({ ...cur, serial: { ...cur.serial, ...s } }));

  const kinds = new Set(drafts.map((d) => d.kind));
  const oneKind = kinds.size === 1 ? drafts[0].kind : null;
  const ssh = drafts.filter((d) => d.kind === "ssh");
  const ids = new Set(drafts.map((d) => d.id));
  const { profiles } = highlightStore.get();

  const user = common(drafts, (d) => d.username ?? "");
  const color = common(drafts, (d) => d.tabColor);
  const jump = ssh.length ? common(ssh, (d) => d.jumpHost) : null;
  const log = common(drafts, (d) => d.log);
  const hl = common(drafts, (d) => d.highlight);
  const port = common(drafts, (d) => d.port);
  const notes = common(drafts, (d) => d.notes);
  const serials = oneKind === "serial" ? drafts.map((d) => d.serial).filter((s): s is SerialSettings => !!s) : [];
  const serial = (key: keyof SerialSettings) => common(serials, (s) => s[key]);

  const touched = (k: keyof Patch) => k in patch;
  const mixedHere = (k: keyof Patch, mixed: boolean) => mixed && !touched(k);
  const changes = Object.keys(patch).length;

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      await onSave(drafts.map((d) => applyPatch(d, patch)));
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  };

  /** A <select> over string values, with a leftover "mixed" entry until touched. */
  const choose = (
    id: string,
    showMixed: boolean,
    value: string,
    options: { value: string; label: string }[],
    onChange: (v: string) => void,
  ) => (
    <select id={id} value={showMixed ? MIXED : value} onChange={(e) => onChange(e.target.value)}>
      {showMixed && (
        <option value={MIXED} disabled>
          Mixed: leave unchanged
        </option>
      )}
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );

  const shown = <T,>(k: keyof Patch, fallback: T): T => (touched(k) ? (patch[k] as T) : fallback);
  const names = drafts.map((d) => d.name);
  const list = names.length > 4 ? `${names.slice(0, 4).join(", ")} and ${names.length - 4} more` : names.join(", ");

  return (
    <Modal
      title={`Edit ${drafts.length} connections`}
      className="modal-editor"
      onSubmit={save}
      onClose={onClose}
      actions={
        <>
          <span className="modal-foot-note">
            {changes === 0 ? "Nothing changed yet" : `${changes} ${changes === 1 ? "setting" : "settings"} will change`}
          </span>
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={saving || changes === 0}>
            Save {drafts.length}
          </button>
        </>
      }
    >
      <p className="bulk-intro">
        Editing <b>{list}</b>. Only the settings you change are applied; a field marked <i>Mixed</i> differs between
        them and stays as it is until you set it. Names and hosts are not shown, since each connection keeps its own.
      </p>
      <div className="form-grid">
        {oneKind && oneKind !== "serial" && (
          <>
            <label htmlFor="be-port">Port</label>
            <input
              id="be-port"
              className="port-input"
              type="number"
              min={1}
              max={65535}
              value={touched("port") ? (patch.port || "") : mixedHere("port", port.mixed) ? "" : port.value || ""}
              placeholder={port.mixed ? "Mixed" : undefined}
              onChange={(e) => set({ port: Number(e.target.value) })}
            />
          </>
        )}

        {serials.length > 0 && (
          <>
            <label htmlFor="be-baud">Baud rate</label>
            {choose(
              "be-baud",
              serial("baud").mixed && patch.serial?.baud === undefined,
              String(patch.serial?.baud ?? serial("baud").value),
              bauds.map((b) => ({ value: String(b), label: `${b} baud` })),
              (v) => setSerial({ baud: Number(v) }),
            )}
            <label htmlFor="be-bits">Data bits</label>
            {choose(
              "be-bits",
              serial("dataBits").mixed && patch.serial?.dataBits === undefined,
              String(patch.serial?.dataBits ?? serial("dataBits").value),
              [8, 7, 6, 5].map((b) => ({ value: String(b), label: `${b} data bits` })),
              (v) => setSerial({ dataBits: Number(v) }),
            )}
            <label htmlFor="be-parity">Parity</label>
            {choose(
              "be-parity",
              serial("parity").mixed && patch.serial?.parity === undefined,
              String(patch.serial?.parity ?? serial("parity").value),
              (["none", "odd", "even", "mark", "space"] as Parity[]).map((p) => ({
                value: p,
                label: p === "none" ? "No parity" : `${p[0].toUpperCase()}${p.slice(1)} parity`,
              })),
              (v) => setSerial({ parity: v as Parity }),
            )}
            <label htmlFor="be-stop">Stop bits</label>
            {choose(
              "be-stop",
              serial("stopBits").mixed && patch.serial?.stopBits === undefined,
              String(patch.serial?.stopBits ?? serial("stopBits").value),
              [
                { value: "1", label: "1 stop bit" },
                { value: "2", label: "2 stop bits" },
              ],
              (v) => setSerial({ stopBits: Number(v) }),
            )}
            <label htmlFor="be-flow">Flow control</label>
            {choose(
              "be-flow",
              serial("flow").mixed && patch.serial?.flow === undefined,
              String(patch.serial?.flow ?? serial("flow").value),
              [
                { value: "none", label: "None" },
                { value: "software", label: "XON/XOFF" },
                { value: "hardware", label: "RTS/CTS" },
              ],
              (v) => setSerial({ flow: v as FlowControl }),
            )}
          </>
        )}

        {jump && (
          <>
            <label htmlFor="be-jump">Jump host</label>
            {choose(
              "be-jump",
              mixedHere("jumpHost", jump.mixed),
              shown<string | null>("jumpHost", jump.value) ?? "",
              [
                { value: "", label: "None: connect directly (or from the folder)" },
                ...jumpHosts
                  .filter((j) => !ids.has(j.id))
                  .map((j) => ({ value: j.id, label: `${j.name} (${j.endpoint})` })),
              ],
              (v) => set({ jumpHost: v || null }),
            )}
            {ssh.length < drafts.length && (
              <span className="bulk-note">Applies to the {ssh.length} SSH connections only.</span>
            )}
          </>
        )}

        <label htmlFor="be-user">Username</label>
        <input
          id="be-user"
          value={touched("username") ? (patch.username ?? "") : mixedHere("username", user.mixed) ? "" : user.value}
          onChange={(e) => set({ username: e.target.value })}
          placeholder={user.mixed && !touched("username") ? "Mixed: leave unchanged" : "Asked when connecting"}
          spellCheck={false}
          autoComplete="off"
          data-autofocus
        />

        <label htmlFor="be-log">Session log</label>
        {choose(
          "be-log",
          mixedHere("log", log.mixed),
          (() => {
            const v = shown<boolean | null>("log", log.value);
            return v === null ? "" : v ? "on" : "off";
          })(),
          [
            { value: "", label: "Inherited" },
            { value: "on", label: "Always log these connections" },
            { value: "off", label: "Never log these connections" },
          ],
          (v) => set({ log: v === "" ? null : v === "on" }),
        )}

        <label htmlFor="be-hl">Keyword highlighting</label>
        {choose(
          "be-hl",
          mixedHere("highlight", hl.mixed),
          shown<string | null>("highlight", hl.value) ?? "",
          [{ value: "", label: "Inherited" }, ...profiles.map((p) => ({ value: p.id, label: p.name }))],
          (v) => set({ highlight: v || null }),
        )}

        <span className="form-label">Tab color{mixedHere("tabColor", color.mixed) ? " (mixed)" : ""}</span>
        <div className="swatches" role="radiogroup" aria-label="Tab color">
          {(() => {
            const current = shown<string | null>("tabColor", color.value);
            const unknown = mixedHere("tabColor", color.mixed);
            return (
              <>
                <button
                  type="button"
                  role="radio"
                  aria-checked={!unknown && !current}
                  className="swatch swatch-none"
                  aria-label="No color"
                  title="No color"
                  onClick={() => set({ tabColor: null })}
                />
                {swatches.map((c) => (
                  <button
                    key={c}
                    type="button"
                    role="radio"
                    aria-checked={!unknown && current === c}
                    aria-label={c}
                    className="swatch"
                    style={{ background: c }}
                    onClick={() => set({ tabColor: c })}
                  />
                ))}
              </>
            );
          })()}
        </div>

        <label htmlFor="be-notes">Notes</label>
        <textarea
          id="be-notes"
          rows={3}
          value={touched("notes") ? (patch.notes ?? "") : mixedHere("notes", notes.mixed) ? "" : notes.value}
          placeholder={notes.mixed && !touched("notes") ? "Mixed: typing here replaces every note" : undefined}
          onChange={(e) => set({ notes: e.target.value })}
        />
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
