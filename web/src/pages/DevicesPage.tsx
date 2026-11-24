import { useDevices } from "../api/hooks";
import type { Device } from "../api/types";
import { QueryView } from "../components/QueryView";
import { Time } from "../components/Time";
import { formatBytes, formatNumber } from "../format";
import { t } from "../messages";
import { useTitle } from "../useTitle";

function DeviceName({ d }: { d: Device }) {
  return (
    <>
      {d.name ?? t.common.unnamed} <code>{d.id}</code>
      {d.state && !d.state.verified && (
        <>
          {" "}
          <strong>{t.devices.unverified}</strong>
        </>
      )}
    </>
  );
}

function BehindText({ behind }: { behind: number | null }) {
  if (behind === null) return <span className="muted">{t.devices.noConfig}</span>;
  if (behind > 0) return <strong>{t.devices.behind(behind)}</strong>;
  return <span className="muted">{t.devices.upToDate}</span>;
}

function DeviceDetails({ d }: { d: Device }) {
  const s = d.state;
  if (!s) return null;
  return (
    <section aria-labelledby={`device-${d.id}`}>
      <h3 id={`device-${d.id}`}>
        <DeviceName d={d} />
      </h3>
      {d.name === null && <p className="muted">{t.devices.noReports}</p>}
      <dl className="fields">
        <dt>{t.devices.firstSeen}</dt>
        <dd>
          <Time ms={d.first_seen_ms} />
        </dd>
        <dt>{t.devices.etag}</dt>
        <dd>
          <code>{s.applied_etag}</code>
        </dd>
        <dt>{t.devices.localValues}</dt>
        <dd>
          {s.local_values.length === 0 ? (
            t.devices.noLocalValues
          ) : (
            <ul>
              {s.local_values.map((v) => (
                <li key={v.key}>
                  <code>{v.key}</code> = <code>{v.value_json}</code>
                  {v.locked && (
                    <>
                      {" "}
                      <strong title={t.devices.lockedTitle}>{t.devices.locked}</strong>
                    </>
                  )}
                </li>
              ))}
            </ul>
          )}
        </dd>
        <dt>{t.devices.requests}</dt>
        <dd>
          {s.requests.length === 0 ? (
            t.devices.noRequests
          ) : (
            <ul>
              {s.requests.map((r) => (
                <li key={r.id}>
                  <code>{r.id}</code>：{t.devices.states[r.state] ?? r.state}
                </li>
              ))}
            </ul>
          )}
        </dd>
        <dt>{t.devices.lastFailure}</dt>
        <dd>
          {s.last_failure ? (
            <span className="warning">
              {t.devices.failure(
                s.last_failure.serial,
                t.devices.failureReasons[s.last_failure.reason] ?? s.last_failure.reason,
              )}
            </span>
          ) : (
            t.devices.noFailure
          )}
        </dd>
      </dl>
    </section>
  );
}

export function DevicesPage() {
  useTitle(t.devices.title);
  const devices = useDevices();

  return (
    <>
      <h1>{t.devices.title}</h1>
      <p>{t.devices.unverifiedNote}</p>
      <QueryView query={devices}>
        {({ items }) =>
          items.length === 0 ? (
            <p>{t.devices.empty}</p>
          ) : (
            <>
              <div className="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">{t.devices.colDevice}</th>
                      <th scope="col">{t.devices.colSessions}</th>
                      <th scope="col">{t.devices.colBytes}</th>
                      <th scope="col">{t.devices.colLastUpload}</th>
                      <th scope="col">{t.devices.colLastSeen}</th>
                      <th scope="col">{t.devices.colVersion}</th>
                      <th scope="col">{t.devices.colSerial}</th>
                      <th scope="col">{t.devices.colExec}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {items.map((d) => (
                      <tr key={d.id}>
                        <td>
                          <DeviceName d={d} />
                        </td>
                        <td className="num">{formatNumber(d.sessions)}</td>
                        <td className="num">{formatBytes(d.bytes)}</td>
                        <td>
                          <Time ms={d.last_upload_ms} />
                        </td>
                        {d.state ? (
                          <>
                            <td>
                              <Time ms={d.state.received_ms} />
                            </td>
                            <td>{d.state.client_version}</td>
                            <td>
                              {d.state.applied_serial} <BehindText behind={d.state.behind} />
                            </td>
                            <td>{d.state.exec_allowed ? t.devices.execAllowed : t.devices.execDenied}</td>
                          </>
                        ) : (
                          <td colSpan={4} className="muted">
                            {t.devices.noState}
                          </td>
                        )}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              <h2>{t.devices.details}</h2>
              {items.map((d) => (
                <DeviceDetails key={d.id} d={d} />
              ))}
            </>
          )
        }
      </QueryView>
    </>
  );
}
