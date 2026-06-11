import {
  Activity,
  Database,
  FileSearch,
  FolderTree,
  Gauge,
  LogIn,
  HardDrive,
  Pencil,
  RefreshCw,
  Settings,
  ShieldCheck,
  Trash2,
  X,
  type LucideIcon,
} from 'lucide-react';
import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from 'react';
import {
  ApiError,
  createVolume,
  deleteVolume as deleteVolumeRequest,
  fetchJobStatus,
  fetchHealth,
  fetchInstances,
  fetchVolumes,
  runGcJob,
  type HealthResponse,
  type InstanceInfoResponse,
  type InstanceResponse,
  type JobStatusResponse,
  type VolumeResponse,
  updateVolume,
} from './api';
import { loadFeatureStatus, type FeatureKey, type FeatureStatus } from './featureStatus';
import { loadInstanceDetails } from './instanceDetails';
import { labelsFromText, labelsToText } from './volumeEdit';

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

type VolumeFormState = {
  name: string;
  mount_point: string;
  data_backend: string;
  data_dir: string;
  meta_backend: string;
  meta_url: string;
};

type VolumeEditFormState = {
  name: string;
  description: string;
  labels: string;
};

const emptyVolumeForm: VolumeFormState = {
  name: '',
  mount_point: '',
  data_backend: 'local-fs',
  data_dir: '',
  meta_backend: 'sqlx',
  meta_url: '',
};

type CurrentJob = {
  pid: number;
  status: JobStatusResponse;
};

type FeatureResult = {
  feature: FeatureKey;
  status: FeatureStatus;
};

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

function featureForPage(page: PageKey): FeatureKey | null {
  if (page === 'browser' || page === 'trash' || page === 'acl' || page === 'csi') {
    return page;
  }
  return null;
}

export function App() {
  const [page, setPage] = useState<PageKey>(() => pageFromPathname(window.location.pathname));
  const [token, setToken] = useState<string>(() => sessionStorage.getItem(TOKEN_STORAGE_KEY) ?? '');
  const [tokenInput, setTokenInput] = useState('');
  const [authRequired, setAuthRequired] = useState(false);
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [instances, setInstances] = useState<InstanceResponse[]>([]);
  const [instanceDetails, setInstanceDetails] = useState<Record<number, InstanceInfoResponse>>({});
  const [instanceError, setInstanceError] = useState<string | null>(null);
  const [volumes, setVolumes] = useState<VolumeResponse[]>([]);
  const [volumeError, setVolumeError] = useState<string | null>(null);
  const [volumeForm, setVolumeForm] = useState<VolumeFormState>(emptyVolumeForm);
  const [creatingVolume, setCreatingVolume] = useState(false);
  const [createVolumeError, setCreateVolumeError] = useState<string | null>(null);
  const [editingVolumeId, setEditingVolumeId] = useState<string | null>(null);
  const [editVolumeForm, setEditVolumeForm] = useState<VolumeEditFormState | null>(null);
  const [volumeActionInFlight, setVolumeActionInFlight] = useState(false);
  const [volumeActionError, setVolumeActionError] = useState<string | null>(null);
  const [selectedJobPid, setSelectedJobPid] = useState<number | null>(null);
  const [gcDryRun, setGcDryRun] = useState(true);
  const [jobRequestInFlight, setJobRequestInFlight] = useState(false);
  const [jobError, setJobError] = useState<string | null>(null);
  const [currentJob, setCurrentJob] = useState<CurrentJob | null>(null);
  const [featureResult, setFeatureResult] = useState<FeatureResult | null>(null);
  const [featureLoading, setFeatureLoading] = useState(false);
  const [featureError, setFeatureError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function loadConsoleData() {
      try {
        const result = await fetchHealth(token);
        if (cancelled) return;
        setHealth(result);
        setError(null);
        setAuthRequired(false);

        try {
          const instanceResult = await fetchInstances(token);
          if (!cancelled) {
            setInstances(instanceResult.instances);
            setInstanceDetails({});
          }
          const detailState = await loadInstanceDetails(instanceResult.instances, token);
          if (!cancelled) {
            setInstanceDetails(detailState.details);
            if (detailState.authRequired) {
              setHealth(null);
              setAuthRequired(true);
            } else {
              setInstanceError(detailState.error);
            }
          }
        } catch (err: unknown) {
          if (cancelled) return;
          if (err instanceof ApiError && err.status === 401) {
            setHealth(null);
            setAuthRequired(true);
          } else {
            setInstanceError(err instanceof Error ? err.message : 'instances request failed');
          }
        }

        try {
          const volumeResult = await fetchVolumes(token);
          if (!cancelled) {
            setVolumes(volumeResult.volumes);
            setVolumeError(null);
          }
        } catch (err: unknown) {
          if (cancelled) return;
          if (err instanceof ApiError && err.status === 401) {
            setHealth(null);
            setAuthRequired(true);
          } else {
            setVolumeError(err instanceof Error ? err.message : 'volume request failed');
          }
        }
      } catch (err: unknown) {
        if (cancelled) return;
        if (err instanceof ApiError && err.status === 401) {
          setHealth(null);
          setError(null);
          setAuthRequired(true);
        } else {
          setError(err instanceof Error ? err.message : 'health request failed');
        }
      }
    }

    void loadConsoleData();
    return () => {
      cancelled = true;
    };
  }, [token]);

  useEffect(() => {
    if (instances.length === 0) {
      setSelectedJobPid(null);
      return;
    }

    setSelectedJobPid((current) =>
      current !== null && instances.some((instance) => instance.pid === current)
        ? current
        : instances[0].pid,
    );
  }, [instances]);

  useEffect(() => {
    const handlePopState = () => setPage(pageFromPathname(window.location.pathname));
    window.addEventListener('popstate', handlePopState);
    return () => window.removeEventListener('popstate', handlePopState);
  }, []);

  useEffect(() => {
    const feature = featureForPage(page);
    if (!feature) return;
    let cancelled = false;

    setFeatureLoading(true);
    setFeatureError(null);
    void loadFeatureStatus(feature, volumes, token)
      .then((status) => {
        if (!cancelled) {
          setFeatureResult({ feature, status });
        }
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        if (err instanceof ApiError && err.status === 401) {
          setAuthRequired(true);
        } else {
          setFeatureError(err instanceof Error ? err.message : 'feature request failed');
        }
      })
      .finally(() => {
        if (!cancelled) {
          setFeatureLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [page, token, volumes]);

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

  const updateVolumeForm = (field: keyof VolumeFormState, value: string) => {
    setVolumeForm((current) => ({ ...current, [field]: value }));
  };

  const submitVolume = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setCreatingVolume(true);
    setCreateVolumeError(null);
    try {
      const created = await createVolume(
        {
          name: volumeForm.name.trim(),
          mount_config: {
            mount_point: optionalField(volumeForm.mount_point),
            data_backend: volumeForm.data_backend.trim(),
            data_dir: optionalField(volumeForm.data_dir),
            meta_backend: volumeForm.meta_backend.trim(),
            meta_url: optionalField(volumeForm.meta_url),
          },
        },
        token,
      );
      setVolumes((current) => [...current, created]);
      setVolumeForm(emptyVolumeForm);
      setVolumeError(null);
    } catch (err: unknown) {
      if (err instanceof ApiError && err.status === 401) {
        setAuthRequired(true);
      } else {
        setCreateVolumeError(err instanceof Error ? err.message : 'create volume request failed');
      }
    } finally {
      setCreatingVolume(false);
    }
  };

  const startVolumeEdit = (volume: VolumeResponse) => {
    setEditingVolumeId(volume.id);
    setEditVolumeForm({
      name: volume.name,
      description: volume.description ?? '',
      labels: labelsToText(volume.labels),
    });
    setVolumeActionError(null);
  };

  const updateVolumeEditForm = (field: keyof VolumeEditFormState, value: string) => {
    setEditVolumeForm((current) => (current ? { ...current, [field]: value } : current));
  };

  const cancelVolumeEdit = () => {
    setEditingVolumeId(null);
    setEditVolumeForm(null);
    setVolumeActionError(null);
  };

  const submitVolumeEdit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!editingVolumeId || !editVolumeForm) return;

    setVolumeActionInFlight(true);
    setVolumeActionError(null);
    try {
      const labels = labelsFromText(editVolumeForm.labels);
      const updated = await updateVolume(
        editingVolumeId,
        {
          name: editVolumeForm.name.trim(),
          description: editVolumeForm.description.trim() || null,
          labels,
        },
        token,
      );
      setVolumes((current) =>
        current.map((volume) => (volume.id === updated.id ? updated : volume)),
      );
      setEditingVolumeId(null);
      setEditVolumeForm(null);
    } catch (err: unknown) {
      if (err instanceof ApiError && err.status === 401) {
        setAuthRequired(true);
      } else {
        setVolumeActionError(err instanceof Error ? err.message : 'volume update failed');
      }
    } finally {
      setVolumeActionInFlight(false);
    }
  };

  const deleteVolumeEntry = async (volume: VolumeResponse) => {
    if (!window.confirm(`Delete registry entry ${volume.name}?`)) return;

    setVolumeActionInFlight(true);
    setVolumeActionError(null);
    try {
      await deleteVolumeRequest(volume.id, token);
      setVolumes((current) => current.filter((entry) => entry.id !== volume.id));
      if (editingVolumeId === volume.id) {
        setEditingVolumeId(null);
        setEditVolumeForm(null);
      }
    } catch (err: unknown) {
      if (err instanceof ApiError && err.status === 401) {
        setAuthRequired(true);
      } else {
        setVolumeActionError(err instanceof Error ? err.message : 'volume delete failed');
      }
    } finally {
      setVolumeActionInFlight(false);
    }
  };

  const submitGcJob = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (selectedJobPid === null) return;

    setJobRequestInFlight(true);
    setJobError(null);
    try {
      const accepted = await runGcJob(selectedJobPid, { dry_run: gcDryRun }, token);
      const status = await fetchJobStatus(selectedJobPid, accepted.job_id, token);
      setCurrentJob({ pid: selectedJobPid, status });
    } catch (err: unknown) {
      if (err instanceof ApiError && err.status === 401) {
        setAuthRequired(true);
      } else {
        setJobError(err instanceof Error ? err.message : 'GC job request failed');
      }
    } finally {
      setJobRequestInFlight(false);
    }
  };

  const refreshCurrentJob = async () => {
    if (!currentJob) return;

    setJobRequestInFlight(true);
    setJobError(null);
    try {
      const status = await fetchJobStatus(currentJob.pid, currentJob.status.job_id, token);
      setCurrentJob({ pid: currentJob.pid, status });
    } catch (err: unknown) {
      if (err instanceof ApiError && err.status === 401) {
        setAuthRequired(true);
      } else {
        setJobError(err instanceof Error ? err.message : 'job status request failed');
      }
    } finally {
      setJobRequestInFlight(false);
    }
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
            renderPage(page, {
              health,
              error,
              instances,
              instanceDetails,
              instanceError,
              volumes,
              volumeError,
              volumeForm,
              creatingVolume,
              createVolumeError,
              editingVolumeId,
              editVolumeForm,
              volumeActionInFlight,
              volumeActionError,
              selectedJobPid,
              gcDryRun,
              jobRequestInFlight,
              jobError,
              currentJob,
              featureResult,
              featureLoading,
              featureError,
              onVolumeFormChange: updateVolumeForm,
              onVolumeSubmit: submitVolume,
              onVolumeEditStart: startVolumeEdit,
              onVolumeEditFormChange: updateVolumeEditForm,
              onVolumeEditSubmit: submitVolumeEdit,
              onVolumeEditCancel: cancelVolumeEdit,
              onVolumeDelete: deleteVolumeEntry,
              onSelectedJobPidChange: setSelectedJobPid,
              onGcDryRunChange: setGcDryRun,
              onGcJobSubmit: submitGcJob,
              onRefreshCurrentJob: refreshCurrentJob,
            })
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

type RenderContext = {
  health: HealthResponse | null;
  error: string | null;
  instances: InstanceResponse[];
  instanceDetails: Record<number, InstanceInfoResponse>;
  instanceError: string | null;
  volumes: VolumeResponse[];
  volumeError: string | null;
  volumeForm: VolumeFormState;
  creatingVolume: boolean;
  createVolumeError: string | null;
  editingVolumeId: string | null;
  editVolumeForm: VolumeEditFormState | null;
  volumeActionInFlight: boolean;
  volumeActionError: string | null;
  selectedJobPid: number | null;
  gcDryRun: boolean;
  jobRequestInFlight: boolean;
  jobError: string | null;
  currentJob: CurrentJob | null;
  featureResult: FeatureResult | null;
  featureLoading: boolean;
  featureError: string | null;
  onVolumeFormChange: (field: keyof VolumeFormState, value: string) => void;
  onVolumeSubmit: (event: FormEvent<HTMLFormElement>) => void;
  onVolumeEditStart: (volume: VolumeResponse) => void;
  onVolumeEditFormChange: (field: keyof VolumeEditFormState, value: string) => void;
  onVolumeEditSubmit: (event: FormEvent<HTMLFormElement>) => void;
  onVolumeEditCancel: () => void;
  onVolumeDelete: (volume: VolumeResponse) => void;
  onSelectedJobPidChange: (pid: number) => void;
  onGcDryRunChange: (enabled: boolean) => void;
  onGcJobSubmit: (event: FormEvent<HTMLFormElement>) => void;
  onRefreshCurrentJob: () => void;
};

function renderPage(page: PageKey, context: RenderContext) {
  const {
    health,
    error,
    instances,
    instanceDetails,
    instanceError,
    volumes,
    volumeError,
    volumeForm,
    creatingVolume,
    createVolumeError,
    editingVolumeId,
    editVolumeForm,
    volumeActionInFlight,
    volumeActionError,
    selectedJobPid,
    gcDryRun,
    jobRequestInFlight,
    jobError,
    currentJob,
    featureResult,
    featureLoading,
    featureError,
    onVolumeFormChange,
    onVolumeSubmit,
    onVolumeEditStart,
    onVolumeEditFormChange,
    onVolumeEditSubmit,
    onVolumeEditCancel,
    onVolumeDelete,
    onSelectedJobPidChange,
    onGcDryRunChange,
    onGcJobSubmit,
    onRefreshCurrentJob,
  } = context;

  if (page === 'overview') {
    return (
      <>
        <Panel title="Runtime">
          <Metric label="Service" value={health?.service ?? 'waiting'} />
          <Metric label="Commit" value={health?.commit_short ?? 'unknown'} />
          <Metric label="Auth" value={health?.auth_mode ?? 'unknown'} />
          <Metric label="Live mounts" value={String(instances.length)} />
        </Panel>
        <Panel title="Live instances">
          {instanceError ? <p className="error-text">{instanceError}</p> : null}
          {instances.length === 0 ? (
            <p className="muted">No live BrewFS mount records found.</p>
          ) : (
            <div className="instance-list">
              {instances.map((instance) => (
                <div className="instance-row" key={instance.pid}>
                  <strong>{instance.mount_point}</strong>
                  <span>
                    pid {instance.pid}
                    {instanceDetails[instance.pid]
                      ? ` · ${instanceDetails[instance.pid].meta_backend} · ${instanceDetails[instance.pid].version}`
                      : ''}
                  </span>
                  <code>{instance.socket_path}</code>
                  {instanceDetails[instance.pid] ? (
                    <span>
                      capabilities:{' '}
                      {enabledCapabilities(instanceDetails[instance.pid].capabilities).join(', ') ||
                        'none'}
                    </span>
                  ) : null}
                </div>
              ))}
            </div>
          )}
          {error ? <p className="error-text">{error}</p> : null}
        </Panel>
      </>
    );
  }

  if (page === 'filesystems') {
    return (
      <FilesystemsPage
        volumes={volumes}
        volumeError={volumeError}
        form={volumeForm}
        creating={creatingVolume}
        createError={createVolumeError}
        editingVolumeId={editingVolumeId}
        editForm={editVolumeForm}
        actionInFlight={volumeActionInFlight}
        actionError={volumeActionError}
        onChange={onVolumeFormChange}
        onSubmit={onVolumeSubmit}
        onEditStart={onVolumeEditStart}
        onEditFormChange={onVolumeEditFormChange}
        onEditSubmit={onVolumeEditSubmit}
        onEditCancel={onVolumeEditCancel}
        onDelete={onVolumeDelete}
      />
    );
  }

  if (page === 'jobs') {
    return (
      <JobsPage
        instances={instances}
        selectedPid={selectedJobPid}
        dryRun={gcDryRun}
        requestInFlight={jobRequestInFlight}
        error={jobError}
        currentJob={currentJob}
        onSelectedPidChange={onSelectedJobPidChange}
        onDryRunChange={onGcDryRunChange}
        onSubmit={onGcJobSubmit}
        onRefresh={onRefreshCurrentJob}
      />
    );
  }

  if (page === 'browser') {
    return (
      <FeatureStatusPanel
        feature="browser"
        fallbackTitle="File browser"
        result={featureResult}
        loading={featureLoading}
        error={featureError}
      />
    );
  }

  if (page === 'trash') {
    return (
      <FeatureStatusPanel
        feature="trash"
        fallbackTitle="Trash"
        result={featureResult}
        loading={featureLoading}
        error={featureError}
      />
    );
  }

  if (page === 'acl') {
    return (
      <FeatureStatusPanel
        feature="acl"
        fallbackTitle="ACL"
        result={featureResult}
        loading={featureLoading}
        error={featureError}
      />
    );
  }

  if (page === 'csi') {
    return (
      <FeatureStatusPanel
        feature="csi"
        fallbackTitle="CSI"
        result={featureResult}
        loading={featureLoading}
        error={featureError}
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

function FeatureStatusPanel({
  feature,
  fallbackTitle,
  result,
  loading,
  error,
}: {
  feature: FeatureKey;
  fallbackTitle: string;
  result: FeatureResult | null;
  loading: boolean;
  error: string | null;
}) {
  const status = result?.feature === feature ? result.status : null;

  return (
    <article className="panel empty-panel">
      <h2>{status?.title ?? fallbackTitle}</h2>
      {loading && !status ? <p className="muted">Loading status.</p> : null}
      {error ? <p className="error-text">{error}</p> : null}
      {status ? (
        <>
          <Metric label="State" value={status.state} />
          {status.volumeName ? <Metric label="Filesystem" value={status.volumeName} /> : null}
          <p className="muted feature-message">{status.message}</p>
        </>
      ) : null}
    </article>
  );
}

function JobsPage({
  instances,
  selectedPid,
  dryRun,
  requestInFlight,
  error,
  currentJob,
  onSelectedPidChange,
  onDryRunChange,
  onSubmit,
  onRefresh,
}: {
  instances: InstanceResponse[];
  selectedPid: number | null;
  dryRun: boolean;
  requestInFlight: boolean;
  error: string | null;
  currentJob: CurrentJob | null;
  onSelectedPidChange: (pid: number) => void;
  onDryRunChange: (enabled: boolean) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  onRefresh: () => void;
}) {
  if (instances.length === 0) {
    return <EmptyPanel title="No live instances" detail="GC jobs require a mounted BrewFS instance." />;
  }

  const gc = currentJob?.status.outcome?.Gc;

  return (
    <>
      <article className="panel">
        <h2>Run GC</h2>
        <form className="job-form" onSubmit={onSubmit}>
          <label>
            <span>Instance</span>
            <select
              value={selectedPid ?? ''}
              onChange={(event) => onSelectedPidChange(Number(event.target.value))}
            >
              {instances.map((instance) => (
                <option key={instance.pid} value={instance.pid}>
                  {instance.mount_point} · pid {instance.pid}
                </option>
              ))}
            </select>
          </label>
          <label className="check-row">
            <input
              type="checkbox"
              checked={dryRun}
              onChange={(event) => onDryRunChange(event.target.checked)}
            />
            <span>Dry run</span>
          </label>
          {error ? <p className="error-text">{error}</p> : null}
          <button className="primary-button" type="submit" disabled={requestInFlight}>
            <Activity size={16} aria-hidden="true" />
            <span>{requestInFlight ? 'Working' : 'Start GC'}</span>
          </button>
        </form>
      </article>

      <article className="panel">
        <h2>Latest job</h2>
        {currentJob ? (
          <>
            <Metric label="Instance" value={`pid ${currentJob.pid}`} />
            <Metric label="Job ID" value={currentJob.status.job_id} />
            <Metric label="State" value={currentJob.status.state} />
            {currentJob.status.detail ? (
              <p className="muted job-detail">{currentJob.status.detail}</p>
            ) : null}
            {gc ? (
              <div className="job-metrics">
                <Metric label="Orphan slices" value={String(gc.orphan_slice_count)} />
                <Metric label="Orphan objects" value={String(gc.orphan_object_count)} />
                <Metric label="Deleted objects" value={String(gc.deleted_object_count)} />
                <Metric label="Errors" value={String(gc.error_count)} />
              </div>
            ) : null}
            <button
              className="secondary-button"
              type="button"
              onClick={onRefresh}
              disabled={requestInFlight}
            >
              <RefreshCw size={16} aria-hidden="true" />
              <span>Refresh</span>
            </button>
          </>
        ) : (
          <p className="muted">No job has been started from this console session.</p>
        )}
      </article>
    </>
  );
}

function FilesystemsPage({
  volumes,
  volumeError,
  form,
  creating,
  createError,
  editingVolumeId,
  editForm,
  actionInFlight,
  actionError,
  onChange,
  onSubmit,
  onEditStart,
  onEditFormChange,
  onEditSubmit,
  onEditCancel,
  onDelete,
}: {
  volumes: VolumeResponse[];
  volumeError: string | null;
  form: VolumeFormState;
  creating: boolean;
  createError: string | null;
  editingVolumeId: string | null;
  editForm: VolumeEditFormState | null;
  actionInFlight: boolean;
  actionError: string | null;
  onChange: (field: keyof VolumeFormState, value: string) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  onEditStart: (volume: VolumeResponse) => void;
  onEditFormChange: (field: keyof VolumeEditFormState, value: string) => void;
  onEditSubmit: (event: FormEvent<HTMLFormElement>) => void;
  onEditCancel: () => void;
  onDelete: (volume: VolumeResponse) => void;
}) {
  return (
    <>
      <article className="panel table-panel">
        <h2>Registered filesystems</h2>
        {volumeError ? <p className="error-text">{volumeError}</p> : null}
        {actionError ? <p className="error-text">{actionError}</p> : null}
        {volumes.length === 0 ? (
          <p className="muted">No registered filesystems.</p>
        ) : (
          <div className="table-wrap">
            <table className="data-table">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Data</th>
                  <th>Meta</th>
                  <th>Mount</th>
                  <th>Meta URL</th>
                  <th>Actions</th>
                </tr>
              </thead>
              <tbody>
                {volumes.map((volume) => (
                  <tr key={volume.id}>
                    <td>{volume.name}</td>
                    <td>{volume.mount_config.data_backend}</td>
                    <td>{volume.mount_config.meta_backend}</td>
                    <td>{volume.mount_config.mount_point ?? '-'}</td>
                    <td>{volume.mount_config.meta_url_redacted ?? '-'}</td>
                    <td>
                      <div className="table-actions">
                        <button
                          className="secondary-button compact-button"
                          type="button"
                          onClick={() => onEditStart(volume)}
                          disabled={actionInFlight}
                        >
                          <Pencil size={14} aria-hidden="true" />
                          <span>Edit</span>
                        </button>
                        <button
                          className="danger-button compact-button"
                          type="button"
                          onClick={() => onDelete(volume)}
                          disabled={actionInFlight}
                        >
                          <Trash2 size={14} aria-hidden="true" />
                          <span>Delete</span>
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </article>

      <article className="panel">
        <h2>Create registry entry</h2>
        <form className="volume-form" onSubmit={onSubmit}>
          <label>
            <span>Name</span>
            <input
              required
              value={form.name}
              onChange={(event) => onChange('name', event.target.value)}
            />
          </label>
          <label>
            <span>Mount point</span>
            <input
              value={form.mount_point}
              onChange={(event) => onChange('mount_point', event.target.value)}
            />
          </label>
          <label>
            <span>Data backend</span>
            <input
              required
              value={form.data_backend}
              onChange={(event) => onChange('data_backend', event.target.value)}
            />
          </label>
          <label>
            <span>Data dir</span>
            <input
              value={form.data_dir}
              onChange={(event) => onChange('data_dir', event.target.value)}
            />
          </label>
          <label>
            <span>Meta backend</span>
            <input
              required
              value={form.meta_backend}
              onChange={(event) => onChange('meta_backend', event.target.value)}
            />
          </label>
          <label>
            <span>Meta URL</span>
            <input
              type="password"
              value={form.meta_url}
              onChange={(event) => onChange('meta_url', event.target.value)}
            />
          </label>
          {createError ? <p className="error-text">{createError}</p> : null}
          <button className="primary-button" type="submit" disabled={creating}>
            <HardDrive size={16} aria-hidden="true" />
            <span>{creating ? 'Creating' : 'Create'}</span>
          </button>
        </form>
      </article>

      {editingVolumeId && editForm ? (
        <article className="panel">
          <h2>Edit registry entry</h2>
          <form className="volume-form" onSubmit={onEditSubmit}>
            <label>
              <span>Name</span>
              <input
                required
                value={editForm.name}
                onChange={(event) => onEditFormChange('name', event.target.value)}
              />
            </label>
            <label>
              <span>Description</span>
              <input
                value={editForm.description}
                onChange={(event) => onEditFormChange('description', event.target.value)}
              />
            </label>
            <label>
              <span>Labels</span>
              <textarea
                value={editForm.labels}
                onChange={(event) => onEditFormChange('labels', event.target.value)}
              />
            </label>
            <div className="form-actions">
              <button className="primary-button" type="submit" disabled={actionInFlight}>
                <HardDrive size={16} aria-hidden="true" />
                <span>{actionInFlight ? 'Saving' : 'Save'}</span>
              </button>
              <button
                className="secondary-button"
                type="button"
                onClick={onEditCancel}
                disabled={actionInFlight}
              >
                <X size={16} aria-hidden="true" />
                <span>Cancel</span>
              </button>
            </div>
          </form>
        </article>
      ) : null}
    </>
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

function optionalField(value: string): string | undefined {
  const trimmed = value.trim();
  return trimmed ? trimmed : undefined;
}

function enabledCapabilities(capabilities: Record<string, boolean>): string[] {
  return Object.entries(capabilities)
    .filter(([, enabled]) => enabled)
    .map(([name]) => name);
}
