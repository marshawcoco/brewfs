import {
  Activity,
  Database,
  FileSearch,
  FolderTree,
  Gauge,
  LogIn,
  HardDrive,
  Settings,
  ShieldCheck,
  Trash2,
  type LucideIcon,
} from 'lucide-react';
import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from 'react';
import { ApiError, fetchHealth, type HealthResponse } from './api';

type PageKey =
  | 'overview'
  | 'filesystems'
  | 'browser'
  | 'trash'
  | 'acl'
  | 'jobs'
  | 'csi'
  | 'settings';

const navItems: Array<{ key: PageKey; label: string; icon: LucideIcon }> = [
  { key: 'overview', label: 'Overview', icon: Gauge },
  { key: 'filesystems', label: 'Filesystems', icon: HardDrive },
  { key: 'browser', label: 'Browser', icon: FileSearch },
  { key: 'trash', label: 'Trash', icon: Trash2 },
  { key: 'acl', label: 'ACL', icon: ShieldCheck },
  { key: 'jobs', label: 'Jobs', icon: Activity },
  { key: 'csi', label: 'CSI', icon: Database },
  { key: 'settings', label: 'Settings', icon: Settings },
];

const pagePaths: Record<PageKey, string> = {
  overview: '/',
  filesystems: '/filesystems',
  browser: '/browser',
  trash: '/trash',
  acl: '/acl',
  jobs: '/jobs',
  csi: '/csi',
  settings: '/settings',
};

const TOKEN_STORAGE_KEY = 'brewfs.console.token';

function pageTitle(page: PageKey): string {
  return navItems.find((item) => item.key === page)?.label ?? 'Overview';
}

function pageFromPathname(pathname: string): PageKey {
  const normalized = pathname === '/' ? '/' : pathname.replace(/\/+$/, '');
  return (
    (Object.entries(pagePaths).find(([, path]) => path === normalized)?.[0] as PageKey | undefined) ??
    'overview'
  );
}

export function App() {
  const [page, setPage] = useState<PageKey>(() => pageFromPathname(window.location.pathname));
  const [token, setToken] = useState<string>(() => sessionStorage.getItem(TOKEN_STORAGE_KEY) ?? '');
  const [tokenInput, setTokenInput] = useState('');
  const [authRequired, setAuthRequired] = useState(false);
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    fetchHealth(token)
      .then((result) => {
        if (!cancelled) {
          setHealth(result);
          setError(null);
          setAuthRequired(false);
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          if (err instanceof ApiError && err.status === 401) {
            setHealth(null);
            setError(null);
            setAuthRequired(true);
          } else {
            setError(err instanceof Error ? err.message : 'health request failed');
          }
        }
      });
    return () => {
      cancelled = true;
    };
  }, [token]);

  useEffect(() => {
    const handlePopState = () => setPage(pageFromPathname(window.location.pathname));
    window.addEventListener('popstate', handlePopState);
    return () => window.removeEventListener('popstate', handlePopState);
  }, []);

  const navigate = (nextPage: PageKey) => {
    setPage(nextPage);
    const nextPath = pagePaths[nextPage];
    if (window.location.pathname !== nextPath) {
      window.history.pushState(null, '', nextPath);
    }
  };

  const submitToken = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const nextToken = tokenInput.trim();
    if (!nextToken) return;
    sessionStorage.setItem(TOKEN_STORAGE_KEY, nextToken);
    setToken(nextToken);
    setTokenInput('');
    setError(null);
  };

  const status = useMemo(() => {
    if (authRequired) return { label: 'Auth required', tone: 'warn' };
    if (error) return { label: 'API unavailable', tone: 'bad' };
    if (!health) return { label: 'Connecting', tone: 'warn' };
    return { label: `BrewFS ${health.version}`, tone: 'good' };
  }, [authRequired, error, health]);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <FolderTree size={24} aria-hidden="true" />
          <div>
            <strong>BrewFS</strong>
            <span>Console</span>
          </div>
        </div>
        <nav aria-label="Primary navigation">
          {navItems.map((item) => {
            const Icon = item.icon;
            return (
              <button
                key={item.key}
                className={page === item.key ? 'nav-item active' : 'nav-item'}
                type="button"
                onClick={() => navigate(item.key)}
              >
                <Icon size={18} aria-hidden="true" />
                <span>{item.label}</span>
              </button>
            );
          })}
        </nav>
      </aside>

      <main className="workspace">
        <header className="topbar">
          <div>
            <p className="eyebrow">Phase 1A scaffold</p>
            <h1>{pageTitle(page)}</h1>
          </div>
          <div className={`status-pill ${status.tone}`}>{status.label}</div>
        </header>

        <section className="content-grid">
          {authRequired ? (
            <AuthPanel value={tokenInput} onChange={setTokenInput} onSubmit={submitToken} />
          ) : (
            renderPage(page, health, error)
          )}
        </section>
      </main>
    </div>
  );
}

function AuthPanel({
  value,
  onChange,
  onSubmit,
}: {
  value: string;
  onChange: (value: string) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
}) {
  return (
    <article className="panel empty-panel">
      <h2>Console token required</h2>
      <form className="auth-form" onSubmit={onSubmit}>
        <label htmlFor="console-token">Bearer token</label>
        <div className="input-row">
          <input
            id="console-token"
            className="auth-input"
            type="password"
            value={value}
            autoComplete="current-password"
            onChange={(event) => onChange(event.target.value)}
          />
          <button className="primary-button" type="submit">
            <LogIn size={16} aria-hidden="true" />
            <span>Unlock</span>
          </button>
        </div>
      </form>
    </article>
  );
}

function renderPage(page: PageKey, health: HealthResponse | null, error: string | null) {
  if (page === 'overview') {
    return (
      <>
        <Panel title="Runtime">
          <Metric label="Service" value={health?.service ?? 'waiting'} />
          <Metric label="Commit" value={health?.commit_short ?? 'unknown'} />
          <Metric label="Auth" value={health?.auth_mode ?? 'unknown'} />
        </Panel>
        <Panel title="Scaffold status">
          <p className="muted">
            The console shell is connected to the health API. Volume registry, jobs, file browsing,
            trash, ACL, and CSI data are intentionally empty in this phase.
          </p>
          {error ? <p className="error-text">{error}</p> : null}
        </Panel>
      </>
    );
  }

  if (page === 'filesystems') {
    return (
      <EmptyPanel title="No registered filesystems" detail="Volume registry arrives in Phase 1B." />
    );
  }

  if (page === 'jobs') {
    return (
      <EmptyPanel
        title="No jobs"
        detail="Runtime job discovery arrives with control-plane integration."
      />
    );
  }

  if (page === 'browser') {
    return (
      <EmptyPanel
        title="File browser unavailable"
        detail="Namespace browsing depends on mounted instance discovery and file metadata APIs."
      />
    );
  }

  if (page === 'trash') {
    return (
      <EmptyPanel
        title="Trash unavailable"
        detail="Trash listing and restore actions depend on delayed-delete metadata APIs."
      />
    );
  }

  if (page === 'acl') {
    return (
      <EmptyPanel
        title="ACL unavailable"
        detail="ACL editing will be enabled only for backends that advertise ACL support."
      />
    );
  }

  if (page === 'csi') {
    return (
      <EmptyPanel
        title="CSI dashboard disabled"
        detail="Kubernetes resource discovery is a subsequent read-only integration."
      />
    );
  }

  return (
    <EmptyPanel
      title="Settings unavailable"
      detail="Token auth and registry settings arrive in Phase 1B."
    />
  );
}

function Panel({ title, children }: { title: string; children: ReactNode }) {
  return (
    <article className="panel">
      <h2>{title}</h2>
      {children}
    </article>
  );
}

function EmptyPanel({ title, detail }: { title: string; detail: string }) {
  return (
    <article className="panel empty-panel">
      <h2>{title}</h2>
      <p className="muted">{detail}</p>
    </article>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}
