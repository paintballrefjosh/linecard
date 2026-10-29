import { useEffect, useState } from "react";
import { getBackend } from "../api";
import type {
  ConnectionDraft,
  ConnectionKind,
  FlowControl,
  ForwardKind,
  Inherited,
  Parity,
  PortForward,
  SerialSettings,
} from "../types";
import { ArrowRight, X } from "lucide-react";
import { Modal } from "./Dialog";
import { highlightStore } from "../highlight";
import { kindLabel } from "./icons";

const defaultPort: Record<ConnectionKind, number> = { ssh: 22, telnet: 23, rdp: 3389, vnc: 5900, serial: 0 };
const kinds: ConnectionKind[] = ["ssh", "telnet", "serial", "rdp", "vnc"];
const bauds = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200];
/** Same hues as the tab-color convention: red for production, etc. */
export const swatches = ["#d64545", "#e07b39", "#d4a72c", "#3fa35b", "#2aa3a3", "#3a7bd5", "#8a5cd6", "#c94f9b"];

/** 9600 8N1, no flow control: the default for most network gear consoles. */
const consoleDefaults = (port = "COM1"): SerialSettings => ({
  port,
  baud: 9600,
  dataBits: 8,
  parity: "none",
  stopBits: 1,
  flow: "none",
});

export function blankDraft(folderId: string | null): ConnectionDraft {
  return {
    id: null,
    folderId,
    name: "",
    kind: "ssh",
    host: "",
    port: 22,
    serial: null,
    username: null,
    tabColor: null,
    jumpHost: null,
    log: null,
    highlight: null,
    forwards: [],
    notes: "",
  };
}

interface Props {
  initial: ConnectionDraft;
  /** Where a new connection goes, e.g. "/DC1 Fort Worth/Spine". */
  location: string;
  onSave(draft: ConnectionDraft): Promise<void>;
  onClose(): void;
  /** Saved SSH connections that can serve as a jump host. */
  jumpHosts: { id: string; name: string; endpoint: string }[];
}

/** Pick a keyword-highlight profile, or inherit one. */
export function HighlightSelect({
  id,
  value,
  inherited,
  onChange,
}: {
  id: string;
  value: string | null;
  inherited: { value: string; from: string } | null;
  onChange(v: string | null): void;
}) {
  const { profiles } = highlightStore.get();
  return (
    <select id={id} value={value ?? ""} onChange={(e) => onChange(e.target.value || null)}>
      <option value="">{inherited ? `Inherited: ${inherited.value} (from ${inherited.from})` : "Inherited"}</option>
      {profiles.map((p) => (
        <option key={p.id} value={p.id}>
          {p.name}
        </option>
      ))}
    </select>
  );
}

/** A forward as typed: "8080" or "0.0.0.0:8080", and "host:port". */
interface ForwardRow {
  kind: ForwardKind;
  listen: string;
  dest: string;
}

const toRow = (f: PortForward): ForwardRow => ({
  kind: f.kind,
  listen: f.bindAddress ? `${f.bindAddress}:${f.bindPort}` : String(f.bindPort || ""),
  dest: f.kind === "dynamic" ? "" : `${f.host}:${f.port}`,
});

/** "host:port" (or "[v6]:port"); a bare port when `portOnly` allows it. */
function splitHostPort(text: string, portOnly: boolean): { host: string; port: number } | null {
  const t = text.trim();
  if (portOnly && /^\d+$/.test(t)) return { host: "", port: Number(t) };
  const m = t.match(/^\[?([^\]]*?)\]?:(\d+)$/);
  return m ? { host: m[1], port: Number(m[2]) } : null;
}

function fromRow(r: ForwardRow, n: number): PortForward {
  const listen = splitHostPort(r.listen, true);
  if (!listen || listen.port > 65535 || (listen.port === 0 && r.kind !== "remote"))
    throw new Error(`Port forward ${n}: enter a port to listen on, like 8080 or 0.0.0.0:8080`);
  if (r.kind === "dynamic") return { kind: r.kind, bindAddress: listen.host, bindPort: listen.port, host: "", port: 0 };
  const dest = splitHostPort(r.dest, false);
  if (!dest || !dest.host || !dest.port || dest.port > 65535)
    throw new Error(`Port forward ${n}: enter where it goes as host:port, like 10.0.0.5:80`);
  return { kind: r.kind, bindAddress: listen.host, bindPort: listen.port, host: dest.host, port: dest.port };
}

const forwardKinds: { value: ForwardKind; label: string; title: string }[] = [
  { value: "local", label: "Local", title: "A port on this computer reaches a host from the server (ssh -L)" },
  { value: "remote", label: "Remote", title: "A port on the server reaches a host from this computer (ssh -R)" },
  {
    value: "dynamic",
    label: "SOCKS",
    title: "A SOCKS proxy on this computer that connects out from the server (ssh -D)",
  },
];

function ForwardsEditor({ rows, onChange }: { rows: ForwardRow[]; onChange(rows: ForwardRow[]): void }) {
  const set = (i: number, patch: Partial<ForwardRow>) =>
    onChange(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <div className="forwards">
      {rows.map((r, i) => (
        <div className="forward-row" key={i}>
          <select
            aria-label="Forward type"
            value={r.kind}
            title={forwardKinds.find((k) => k.value === r.kind)?.title}
            onChange={(e) => set(i, { kind: e.target.value as ForwardKind })}
          >
            {forwardKinds.map((k) => (
              <option key={k.value} value={k.value} title={k.title}>
                {k.label}
              </option>
            ))}
          </select>
          <input
            aria-label={r.kind === "remote" ? "Port on the server" : "Port on this computer"}
            placeholder={r.kind === "remote" ? "Server port" : "Local port"}
            value={r.listen}
            spellCheck={false}
            onChange={(e) => set(i, { listen: e.target.value })}
          />
          {r.kind === "dynamic" ? (
            <span className="forward-socks">SOCKS 4/5 proxy</span>
          ) : (
            <>
              <ArrowRight size={14} className="forward-arrow" aria-hidden />
              <input
                aria-label="Destination host:port"
                placeholder={r.kind === "remote" ? "localhost:3000" : "10.0.0.5:80"}
                value={r.dest}
                spellCheck={false}
                onChange={(e) => set(i, { dest: e.target.value })}
              />
            </>
          )}
          <button
            type="button"
            className="icon-btn"
            aria-label="Remove port forward"
            onClick={() => onChange(rows.filter((_, j) => j !== i))}
          >
            <X size={15} />
          </button>
        </div>
      ))}
      <button
        type="button"
        className="btn btn-small forward-add"
        onClick={() => onChange([...rows, { kind: "local", listen: "", dest: "" }])}
      >
        Add port forward
      </button>
    </div>
  );
}

export function ConnectionEditor({ initial, location, onSave, onClose, jumpHosts }: Props) {
  const [d, setD] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const set = (patch: Partial<ConnectionDraft>) => setD((cur) => ({ ...cur, ...patch }));
  const setSerial = (patch: Partial<SerialSettings>) =>
    setD((cur) => ({ ...cur, serial: { ...(cur.serial ?? consoleDefaults()), ...patch } }));
  const [inherited, setInherited] = useState<Inherited | null>(null);
  const [forwards, setForwards] = useState<ForwardRow[]>(() => initial.forwards.map(toRow));
  useEffect(() => {
    void getBackend().then((b) => b.inheritedSettings(initial.folderId).then(setInherited));
  }, [initial.folderId]);
  const isNew = initial.id === null;
  const serial = d.kind === "serial";
  /** Ports present now (USB adapters come and go), refreshed when Serial is picked. */
  const [ports, setPorts] = useState<{ name: string; description: string | null }[]>([]);
  useEffect(() => {
    if (serial) void getBackend().then((b) => b.listSerialPorts().then(setPorts));
  }, [serial]);

  const changeKind = (kind: ConnectionKind) => {
    // Follow the protocol's default port unless the user picked a custom one.
    const port = d.port === defaultPort[d.kind] || d.port === 0 ? defaultPort[kind] : d.port;
    set({ kind, port, serial: kind === "serial" ? (d.serial ?? consoleDefaults()) : d.serial });
  };

  const save = async () => {
    // An unnamed connection takes its host (or serial port) as its name.
    const name = d.name.trim() || (serial ? (d.serial?.port ?? "") : d.host.trim());
    setSaving(true);
    setError(null);
    try {
      const fwds = d.kind === "ssh" ? forwards.map((r, i) => fromRow(r, i + 1)) : [];
      await onSave({ ...d, name, forwards: fwds });
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  };

  return (
    <Modal
      title={isNew ? "New connection" : `Edit ${initial.name}`}
      className="modal-editor"
      onSubmit={save}
      onClose={onClose}
      actions={
        <>
          {isNew && <span className="modal-foot-note">In {location}</span>}
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={saving}>
            {isNew ? "Create" : "Save"}
          </button>
        </>
      }
    >
      <div className="form-grid">
        <label htmlFor="ce-name">Name</label>
        <input
          id="ce-name"
          value={d.name}
          onChange={(e) => set({ name: e.target.value })}
          placeholder={serial ? d.serial?.port : d.host || "Defaults to the host"}
          data-autofocus
        />

        <label htmlFor="ce-kind">Protocol</label>
        <select id="ce-kind" value={d.kind} onChange={(e) => changeKind(e.target.value as ConnectionKind)}>
          {kinds.map((k) => (
            <option key={k} value={k}>
              {kindLabel[k]}
            </option>
          ))}
        </select>

        {serial ? (
          <>
            <label htmlFor="ce-com">Port</label>
            <div className="form-row">
              <input
                id="ce-com"
                value={d.serial?.port ?? ""}
                onChange={(e) => setSerial({ port: e.target.value })}
                placeholder="COM3 or /dev/ttyUSB0"
                list="ce-com-list"
                required
              />
              <datalist id="ce-com-list">
                {ports.map((p) => (
                  <option key={p.name} value={p.name}>
                    {p.description ?? undefined}
                  </option>
                ))}
              </datalist>
              <select
                aria-label="Baud rate"
                value={d.serial?.baud ?? 9600}
                onChange={(e) => setSerial({ baud: Number(e.target.value) })}
              >
                {bauds.map((b) => (
                  <option key={b} value={b}>
                    {b} baud
                  </option>
                ))}
              </select>
            </div>

            <label htmlFor="ce-bits">Framing</label>
            <div className="form-row">
              <select
                id="ce-bits"
                aria-label="Data bits"
                value={d.serial?.dataBits ?? 8}
                onChange={(e) => setSerial({ dataBits: Number(e.target.value) })}
              >
                {[8, 7, 6, 5].map((b) => (
                  <option key={b} value={b}>
                    {b} data bits
                  </option>
                ))}
              </select>
              <select
                aria-label="Parity"
                value={d.serial?.parity ?? "none"}
                onChange={(e) => setSerial({ parity: e.target.value as Parity })}
              >
                {(["none", "odd", "even", "mark", "space"] as Parity[]).map((p) => (
                  <option key={p} value={p}>
                    {p === "none" ? "No parity" : `${p[0].toUpperCase()}${p.slice(1)} parity`}
                  </option>
                ))}
              </select>
              <select
                aria-label="Stop bits"
                value={d.serial?.stopBits ?? 1}
                onChange={(e) => setSerial({ stopBits: Number(e.target.value) })}
              >
                <option value={1}>1 stop bit</option>
                <option value={2}>2 stop bits</option>
              </select>
            </div>

            <label htmlFor="ce-flow">Flow control</label>
            <select
              id="ce-flow"
              value={d.serial?.flow ?? "none"}
              onChange={(e) => setSerial({ flow: e.target.value as FlowControl })}
            >
              <option value="none">None</option>
              <option value="software">XON/XOFF</option>
              <option value="hardware">RTS/CTS</option>
            </select>
          </>
        ) : (
          <>
            <label htmlFor="ce-host">Host</label>
            <div className="form-row">
              <input
                id="ce-host"
                value={d.host}
                onChange={(e) => set({ host: e.target.value })}
                placeholder="10.0.0.1 or switch.example.net"
                spellCheck={false}
                required
              />
              <input
                className="port-input"
                aria-label="Port"
                type="number"
                min={1}
                max={65535}
                value={d.port || ""}
                onChange={(e) => set({ port: Number(e.target.value) })}
                required
              />
            </div>
          </>
        )}

        {d.kind === "ssh" && (
          <>
            <label htmlFor="ce-jump">Jump host</label>
            <select id="ce-jump" value={d.jumpHost ?? ""} onChange={(e) => set({ jumpHost: e.target.value || null })}>
              <option value="">
                {inherited?.jumpHost
                  ? `Inherited: ${inherited.jumpHost.value} (from ${inherited.jumpHost.from})`
                  : "None: connect directly"}
              </option>
              {jumpHosts
                .filter((j) => j.id !== d.id)
                .map((j) => (
                  <option key={j.id} value={j.id}>
                    {j.name} ({j.endpoint})
                  </option>
                ))}
            </select>
          </>
        )}

        {d.kind === "ssh" && (
          <>
            <span className="form-label">Port forwarding</span>
            <ForwardsEditor rows={forwards} onChange={setForwards} />
          </>
        )}

        <label htmlFor="ce-user">Username</label>
        <input
          id="ce-user"
          value={d.username ?? ""}
          onChange={(e) => set({ username: e.target.value })}
          placeholder={
            inherited?.username
              ? `${inherited.username.value} (from ${inherited.username.from})`
              : "Asked when connecting"
          }
          spellCheck={false}
          autoComplete="off"
        />

        <label htmlFor="ce-log">Session log</label>
        <select
          id="ce-log"
          value={d.log === null ? "" : d.log ? "on" : "off"}
          onChange={(e) => set({ log: e.target.value === "" ? null : e.target.value === "on" })}
        >
          <option value="">
            {inherited?.log
              ? `Inherited: ${inherited.log.value === "on" ? "logging" : "not logging"} (from ${inherited.log.from})`
              : "Inherited"}
          </option>
          <option value="on">Always log this connection</option>
          <option value="off">Never log this connection</option>
        </select>

        <label htmlFor="ce-hl">Keyword highlighting</label>
        <HighlightSelect
          id="ce-hl"
          value={d.highlight}
          inherited={inherited?.highlight ?? null}
          onChange={(highlight) => set({ highlight })}
        />

        <span className="form-label">Tab color</span>
        <div className="swatches" role="radiogroup" aria-label="Tab color">
          <button
            type="button"
            role="radio"
            aria-checked={!d.tabColor}
            className="swatch swatch-none"
            aria-label={inherited?.tabColor ? `Inherited color from ${inherited.tabColor.from}` : "No color"}
            title={
              inherited?.tabColor
                ? `Inherited: ${inherited.tabColor.value} (from ${inherited.tabColor.from})`
                : "No color"
            }
            style={inherited?.tabColor ? { background: inherited.tabColor.value } : undefined}
            onClick={() => set({ tabColor: null })}
          />
          {swatches.map((c) => (
            <button
              key={c}
              type="button"
              role="radio"
              aria-checked={d.tabColor === c}
              aria-label={c}
              className="swatch"
              style={{ background: c }}
              onClick={() => set({ tabColor: c })}
            />
          ))}
        </div>

        <label htmlFor="ce-notes">Notes</label>
        <textarea id="ce-notes" rows={3} value={d.notes} onChange={(e) => set({ notes: e.target.value })} />
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
