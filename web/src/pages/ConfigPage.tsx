import { useConfig } from "../api/hooks";
import type { ConfigInfo, ConfigRequest } from "../api/types";
import { CopyBox } from "../components/CopyBox";
import { QueryView } from "../components/QueryView";
import { Time } from "../components/Time";
import { formatBytes, prettyJson } from "../format";
import { t } from "../messages";
import { useTitle } from "../useTitle";

function RequestRow({ r }: { r: ConfigRequest }) {
  return (
    <tr>
      <td>{t.config.kinds[r.kind] ?? r.kind}</td>
      <td>
        <code>{r.id}</code>
      </td>
      <td>{r.action ? (t.config.actions[r.action] ?? r.action) : "—"}</td>
      <td>
        {r.changes.length === 0 ? (
          "—"
        ) : (
          <ul>
            {r.changes.map((c) => (
              <li key={c.key}>
                <code>{c.key}</code> = <code>{c.value_json}</code>
              </li>
            ))}
          </ul>
        )}
      </td>
      <td>
        <ul>
          {r.devices.map((d) => (
            <li key={d.id}>
              {d.name ?? d.id}：{t.devices.states[d.state] ?? d.state}
            </li>
          ))}
        </ul>
      </td>
    </tr>
  );
}

function CurrentConfig({ current }: { current: NonNullable<ConfigInfo["current"]> }) {
  const hasExecutable = current.resources.some((r) => r.executable);
  return (
    <>
      <h2>{t.config.current}</h2>
      <dl className="fields">
        <dt>{t.config.serial}</dt>
        <dd>{current.serial}</dd>
        <dt>{t.config.etag}</dt>
        <dd>
          <code>{current.etag}</code>
        </dd>
        <dt>{t.config.issued}</dt>
        <dd>
          <Time ms={current.issued_at_ms} />
        </dd>
        <dt>{t.config.keyId}</dt>
        <dd>
          <code>{current.key_id}</code>
        </dd>
      </dl>

      <h3>{t.config.payload}</h3>
      <pre className="text">{prettyJson(current.payload)}</pre>

      <h3>{t.config.resources}</h3>
      {hasExecutable && <p className="warning">{t.config.executableWarning}</p>}
      {current.resources.length === 0 ? (
        <p>{t.config.noResources}</p>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th scope="col">{t.config.colName}</th>
                <th scope="col">{t.config.colVersion}</th>
                <th scope="col">{t.config.colSha}</th>
                <th scope="col">{t.config.colSize}</th>
                <th scope="col">{t.config.colExecutable}</th>
              </tr>
            </thead>
            <tbody>
              {current.resources.map((r) => (
                <tr key={`${r.name}@${r.version}`}>
                  <td>
                    <code>{r.name}</code>
                  </td>
                  <td>{r.version}</td>
                  <td>
                    <code>{r.sha256}</code>
                  </td>
                  <td className="num">{formatBytes(r.size)}</td>
                  <td>{r.executable ? <strong>{t.config.executableMarker}</strong> : t.common.no}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}

export function ConfigPage() {
  useTitle(t.config.title);
  const config = useConfig();

  return (
    <>
      <h1>{t.config.title}</h1>
      <QueryView query={config}>
        {(c) => (
          <>
            {c.source.last_error !== null && (
              <section role="alert" aria-labelledby="config-error-title">
                <h2 id="config-error-title" className="error">
                  ⚠ {t.config.lastError}
                </h2>
                <pre className="text error">{c.source.last_error}</pre>
                <p>{t.config.lastErrorHint}</p>
              </section>
            )}

            {c.current ? (
              <CurrentConfig current={c.current} />
            ) : (
              <>
                <h2>{t.config.emptyTitle}</h2>
                <p>{t.config.emptyBody}</p>
              </>
            )}

            <h2>{t.config.publicView}</h2>
            <p>{c.public.enabled ? t.config.publicOn(c.public.serial) : t.config.publicOff(c.public.serial)}</p>

            <h2>{t.config.signing}</h2>
            <dl className="fields">
              <dt>{t.config.keyId}</dt>
              <dd>
                <code>{c.signing.key_id}</code>
              </dd>
            </dl>
            <CopyBox label={t.config.publicKey} value={c.signing.public_key} rows={2} />
            <CopyBox label={t.config.connectCommand} value={c.signing.connect_command} rows={3} />
            <p className="muted">{t.config.connectHint}</p>

            <h2>{t.config.requests}</h2>
            {c.requests.length === 0 ? (
              <p>{t.config.noRequests}</p>
            ) : (
              <div className="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">{t.config.colKind}</th>
                      <th scope="col">{t.config.colId}</th>
                      <th scope="col">{t.config.colAction}</th>
                      <th scope="col">{t.config.colChanges}</th>
                      <th scope="col">{t.config.colDevices}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {c.requests.map((r) => (
                      <RequestRow key={`${r.kind}:${r.id}`} r={r} />
                    ))}
                  </tbody>
                </table>
              </div>
            )}

            <h2>{t.config.history}</h2>
            {c.history.length === 0 ? (
              <p>{t.config.noHistory}</p>
            ) : (
              <div className="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">{t.config.serial}</th>
                      <th scope="col">{t.config.colView}</th>
                      <th scope="col">{t.config.issued}</th>
                      <th scope="col">{t.config.etag}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {c.history.map((h) => (
                      <tr key={`${h.serial}:${h.view}`}>
                        <td className="num">{h.serial}</td>
                        <td>{t.config.views[h.view] ?? h.view}</td>
                        <td>
                          <Time ms={h.issued_at_ms} />
                        </td>
                        <td>
                          <code>{h.etag}</code>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}

            <h2>{t.config.source}</h2>
            <dl className="fields">
              <dt>{t.config.dir}</dt>
              <dd>
                <code>{c.source.dir}</code>
              </dd>
              <dt>{t.config.checked}</dt>
              <dd>
                <Time ms={c.source.checked_ms} />
              </dd>
              {c.source.last_error === null && (
                <>
                  <dt>{t.config.fileStatus}</dt>
                  <dd>{t.config.sourceOk}</dd>
                </>
              )}
              <dt>{t.config.deviceToken}</dt>
              <dd className={c.device_token_set ? undefined : "warning"}>
                {c.device_token_set ? t.config.tokenSet : t.config.tokenNotSet}
              </dd>
            </dl>
          </>
        )}
      </QueryView>
    </>
  );
}
