import {
  ApiError,
  fetchAcl,
  fetchCsiSummary,
  fetchFileList,
  fetchTrash,
  type VolumeResponse,
} from './api';

export type FeatureKey = 'browser' | 'trash' | 'acl' | 'csi';
export type FeatureState = 'ready' | 'unsupported' | 'unavailable';

export interface FeatureStatus {
  state: FeatureState;
  title: string;
  message: string;
  volumeName?: string;
}

export async function loadFeatureStatus(
  feature: FeatureKey,
  volumes: VolumeResponse[],
  token?: string | null,
): Promise<FeatureStatus> {
  if (feature === 'csi') {
    try {
      const summary = await fetchCsiSummary(token);
      return {
        state: 'ready',
        title: 'CSI summary',
        message: `${summary.storageclasses} storage classes, ${summary.persistentvolumes} PVs, ${summary.persistentvolumeclaims} PVCs, ${summary.pods} pods, ${summary.unhealthy_mounts} unhealthy mounts`,
      };
    } catch (err: unknown) {
      return unsupportedOrThrow(err, 'CSI dashboard unavailable');
    }
  }

  const volume = volumes[0];
  if (!volume) {
    return {
      state: 'unavailable',
      title: 'No registered filesystems',
      message: 'Register a filesystem before using this view.',
    };
  }

  try {
    if (feature === 'browser') {
      await fetchFileList(volume.id, '/', token);
    } else if (feature === 'trash') {
      await fetchTrash(volume.id, token);
    } else {
      await fetchAcl(volume.id, '/', token);
    }

    return {
      state: 'ready',
      title: titleForFeature(feature),
      message: `${titleForFeature(feature)} is available for ${volume.name}.`,
      volumeName: volume.name,
    };
  } catch (err: unknown) {
    return {
      ...unsupportedOrThrow(err, `${titleForFeature(feature)} unavailable`),
      volumeName: volume.name,
    };
  }
}

function unsupportedOrThrow(err: unknown, title: string): FeatureStatus {
  if (err instanceof ApiError && err.status === 501) {
    return {
      state: 'unsupported',
      title,
      message: 'The server exposes this endpoint but the capability is not implemented yet.',
    };
  }
  throw err;
}

function titleForFeature(feature: Exclude<FeatureKey, 'csi'>): string {
  if (feature === 'browser') return 'File browser';
  if (feature === 'trash') return 'Trash';
  return 'ACL';
}
