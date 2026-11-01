import type { RefObject } from "react";
import type { DockviewApi } from "dockview";
import { Radio } from "lucide-react";
import { broadcast, useBroadcast } from "../broadcast";

/** Terminal tabs (not remote screens), which can take typed input. */
export function terminalTabIds(api: DockviewApi): string[] {
  return api.panels
    .filter((p) => {
      const kind = (p.params as { conn?: { kind?: string } } | undefined)?.conn?.kind;
      return kind !== "rdp" && kind !== "vnc" && !(p.params as { files?: boolean } | undefined)?.files;
    })
    .map((p) => p.id);
}

/** Toolbar toggle: type once, send to every terminal tab that's included. */
export function BroadcastButton({ dock }: { dock: RefObject<DockviewApi | null> }) {
  const { on, members } = useBroadcast();
  const toggle = () => {
    if (on) broadcast.stop();
    else if (dock.current) broadcast.start(terminalTabIds(dock.current));
  };
  return (
    <button
      className={`icon-btn broadcast-btn ${on ? "is-on" : ""}`}
      onClick={toggle}
      aria-pressed={on}
      title={
        on
          ? `Broadcasting your typing to ${members.size} ${members.size === 1 ? "session" : "sessions"}. Click to stop.`
          : "Broadcast input: type once, send to every open terminal tab"
      }
      aria-label="Broadcast input"
    >
      <Radio size={16} />
      {on && <span className="broadcast-count">{members.size}</span>}
    </button>
  );
}
