import { NavLink, Outlet } from "react-router";
import { useLogout, useOverview } from "../api/hooks";
import { t } from "../messages";

const links = [
  { to: "/", label: t.nav.overview, end: true },
  { to: "/sessions", label: t.nav.sessions, end: false },
  { to: "/devices", label: t.nav.devices, end: false },
  { to: "/config", label: t.nav.config, end: false },
  { to: "/storage", label: t.nav.storage, end: false },
  { to: "/activity", label: t.nav.activity, end: false },
  { to: "/export", label: t.nav.export, end: false },
  { to: "/apps", label: t.nav.apps, end: false },
];

function isHttpUrl(url: string): boolean {
  return /^https?:\/\//i.test(url);
}

export function Layout() {
  const overview = useOverview();
  const logout = useLogout();
  const info = overview.data;

  return (
    <div className="app">
      <nav className="nav" aria-label={t.nav.label}>
        <strong className="brand">{t.appName}</strong>
        <ul>
          {links.map((l) => (
            <li key={l.to}>
              <NavLink to={l.to} end={l.end}>
                {l.label}
              </NavLink>
            </li>
          ))}
        </ul>
        <button type="button" onClick={() => logout.mutate()} disabled={logout.isPending}>
          {logout.isPending ? t.nav.loggingOut : t.nav.logout}
        </button>
      </nav>
      <main>
        <Outlet />
      </main>
      <footer>
        <span>
          {t.footer.version} {info?.version ?? "—"}
        </span>
        {" · "}
        <span>
          {t.footer.commit} {info?.commit ?? "—"}
        </span>
        {" · "}
        {info && isHttpUrl(info.source_url) ? (
          <a href={info.source_url} target="_blank" rel="noopener noreferrer">
            {t.footer.source}
          </a>
        ) : (
          <span>{t.footer.source}</span>
        )}
        {" · "}
        <span>{t.footer.license}</span>
      </footer>
    </div>
  );
}
