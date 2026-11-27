import { useEffect, useState } from "react";
import { useForcePathMap, usePathMaps, useSavePathMaps, useWithdrawForce } from "../api/hooks";
import type { LocalPathMap, PathMap, PathMapDevice, PathMaps } from "../api/types";
import { describeError, QueryView } from "../components/QueryView";
import { Time } from "../components/Time";
import { t } from "../messages";
import { useTitle } from "../useTitle";

interface Row extends PathMap {
  /** stable React key */
  key: number;
}

let nextKey = 1;

function toRows(items: PathMap[]): Row[] {
  return items.map((m) => ({ ...m, devices: [...m.devices], key: nextKey++ }));
}

function toItems(rows: Row[]): PathMap[] {
  return rows.map(({ from, to, devices }) => ({ from, to, devices }));
}

function sameItems(a: PathMap[], b: PathMap[]): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export function PathsPage() {
  useTitle(t.paths.title);
  const paths = usePathMaps();
  return (
    <>
      <h1>{t.paths.title}</h1>
      <p>{t.paths.intro}</p>
      <QueryView query={paths}>{(data) => <Editor data={data} />}</QueryView>
    </>
  );
}

function Editor({ data }: { data: PathMaps }) {
  const save = useSavePathMaps();
  const force = useForcePathMap();
  const withdraw = useWithdrawForce();
  const [rows, setRows] = useState<Row[]>(() => toRows(data.items));
  const [base, setBase] = useState<PathMap[]>(data.items);

  // A newer copy from the server replaces the table only when nothing is being edited.
  useEffect(() => {
    if (!sameItems(data.items, base)) {
      setRows((current) => (sameItems(toItems(current), base) ? toRows(data.items) : current));
      setBase(data.items);
    }
  }, [data.items, base]);

  const dirty = !sameItems(toItems(rows), data.items);
  const update = (key: number, change: Partial<PathMap>) =>
    setRows((rs) => rs.map((r) => (r.key === key ? { ...r, ...change } : r)));

  function addFor(device: PathMapDevice, local: LocalPathMap, to: string) {
    const listed = rows.some((r) => r.from === local.from && r.devices.includes(device.id));
    if (listed) {
      window.alert(t.paths.alreadyListed);
      return;
    }
    setRows((rs) => [...rs, { key: nextKey++, from: local.from, to, devices: [device.id] }]);
  }

  function askForce(device: PathMapDevice, local: LocalPathMap) {
    const to = window.prompt(t.paths.forcePrompt(local.from), local.to);
    if (to === null) return;
    force.mutate({ device: device.id, from: local.from, to: to.trim() });
  }

  const known = data.devices;
  const label = (id: string) => {
    const d = known.find((x) => x.id === id);
    return t.paths.deviceName(d?.name ?? null, id);
  };

  return (
    <>
      <h2>{t.paths.serverTitle}</h2>
      {data.publish_error && (
        <p role="alert" className="error">
          {t.paths.publishError}
          {data.publish_error}
        </p>
      )}
      <p>
        {data.in_force ? t.paths.inForce(data.published_serial) : t.paths.notInForce}
        {data.updated_ms !== null && (
          <>
            {" "}
            {t.paths.updated} <Time ms={data.updated_ms} />
          </>
        )}
      </p>
      {rows.length === 0 ? (
        <p>{t.paths.serverEmpty}</p>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th scope="col">{t.paths.colFrom}</th>
                <th scope="col">{t.paths.colTo}</th>
                <th scope="col">{t.paths.colDevices}</th>
                <th scope="col">{t.paths.colAction}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => {
                const ids = [...known.map((d) => d.id), ...r.devices.filter((id) => !known.some((d) => d.id === id))];
                return (
                  <tr key={r.key}>
                    <td>
                      <input
                        type="text"
                        aria-label={t.paths.colFrom}
                        value={r.from}
                        onChange={(e) => update(r.key, { from: e.target.value })}
                      />
                    </td>
                    <td>
                      <input
                        type="text"
                        aria-label={t.paths.colTo}
                        placeholder={t.paths.toHint}
                        value={r.to}
                        onChange={(e) => update(r.key, { to: e.target.value })}
                      />
                      {r.to === "" && <small className="muted"> {t.paths.dropped}</small>}
                    </td>
                    <td>
                      <label>
                        <input
                          type="checkbox"
                          checked={r.devices.length === 0}
                          onChange={(e) => update(r.key, { devices: e.target.checked ? [] : ids.slice(0, 1) })}
                        />{" "}
                        {t.paths.allDevices}
                      </label>
                      {r.devices.length > 0 &&
                        ids.map((id) => (
                          <div key={id}>
                            <label>
                            <input
                              type="checkbox"
                              checked={r.devices.includes(id)}
                              onChange={(e) =>
                                update(r.key, {
                                  devices: e.target.checked
                                    ? [...r.devices, id]
                                    : r.devices.filter((d) => d !== id),
                                })
                              }
                            />{" "}
                            {label(id)}
                            </label>
                          </div>
                        ))}
                    </td>
                    <td>
                      <button type="button" onClick={() => setRows((rs) => rs.filter((x) => x.key !== r.key))}>
                        {t.paths.remove}
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      <p>
        <button
          type="button"
          onClick={() => setRows((rs) => [...rs, { key: nextKey++, from: "", to: "", devices: [] }])}
        >
          {t.paths.add}
        </button>{" "}
        <button type="button" disabled={!dirty || save.isPending} onClick={() => save.mutate(toItems(rows))}>
          {save.isPending ? t.paths.saving : t.paths.save}
        </button>{" "}
        <button type="button" disabled={!dirty || save.isPending} onClick={() => setRows(toRows(data.items))}>
          {t.paths.reset}
        </button>
        {dirty && <span className="muted"> {t.paths.unsaved}</span>}
      </p>
      {save.isError && (
        <p role="alert" className="error">
          {t.paths.saveFailed}
          {describeError(save.error)}
        </p>
      )}
      {save.isSuccess && !dirty && (
        <p role="status">
          {save.data.published ? t.paths.saved(save.data.serial) : `${t.paths.notPublished}${save.data.error ?? ""}`}
        </p>
      )}

      <h2>{t.paths.devicesTitle}</h2>
      <p>{t.paths.devicesIntro}</p>
      {known.map((d) => (
        <section key={d.id}>
          <h3>{t.paths.deviceName(d.name, d.id)}</h3>
          {d.local === null ? (
            <p className="muted">{t.paths.notReported}</p>
          ) : d.local.length === 0 ? (
            <p className="muted">
              {t.paths.noLocal} {t.paths.reportedAt} {d.reported_ms !== null && <Time ms={d.reported_ms} />}
            </p>
          ) : (
            <>
              <p className="muted">
                {t.paths.reportedAt} {d.reported_ms !== null && <Time ms={d.reported_ms} />}
              </p>
              <div className="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">{t.paths.colFrom}</th>
                      <th scope="col">{t.paths.colTo}</th>
                      <th scope="col">{t.paths.colAction}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {d.local.map((m) => (
                      <tr key={m.from}>
                        <td>
                          <code>{m.from}</code>
                        </td>
                        <td>
                          <code>{m.to}</code>
                          {m.locked && <small className="muted"> {t.paths.locked}</small>}
                        </td>
                        <td>
                          <button type="button" onClick={() => addFor(d, m, m.to)}>
                            {t.paths.editHere}
                          </button>{" "}
                          <button type="button" onClick={() => addFor(d, m, "")}>
                            {t.paths.dropHere}
                          </button>
                          {m.locked && (
                            <>
                              {" "}
                              <button type="button" disabled={force.isPending} onClick={() => askForce(d, m)}>
                                {t.paths.forceHere}
                              </button>
                            </>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
          )}
        </section>
      ))}

      <h2>{t.paths.forceTitle}</h2>
      <p>{t.paths.forceIntro}</p>
      {force.isError && (
        <p role="alert" className="error">
          {t.paths.forceFailed}
          {describeError(force.error)}
        </p>
      )}
      {withdraw.isError && (
        <p role="alert" className="error">
          {describeError(withdraw.error)}
        </p>
      )}
      {data.force.length === 0 ? (
        <p>{t.paths.forceEmpty}</p>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th scope="col">{t.paths.colRequest}</th>
                <th scope="col">{t.paths.colDevice}</th>
                <th scope="col">{t.paths.colFrom}</th>
                <th scope="col">{t.paths.colTo}</th>
                <th scope="col">{t.paths.colState}</th>
                <th scope="col">{t.paths.colAction}</th>
              </tr>
            </thead>
            <tbody>
              {data.force.map((f) => (
                <tr key={f.id}>
                  <td>
                    <code>{f.id}</code>
                    <br />
                    <small className="muted">
                      <Time ms={f.created_ms} />
                    </small>
                  </td>
                  <td>{label(f.device)}</td>
                  <td>
                    <code>{f.from}</code>
                  </td>
                  <td>{f.to === "" ? t.paths.dropped : <code>{f.to}</code>}</td>
                  <td>{t.paths.forceState[f.state] ?? f.state}</td>
                  <td>
                    <button
                      type="button"
                      disabled={withdraw.isPending && withdraw.variables === f.id}
                      onClick={() => {
                        if (window.confirm(t.paths.confirmWithdraw(f.id))) withdraw.mutate(f.id);
                      }}
                    >
                      {withdraw.isPending && withdraw.variables === f.id ? t.paths.withdrawing : t.paths.withdraw}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}
