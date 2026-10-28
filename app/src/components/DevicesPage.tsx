import { AlertTriangle, Laptop } from "lucide-react";
import type { Device } from "../types";
import { relativeTime } from "./ConnectionTree";

const STALE_MS = 30 * 24 * 60 * 60 * 1000;

/** Compare dotted versions numerically ("0.10.0" > "0.9.3"). */
export function compareVersions(a: string, b: string): number {
  const pa = a.split(/[.-]/).map((x) => parseInt(x, 10) || 0);
  const pb = b.split(/[.-]/).map((x) => parseInt(x, 10) || 0);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d) return d;
  }
  return 0;
}

export function versionMismatch(devices: Device[]): boolean {
  const live = devices.filter((d) => Date.now() - d.lastSeen < STALE_MS);
  return new Set(live.map((d) => d.appVersion)).size > 1;
}

export function DevicesPage({ devices }: { devices: Device[] }) {
  const newest = devices.reduce((v, d) => (compareVersions(d.appVersion, v) > 0 ? d.appVersion : v), "0");
  const me = devices.find((d) => d.isThisDevice);
  const behind = me && compareVersions(me.appVersion, newest) < 0;
  const mismatch = versionMismatch(devices);

  return (
    <div className="devices-page">
      <h1>Devices</h1>
      <p className="lede">Every device in this sync group, with the app version it last reported.</p>

      {mismatch && (
        <div className="notice notice-warn" role="status">
          <AlertTriangle size={16} />
          <span>
            Devices are running different versions. Newest is {newest}.{behind ? " This device is behind." : ""}
          </span>
        </div>
      )}

      <table className="devices-table">
        <thead>
          <tr>
            <th>Device</th>
            <th>OS</th>
            <th>Version</th>
            <th>Sync format</th>
            <th>Transport</th>
            <th>Last seen</th>
            <th>Pending</th>
          </tr>
        </thead>
        <tbody>
          {devices.map((d) => {
            const stale = Date.now() - d.lastSeen > STALE_MS;
            const old = compareVersions(d.appVersion, newest) < 0;
            return (
              <tr key={d.id} className={stale ? "is-stale" : old ? "is-behind" : ""}>
                <td>
                  <Laptop size={14} /> {d.name}
                  {d.isThisDevice && <span className="tag">This device</span>}
                </td>
                <td>{d.os}</td>
                <td>
                  {d.appVersion}
                  {old && !stale && <span className="tag tag-warn">Update available</span>}
                </td>
                <td>v{d.protocolVersion}</td>
                <td>{d.transport}</td>
                <td>{stale ? `${relativeTime(d.lastSeen)} — consider removing` : relativeTime(d.lastSeen)}</td>
                <td>{d.pendingOps || "—"}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
