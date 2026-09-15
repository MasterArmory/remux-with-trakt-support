use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use remux_macros::{delete, get, post};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AppState, IntoApiError, OptionExt,
    addons::media_tracker::{DeviceAuthPoll, MediaTrackerCtx},
    db::{self, MediaTrackerStatus, UserMediaTracker, auth},
};
use axum_anyhow::ApiResult as Result;

fn require_self_or_admin(target_id: Uuid, session: &auth::AuthSession) -> Result<()> {
    if target_id
        != session
            .user
            .id
        && !session
            .user
            .is_admin
    {
        return Err(anyhow::anyhow!("Forbidden").context_unauthorized("forbidden"));
    }
    Ok(())
}

fn tracker_ctx(state: &AppState) -> MediaTrackerCtx {
    MediaTrackerCtx {
        config: std::sync::Arc::new(
            state
                .ctx
                .config
                .clone(),
        ),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaTrackerDto {
    pub id: Uuid,
    pub addon_id: Uuid,
    pub status: MediaTrackerStatus,
    pub last_success_at: Option<chrono::NaiveDateTime>,
    pub last_error: Option<String>,
}

impl From<UserMediaTracker> for MediaTrackerDto {
    fn from(row: UserMediaTracker) -> Self {
        Self {
            id: row.id,
            addon_id: row.addon_id,
            status: row.status,
            last_success_at: row.last_success_at,
            last_error: row.last_error,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthStartDto {
    pub verification_url: String,
    pub user_code: String,
    pub poll_token: String,
    pub interval_seconds: u64,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthPollRequest {
    pub poll_token: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthPollDto {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<MediaTrackerDto>,
}

#[get("/users/{user_id}/mediatrackers")]
pub async fn list_media_trackers(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path(user_id): Path<Uuid>,
) -> Result<Json<Vec<MediaTrackerDto>>> {
    require_self_or_admin(user_id, &session)?;
    let rows = UserMediaTracker::list_for_user(
        &state
            .ctx
            .db,
        user_id,
    )
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(MediaTrackerDto::from)
            .collect(),
    ))
}

#[post("/users/{user_id}/mediatrackers/{addon_id}/deviceauth")]
pub async fn begin_device_auth(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path((user_id, addon_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<DeviceAuthStartDto>> {
    require_self_or_admin(user_id, &session)?;
    let addon = state
        .ctx
        .addons
        .media_tracker_for(addon_id)
        .context_not_found("Trakt addon is not enabled")?;
    let start = addon
        .begin_device_auth(&tracker_ctx(&state))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Json(DeviceAuthStartDto {
        verification_url: start.verification_url,
        user_code: start.user_code,
        poll_token: start.poll_token,
        interval_seconds: start
            .interval
            .as_secs(),
        expires_in_seconds: start
            .expires_in
            .as_secs(),
    }))
}

#[post("/users/{user_id}/mediatrackers/{addon_id}/deviceauth/poll")]
pub async fn poll_device_auth(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path((user_id, addon_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<DeviceAuthPollRequest>,
) -> Result<Json<DeviceAuthPollDto>> {
    require_self_or_admin(user_id, &session)?;
    let addon = state
        .ctx
        .addons
        .media_tracker_for(addon_id)
        .context_not_found("Trakt addon is not enabled")?;
    match addon
        .poll_device_auth(&payload.poll_token, &tracker_ctx(&state))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
    {
        DeviceAuthPoll::Pending => Ok(Json(DeviceAuthPollDto {
            status: "pending".into(),
            connection: None,
        })),
        DeviceAuthPoll::Denied => Ok(Json(DeviceAuthPollDto {
            status: "denied".into(),
            connection: None,
        })),
        DeviceAuthPoll::Approved(creds) => {
            let filters = addon
                .capabilities()
                .default_event_filter;
            let row = UserMediaTracker::new(user_id, addon_id, creds, filters);
            row.upsert(
                &state
                    .ctx
                    .db,
            )
            .await?;
            if let Ok(watches) = addon
                .import_history(&row.credentials, &tracker_ctx(&state))
                .await
            {
                let _ = crate::services::media_tracker::apply_remote_watches(
                    &state.ctx,
                    user_id,
                    &watches,
                    usize::MAX,
                )
                .await;
            }
            Ok(Json(DeviceAuthPollDto {
                status: "approved".into(),
                connection: Some(MediaTrackerDto::from(row)),
            }))
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaTrackerSyncDto {
    pub applied: usize,
}

#[post("/users/{user_id}/mediatrackers/{addon_id}/sync")]
pub async fn sync_media_tracker(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path((user_id, addon_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<MediaTrackerSyncDto>> {
    require_self_or_admin(user_id, &session)?;
    let addon = state
        .ctx
        .addons
        .media_tracker_for(addon_id)
        .context_not_found("Trakt addon is not enabled")?;
    let row = UserMediaTracker::get_for_user_and_addon(
        &state
            .ctx
            .db,
        user_id,
        addon_id,
    )
    .await?
    .context_not_found("Trakt is not connected for this user")?;
    let watches = addon
        .import_history(&row.credentials, &tracker_ctx(&state))
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let applied = crate::services::media_tracker::apply_remote_watches(
        &state.ctx,
        user_id,
        &watches,
        usize::MAX,
    )
    .await?;
    Ok(Json(MediaTrackerSyncDto { applied }))
}

#[delete("/users/{user_id}/mediatrackers/{addon_id}")]
pub async fn disconnect_media_tracker(
    State(state): State<AppState>,
    session: auth::AuthSession,
    Path((user_id, addon_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse> {
    require_self_or_admin(user_id, &session)?;
    if let Some(existing) = UserMediaTracker::get_for_user_and_addon(
        &state
            .ctx
            .db,
        user_id,
        addon_id,
    )
    .await?
    {
        if let Some(addon) = state
            .ctx
            .addons
            .media_tracker_for(addon_id)
        {
            let _ = addon
                .disconnect(&existing.credentials, &tracker_ctx(&state))
                .await;
        }
        UserMediaTracker::delete(
            &state
                .ctx
                .db,
            existing.id,
        )
        .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
