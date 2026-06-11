use super::{
    AuthMode, ConsoleState,
    registry::{CreateVolumeRequest, RegistryError, UpdateVolumeRequest, VolumeResponse},
};
use crate::{
    control::{
        client::send_request,
        job::{JobOutcome, JobState},
        protocol::{ControlRequest, ControlResponse},
        runtime::InstanceRecord,
    },
    meta::store::MetaStoreCapabilities,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::{path::Path as FsPath, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HealthResponse {
    pub service: &'static str,
    pub version: &'static str,
    pub commit_short: &'static str,
    pub auth_mode: AuthMode,
    pub integrations: HealthIntegrations,
    pub static_assets_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HealthIntegrations {
    pub csi_dashboard: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ListVolumesResponse {
    pub volumes: Vec<VolumeResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ListInstancesResponse {
    pub instances: Vec<InstanceResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InstanceResponse {
    pub pid: u32,
    pub mount_point: String,
    pub socket_path: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InstanceInfoResponse {
    pub pid: u32,
    pub mount_point: String,
    pub started_at: i64,
    pub version: String,
    pub meta_backend: String,
    pub capabilities: MetaStoreCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RunGcJobRequest {
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AcceptedJobResponse {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct JobStatusResponse {
    pub job_id: String,
    pub state: JobState,
    pub detail: Option<String>,
    pub outcome: Option<JobOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PathQuery {
    pub path: Option<String>,
}

impl From<InstanceRecord> for InstanceResponse {
    fn from(record: InstanceRecord) -> Self {
        Self {
            pid: record.pid,
            mount_point: record.mount_point,
            socket_path: record.socket_path.to_string_lossy().into_owned(),
            started_at: record.started_at,
        }
    }
}

impl HealthResponse {
    pub fn from_state(state: &ConsoleState, static_assets_available: bool) -> Self {
        Self {
            service: "brewfs-console",
            version: env!("CARGO_PKG_VERSION"),
            commit_short: env!("BREWFS_GIT_COMMIT_SHORT"),
            auth_mode: state.auth.mode(),
            integrations: HealthIntegrations {
                csi_dashboard: state.csi_dashboard,
            },
            static_assets_available,
        }
    }
}

pub async fn health(State(state): State<ConsoleState>) -> Json<HealthResponse> {
    let static_assets_available = state.static_dir.join("index.html").is_file();
    Json(HealthResponse::from_state(&state, static_assets_available))
}

pub async fn list_volumes(
    State(state): State<ConsoleState>,
) -> Result<Json<ListVolumesResponse>, ApiErrorResponse> {
    let volumes = state
        .registry
        .list()
        .await
        .map_err(ApiErrorResponse::from)?;
    Ok(Json(ListVolumesResponse { volumes }))
}

pub async fn create_volume(
    State(state): State<ConsoleState>,
    Json(request): Json<CreateVolumeRequest>,
) -> Result<(StatusCode, Json<VolumeResponse>), ApiErrorResponse> {
    let volume = state
        .registry
        .create(request)
        .await
        .map_err(ApiErrorResponse::from)?;
    Ok((StatusCode::CREATED, Json(volume)))
}

pub async fn get_volume(
    State(state): State<ConsoleState>,
    Path(volume_id): Path<String>,
) -> Result<Json<VolumeResponse>, ApiErrorResponse> {
    let volume = state
        .registry
        .get(&volume_id)
        .await
        .map_err(ApiErrorResponse::from)?;
    Ok(Json(volume))
}

pub async fn update_volume(
    State(state): State<ConsoleState>,
    Path(volume_id): Path<String>,
    Json(request): Json<UpdateVolumeRequest>,
) -> Result<Json<VolumeResponse>, ApiErrorResponse> {
    let volume = state
        .registry
        .update(&volume_id, request)
        .await
        .map_err(ApiErrorResponse::from)?;
    Ok(Json(volume))
}

pub async fn delete_volume(
    State(state): State<ConsoleState>,
    Path(volume_id): Path<String>,
) -> Result<StatusCode, ApiErrorResponse> {
    state
        .registry
        .delete(&volume_id)
        .await
        .map_err(ApiErrorResponse::from)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_instances(
    State(state): State<ConsoleState>,
) -> Result<Json<ListInstancesResponse>, ApiErrorResponse> {
    let instances = state
        .runtime_registry
        .list_instances()
        .await
        .map_err(|err| {
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime_registry_error",
                format!("failed to read runtime registry: {err}"),
            )
        })?
        .into_iter()
        .map(InstanceResponse::from)
        .collect();
    Ok(Json(ListInstancesResponse { instances }))
}

pub async fn get_instance_info(
    State(state): State<ConsoleState>,
    Path(requested_pid): Path<u32>,
) -> Result<Json<InstanceInfoResponse>, ApiErrorResponse> {
    let response =
        send_instance_control_request(&state, requested_pid, &ControlRequest::GetInfo).await?;

    match response {
        ControlResponse::Info {
            pid,
            mount_point,
            started_at,
            version,
            meta_backend,
            capabilities,
        } => {
            if pid != requested_pid {
                return Err(json_error(
                    StatusCode::BAD_GATEWAY,
                    "control_plane_error",
                    format!("control-plane pid mismatch: requested {requested_pid}, got {pid}"),
                ));
            }
            Ok(Json(InstanceInfoResponse {
                pid,
                mount_point,
                started_at,
                version,
                meta_backend,
                capabilities,
            }))
        }
        ControlResponse::Error { code, message } => Err(json_error(
            StatusCode::BAD_GATEWAY,
            "control_plane_error",
            format!("{code}: {message}"),
        )),
        other => Err(json_error(
            StatusCode::BAD_GATEWAY,
            "control_plane_error",
            format!("unexpected control-plane response: {other:?}"),
        )),
    }
}

pub async fn start_gc_job(
    State(state): State<ConsoleState>,
    Path(pid): Path<u32>,
    Json(request): Json<RunGcJobRequest>,
) -> Result<(StatusCode, Json<AcceptedJobResponse>), ApiErrorResponse> {
    let response = send_instance_control_request(
        &state,
        pid,
        &ControlRequest::RunGc {
            dry_run: request.dry_run,
        },
    )
    .await?;

    match response {
        ControlResponse::Accepted { job_id } => {
            Ok((StatusCode::ACCEPTED, Json(AcceptedJobResponse { job_id })))
        }
        ControlResponse::Error { code, message } => Err(json_error(
            StatusCode::BAD_GATEWAY,
            "control_plane_error",
            format!("{code}: {message}"),
        )),
        other => Err(json_error(
            StatusCode::BAD_GATEWAY,
            "control_plane_error",
            format!("unexpected control-plane response: {other:?}"),
        )),
    }
}

pub async fn get_job_status(
    State(state): State<ConsoleState>,
    Path((pid, requested_job_id)): Path<(u32, String)>,
) -> Result<Json<JobStatusResponse>, ApiErrorResponse> {
    let response = send_instance_control_request(
        &state,
        pid,
        &ControlRequest::GetJob {
            job_id: requested_job_id.clone(),
        },
    )
    .await?;

    match response {
        ControlResponse::JobStatus {
            job_id,
            state,
            detail,
            outcome,
        } => {
            if job_id != requested_job_id {
                return Err(json_error(
                    StatusCode::BAD_GATEWAY,
                    "control_plane_error",
                    format!(
                        "control-plane job id mismatch: requested {requested_job_id}, got {job_id}"
                    ),
                ));
            }
            Ok(Json(JobStatusResponse {
                job_id,
                state,
                detail,
                outcome,
            }))
        }
        ControlResponse::Error { code, message } => Err(json_error(
            StatusCode::BAD_GATEWAY,
            "control_plane_error",
            format!("{code}: {message}"),
        )),
        other => Err(json_error(
            StatusCode::BAD_GATEWAY,
            "control_plane_error",
            format!("unexpected control-plane response: {other:?}"),
        )),
    }
}

pub async fn list_files(
    Path(_volume_id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiErrorResponse> {
    let _path = query.path.unwrap_or_else(|| "/".to_string());
    Err(unsupported(
        "file browser APIs are not implemented for BrewFS volumes yet",
    ))
}

pub async fn list_trash(
    Path(_volume_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiErrorResponse> {
    Err(unsupported(
        "trash APIs are not implemented for BrewFS volumes yet",
    ))
}

pub async fn restore_trash_entry(
    Path((_volume_id, _entry_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiErrorResponse> {
    Err(unsupported(
        "trash APIs are not implemented for BrewFS volumes yet",
    ))
}

pub async fn delete_trash_entry(
    Path((_volume_id, _entry_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiErrorResponse> {
    Err(unsupported(
        "trash APIs are not implemented for BrewFS volumes yet",
    ))
}

pub async fn get_acl(
    Path(_volume_id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiErrorResponse> {
    let _path = query.path.unwrap_or_else(|| "/".to_string());
    Err(unsupported(
        "ACL APIs are not implemented for BrewFS volumes yet",
    ))
}

pub async fn put_acl(
    Path(_volume_id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiErrorResponse> {
    let _path = query.path.unwrap_or_else(|| "/".to_string());
    Err(unsupported(
        "ACL APIs are not implemented for BrewFS volumes yet",
    ))
}

pub async fn delete_acl(
    Path(_volume_id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<serde_json::Value>, ApiErrorResponse> {
    let _path = query.path.unwrap_or_else(|| "/".to_string());
    Err(unsupported(
        "ACL APIs are not implemented for BrewFS volumes yet",
    ))
}

pub async fn csi_summary(
    State(state): State<ConsoleState>,
) -> Result<Json<serde_json::Value>, ApiErrorResponse> {
    if state.csi_dashboard {
        Err(unsupported("CSI dashboard adapter is not implemented yet"))
    } else {
        Err(unsupported("CSI dashboard is disabled"))
    }
}

async fn send_instance_control_request(
    state: &ConsoleState,
    pid: u32,
    request: &ControlRequest,
) -> Result<ControlResponse, ApiErrorResponse> {
    let record = find_runtime_record(state, pid).await?;
    send_control_request(&record.socket_path, request).await
}

async fn find_runtime_record(
    state: &ConsoleState,
    pid: u32,
) -> Result<InstanceRecord, ApiErrorResponse> {
    state
        .runtime_registry
        .list_instances()
        .await
        .map_err(|err| {
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime_registry_error",
                format!("failed to read runtime registry: {err}"),
            )
        })?
        .into_iter()
        .find(|record| record.pid == pid)
        .ok_or_else(|| {
            json_error(
                StatusCode::NOT_FOUND,
                "not_found",
                "runtime instance not found",
            )
        })
}

async fn send_control_request(
    socket_path: &FsPath,
    request: &ControlRequest,
) -> Result<ControlResponse, ApiErrorResponse> {
    tokio::time::timeout(Duration::from_secs(2), send_request(socket_path, request))
        .await
        .map_err(|_| {
            json_error(
                StatusCode::BAD_GATEWAY,
                "control_plane_error",
                "control-plane request timed out",
            )
        })?
        .map_err(|err| {
            json_error(
                StatusCode::BAD_GATEWAY,
                "control_plane_error",
                format!("control-plane request failed: {err}"),
            )
        })
}

fn unsupported(message: impl Into<String>) -> ApiErrorResponse {
    json_error(StatusCode::NOT_IMPLEMENTED, "unsupported", message)
}

pub fn json_error(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
) -> ApiErrorResponse {
    ApiErrorResponse {
        status,
        code,
        message: message.into(),
    }
}

#[derive(Debug)]
pub struct ApiErrorResponse {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl From<RegistryError> for ApiErrorResponse {
    fn from(err: RegistryError) -> Self {
        let status = match err.code() {
            "invalid_config" => StatusCode::BAD_REQUEST,
            "not_found" => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            code: err.code(),
            message: err.message().to_owned(),
        }
    }
}

impl IntoResponse for ApiErrorResponse {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        console::{AuthConfig, AuthMode, ConsoleState, registry::VolumeRegistry},
        control::runtime::RuntimeRegistry,
    };
    use std::path::PathBuf;

    #[test]
    fn health_response_uses_build_metadata_and_state() {
        let static_dir = PathBuf::from("/tmp/brewfs-console-dist");
        let state = ConsoleState {
            auth: AuthConfig::Disabled,
            static_dir: static_dir.clone(),
            registry: VolumeRegistry::new(static_dir.join("state")),
            runtime_registry: RuntimeRegistry::new(static_dir.join("runtime")),
            csi_dashboard: true,
        };

        let response = HealthResponse::from_state(&state, true);

        assert_eq!(response.service, "brewfs-console");
        assert_eq!(response.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(response.commit_short, env!("BREWFS_GIT_COMMIT_SHORT"));
        assert_eq!(response.auth_mode, AuthMode::Disabled);
        assert!(response.integrations.csi_dashboard);
        assert!(response.static_assets_available);
    }
}
