use http::Method;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Body, Endpoint};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MediaTrackerDto {
    pub id: Uuid,
    pub addon_id: Uuid,
    pub status: String,
    #[serde(default)]
    pub last_success_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthStartDto {
    pub verification_url: String,
    pub user_code: String,
    pub poll_token: String,
    pub interval_seconds: u64,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthPollDto {
    pub status: String,
    #[serde(default)]
    pub connection: Option<MediaTrackerDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MediaTrackerSyncDto {
    pub applied: usize,
}

#[derive(Debug, Clone)]
pub struct ListMediaTrackers {
    pub user_id: Uuid,
}

impl Endpoint for ListMediaTrackers {
    type Output = Vec<MediaTrackerDto>;
    fn path(&self) -> String {
        format!("/users/{}/mediatrackers", self.user_id)
    }
}

#[derive(Debug, Clone)]
pub struct BeginMediaTrackerDeviceAuth {
    pub user_id: Uuid,
    pub addon_id: Uuid,
}

impl Endpoint for BeginMediaTrackerDeviceAuth {
    type Output = DeviceAuthStartDto;
    fn path(&self) -> String {
        format!(
            "/users/{}/mediatrackers/{}/deviceauth",
            self.user_id, self.addon_id
        )
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn body(&self) -> Body {
        Body::Json(serde_json::json!({}))
    }
}

#[derive(Debug, Clone)]
pub struct PollMediaTrackerDeviceAuth {
    pub user_id: Uuid,
    pub addon_id: Uuid,
    pub poll_token: String,
}

impl Endpoint for PollMediaTrackerDeviceAuth {
    type Output = DeviceAuthPollDto;
    fn path(&self) -> String {
        format!(
            "/users/{}/mediatrackers/{}/deviceauth/poll",
            self.user_id, self.addon_id
        )
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn body(&self) -> Body {
        Body::Json(serde_json::json!({ "pollToken": self.poll_token }))
    }
}

#[derive(Debug, Clone)]
pub struct SyncMediaTracker {
    pub user_id: Uuid,
    pub addon_id: Uuid,
}

impl Endpoint for SyncMediaTracker {
    type Output = MediaTrackerSyncDto;
    fn path(&self) -> String {
        format!(
            "/users/{}/mediatrackers/{}/sync",
            self.user_id, self.addon_id
        )
    }
    fn method(&self) -> Method {
        Method::POST
    }
    fn body(&self) -> Body {
        Body::Json(serde_json::json!({}))
    }
}
