use crate::{Auth, Body, ClientError, Endpoint, RestClient};
use http::Method;
use serde::{Deserialize, Serialize};

pub const DEFAULT_BASE_URL: &str = "https://api.trakt.tv";

#[derive(Clone, Debug)]
pub struct TraktAuth {
    pub client_id: String,
    pub access_token: Option<String>,
}

impl Auth for TraktAuth {
    fn apply(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = req
            .header("trakt-api-key", &self.client_id)
            .header("trakt-api-version", "2")
            .header("User-Agent", "remux/1.0");
        if let Some(token) = &self.access_token {
            req = req.bearer_auth(token);
        }
        req
    }
}

pub fn trakt_client(
    client_id: &str,
    base_url: &str,
) -> Result<RestClient<TraktAuth>, url::ParseError> {
    trakt_user_client(client_id, None, base_url)
}

pub fn trakt_user_client(
    client_id: &str,
    access_token: Option<&str>,
    base_url: &str,
) -> Result<RestClient<TraktAuth>, url::ParseError> {
    Ok(RestClient::new(base_url)?
        .with_auth(TraktAuth {
            client_id: client_id.to_string(),
            access_token: access_token.map(ToOwned::to_owned),
        })
        .with_retry(crate::ExponentialBackoff::builder().build_with_max_retries(3)))
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TraktItemIds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trakt: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmdb: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvdb: Option<i64>,
}

impl TraktItemIds {
    pub fn is_empty(&self) -> bool {
        self.trakt
            .is_none()
            && self
                .slug
                .is_none()
            && self
                .imdb
                .is_none()
            && self
                .tmdb
                .is_none()
            && self
                .tvdb
                .is_none()
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct TraktMovie {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default)]
    pub ids: TraktItemIds,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct TraktShow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(default)]
    pub ids: TraktItemIds,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct TraktEpisode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<i64>,
    #[serde(default)]
    pub ids: TraktItemIds,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TraktPopularItem {
    pub ids: TraktItemIds,
}

#[derive(Debug, Clone, Serialize)]
pub struct PopularParams {
    pub limit: u32,
}

#[derive(Debug, Clone)]
pub struct MoviePopularEndpoint {
    pub limit: u32,
}

impl Endpoint for MoviePopularEndpoint {
    type Output = Vec<TraktPopularItem>;

    fn path(&self) -> String {
        "movies/popular".to_string()
    }

    fn query_params(&self) -> impl serde::Serialize + '_ {
        PopularParams { limit: self.limit }
    }
}

#[derive(Debug, Clone)]
pub struct ShowPopularEndpoint {
    pub limit: u32,
}

impl Endpoint for ShowPopularEndpoint {
    type Output = Vec<TraktPopularItem>;

    fn path(&self) -> String {
        "shows/popular".to_string()
    }

    fn query_params(&self) -> impl serde::Serialize + '_ {
        PopularParams { limit: self.limit }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TraktStats {
    pub watchers: u64,
    pub recommended: u64,
    pub favorited: u64,
}

impl TraktStats {
    pub fn raw_score(&self) -> f64 {
        self.watchers as f64
            + self.recommended as f64 * 20.0
            + self.favorited as f64 * 10.0
    }
}

#[derive(Debug, Clone)]
pub struct MovieStatsEndpoint {
    pub imdb_id: String,
}

impl Endpoint for MovieStatsEndpoint {
    type Output = TraktStats;

    fn path(&self) -> String {
        format!("movies/{}/stats", self.imdb_id)
    }

    fn query_params(&self) -> impl serde::Serialize + '_ {
        ()
    }
}

#[derive(Debug, Clone)]
pub struct ShowStatsEndpoint {
    pub imdb_id: String,
}

impl Endpoint for ShowStatsEndpoint {
    type Output = TraktStats;

    fn path(&self) -> String {
        format!("shows/{}/stats", self.imdb_id)
    }

    fn query_params(&self) -> impl serde::Serialize + '_ {
        ()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, Serialize)]
struct DeviceCodeRequest {
    client_id: String,
}

#[derive(Debug, Clone)]
pub struct DeviceCodeEndpoint {
    pub client_id: String,
}

impl Endpoint for DeviceCodeEndpoint {
    type Output = DeviceCodeResponse;

    fn path(&self) -> String {
        "oauth/device/code".to_string()
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(
            serde_json::to_value(DeviceCodeRequest {
                client_id: self
                    .client_id
                    .clone(),
            })
            .unwrap_or_default(),
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub refresh_token: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub created_at: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TokenErrorBody {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DeviceTokenRequest {
    code: String,
    client_id: String,
    client_secret: String,
}

#[derive(Debug, Clone)]
pub struct DeviceTokenEndpoint {
    pub code: String,
    pub client_id: String,
    pub client_secret: String,
}

impl Endpoint for DeviceTokenEndpoint {
    type Output = TokenResponse;

    fn path(&self) -> String {
        "oauth/device/token".to_string()
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(
            serde_json::to_value(DeviceTokenRequest {
                code: self
                    .code
                    .clone(),
                client_id: self
                    .client_id
                    .clone(),
                client_secret: self
                    .client_secret
                    .clone(),
            })
            .unwrap_or_default(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceTokenStatus {
    Approved(TokenResponse),
    Pending,
    SlowDown,
    Denied,
    Expired,
}

pub fn device_token_status_from_error(err: &ClientError) -> Option<DeviceTokenStatus> {
    let ClientError::Http {
        status,
        message,
        body,
        ..
    } = err
    else {
        return None;
    };
    if *status != 400 && *status != 404 && *status != 418 {
        return None;
    }
    let raw = body
        .as_ref()
        .map(|b| {
            b.expose()
                .as_str()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or(message.as_str());
    let error = serde_json::from_str::<TokenErrorBody>(raw)
        .ok()
        .map(|b| b.error)
        .unwrap_or_else(|| raw.to_string());
    Some(match error.as_str() {
        "authorization_pending" => DeviceTokenStatus::Pending,
        "slow_down" => DeviceTokenStatus::SlowDown,
        "access_denied" => DeviceTokenStatus::Denied,
        "expired_token" | "invalid_grant" => DeviceTokenStatus::Expired,
        _ if *status == 404 => DeviceTokenStatus::Expired,
        _ => return None,
    })
}

#[derive(Debug, Clone, Serialize)]
struct RefreshTokenRequest {
    refresh_token: String,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    grant_type: String,
}

#[derive(Debug, Clone)]
pub struct RefreshTokenEndpoint {
    pub refresh_token: String,
    pub client_id: String,
    pub client_secret: String,
}

impl Endpoint for RefreshTokenEndpoint {
    type Output = TokenResponse;

    fn path(&self) -> String {
        "oauth/token".to_string()
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(
            serde_json::to_value(RefreshTokenRequest {
                refresh_token: self
                    .refresh_token
                    .clone(),
                client_id: self
                    .client_id
                    .clone(),
                client_secret: self
                    .client_secret
                    .clone(),
                redirect_uri: "urn:ietf:wg:oauth:2.0:oob".to_string(),
                grant_type: "refresh_token".to_string(),
            })
            .unwrap_or_default(),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
struct RevokeRequest {
    token: String,
    client_id: String,
    client_secret: String,
}

#[derive(Debug, Clone)]
pub struct RevokeEndpoint {
    pub token: String,
    pub client_id: String,
    pub client_secret: String,
}

impl Endpoint for RevokeEndpoint {
    type Output = ();

    fn path(&self) -> String {
        "oauth/revoke".to_string()
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(
            serde_json::to_value(RevokeRequest {
                token: self
                    .token
                    .clone(),
                client_id: self
                    .client_id
                    .clone(),
                client_secret: self
                    .client_secret
                    .clone(),
            })
            .unwrap_or_default(),
        )
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct UserSettings {
    #[serde(default)]
    pub user: TraktUser,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TraktUser {
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UserSettingsEndpoint;

impl Endpoint for UserSettingsEndpoint {
    type Output = UserSettings;

    fn path(&self) -> String {
        "users/settings".to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrobbleAction {
    Start,
    Pause,
    Stop,
}

impl ScrobbleAction {
    fn path(self) -> &'static str {
        match self {
            Self::Start => "scrobble/start",
            Self::Pause => "scrobble/pause",
            Self::Stop => "scrobble/stop",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScrobbleBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub movie: Option<TraktMovie>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show: Option<TraktShow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<TraktEpisode>,
    pub progress: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScrobbleResponse {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub progress: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ScrobbleEndpoint {
    pub action: ScrobbleAction,
    pub body: ScrobbleBody,
}

impl Endpoint for ScrobbleEndpoint {
    type Output = ScrobbleResponse;

    fn path(&self) -> String {
        self.action
            .path()
            .to_string()
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(serde_json::to_value(&self.body).unwrap_or_default())
    }
}

/// Jellyfin-style ticks (10_000_000 = 1s) to Trakt's 0–100 progress.
pub fn playback_progress_percent(
    position_ticks: i64,
    runtime_ticks: Option<i64>,
) -> f64 {
    match runtime_ticks {
        Some(runtime) if runtime > 0 => {
            (position_ticks as f64 / runtime as f64 * 100.0).clamp(0.0, 100.0)
        }
        _ => 0.0,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncItems {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub movies: Vec<TraktMovie>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shows: Vec<TraktShow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub episodes: Vec<TraktEpisode>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SyncResult {
    #[serde(default)]
    pub added: Option<serde_json::Value>,
    #[serde(default)]
    pub deleted: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct SyncHistoryEndpoint {
    pub remove: bool,
    pub items: SyncItems,
}

impl Endpoint for SyncHistoryEndpoint {
    type Output = SyncResult;

    fn path(&self) -> String {
        if self.remove {
            "sync/history/remove".to_string()
        } else {
            "sync/history".to_string()
        }
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(serde_json::to_value(&self.items).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatedMovie {
    pub rating: u8,
    #[serde(flatten)]
    pub movie: TraktMovie,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatedShow {
    pub rating: u8,
    #[serde(flatten)]
    pub show: TraktShow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatedEpisode {
    pub rating: u8,
    #[serde(flatten)]
    pub episode: TraktEpisode,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RatingsItems {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub movies: Vec<RatedMovie>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shows: Vec<RatedShow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub episodes: Vec<RatedEpisode>,
}

#[derive(Debug, Clone)]
pub struct SyncRatingsEndpoint {
    pub remove: bool,
    pub items: RatingsItems,
}

impl Endpoint for SyncRatingsEndpoint {
    type Output = SyncResult;

    fn path(&self) -> String {
        if self.remove {
            "sync/ratings/remove".to_string()
        } else {
            "sync/ratings".to_string()
        }
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(serde_json::to_value(&self.items).unwrap_or_default())
    }
}

#[derive(Debug, Clone)]
pub struct SyncWatchlistEndpoint {
    pub remove: bool,
    pub items: SyncItems,
}

impl Endpoint for SyncWatchlistEndpoint {
    type Output = SyncResult;

    fn path(&self) -> String {
        if self.remove {
            "sync/watchlist/remove".to_string()
        } else {
            "sync/watchlist".to_string()
        }
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(serde_json::to_value(&self.items).unwrap_or_default())
    }
}

#[derive(Debug, Clone)]
pub struct SyncFavoritesEndpoint {
    pub remove: bool,
    pub items: SyncItems,
}

impl Endpoint for SyncFavoritesEndpoint {
    type Output = SyncResult;

    fn path(&self) -> String {
        if self.remove {
            "sync/favorites/remove".to_string()
        } else {
            "sync/favorites".to_string()
        }
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(&self) -> Body {
        Body::Json(serde_json::to_value(&self.items).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LastActivities {
    #[serde(default)]
    pub all: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LastActivitiesEndpoint;

impl Endpoint for LastActivitiesEndpoint {
    type Output = LastActivities;

    fn path(&self) -> String {
        "sync/last_activities".to_string()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PageParams {
    pub page: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchedMovie {
    #[serde(default)]
    pub plays: u32,
    #[serde(default)]
    pub last_watched_at: Option<String>,
    pub movie: TraktMovie,
}

#[derive(Debug, Clone)]
pub struct WatchedMoviesEndpoint {
    pub page: u32,
    pub limit: u32,
}

impl Endpoint for WatchedMoviesEndpoint {
    type Output = Vec<WatchedMovie>;

    fn path(&self) -> String {
        "sync/watched/movies".to_string()
    }

    fn query_params(&self) -> impl serde::Serialize + '_ {
        PageParams {
            page: self.page,
            limit: self.limit,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchedEpisode {
    pub number: i64,
    #[serde(default)]
    pub last_watched_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchedSeason {
    pub number: i64,
    #[serde(default)]
    pub episodes: Vec<WatchedEpisode>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchedShow {
    #[serde(default)]
    pub plays: u32,
    #[serde(default)]
    pub last_watched_at: Option<String>,
    pub show: TraktShow,
    #[serde(default)]
    pub seasons: Vec<WatchedSeason>,
}

#[derive(Debug, Clone)]
pub struct WatchedShowsEndpoint {
    pub page: u32,
    pub limit: u32,
}

impl Endpoint for WatchedShowsEndpoint {
    type Output = Vec<WatchedShow>;

    fn path(&self) -> String {
        "sync/watched/shows".to_string()
    }

    fn query_params(&self) -> impl serde::Serialize + '_ {
        PageParams {
            page: self.page,
            limit: self.limit,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HistoryEpisodeEntry {
    #[serde(default)]
    pub watched_at: Option<String>,
    #[serde(default)]
    pub show: Option<TraktShow>,
    #[serde(default)]
    pub episode: Option<TraktEpisode>,
}

#[derive(Debug, Clone)]
pub struct HistoryEpisodesEndpoint {
    pub page: u32,
    pub limit: u32,
}

impl Endpoint for HistoryEpisodesEndpoint {
    type Output = Vec<HistoryEpisodeEntry>;

    fn path(&self) -> String {
        "sync/history/episodes".to_string()
    }

    fn query_params(&self) -> impl serde::Serialize + '_ {
        PageParams {
            page: self.page,
            limit: self.limit,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchlistEntry {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub movie: Option<TraktMovie>,
    #[serde(default)]
    pub show: Option<TraktShow>,
}

#[derive(Debug, Clone)]
pub struct WatchlistEndpoint {
    pub page: u32,
    pub limit: u32,
}

impl Endpoint for WatchlistEndpoint {
    type Output = Vec<WatchlistEntry>;

    fn path(&self) -> String {
        "sync/watchlist".to_string()
    }

    fn query_params(&self) -> impl serde::Serialize + '_ {
        PageParams {
            page: self.page,
            limit: self.limit,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RatedEntry {
    pub rating: u8,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub movie: Option<TraktMovie>,
    #[serde(default)]
    pub show: Option<TraktShow>,
    #[serde(default)]
    pub episode: Option<TraktEpisode>,
}

#[derive(Debug, Clone)]
pub struct RatingsEndpoint {
    pub page: u32,
    pub limit: u32,
}

impl Endpoint for RatingsEndpoint {
    type Output = Vec<RatedEntry>;

    fn path(&self) -> String {
        "sync/ratings".to_string()
    }

    fn query_params(&self) -> impl serde::Serialize + '_ {
        PageParams {
            page: self.page,
            limit: self.limit,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlaybackEntry {
    pub progress: f64,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub movie: Option<TraktMovie>,
    #[serde(default)]
    pub show: Option<TraktShow>,
    #[serde(default)]
    pub episode: Option<TraktEpisode>,
}

#[derive(Debug, Clone)]
pub struct PlaybackEndpoint;

impl Endpoint for PlaybackEndpoint {
    type Output = Vec<PlaybackEntry>;

    fn path(&self) -> String {
        "sync/playback".to_string()
    }
}

/// Live scrobble: empty 204 deserializes as `None`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchingEntry {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub progress: Option<f64>,
    #[serde(default)]
    pub movie: Option<TraktMovie>,
    #[serde(default)]
    pub show: Option<TraktShow>,
    #[serde(default)]
    pub episode: Option<TraktEpisode>,
}

#[derive(Debug, Clone)]
pub struct WatchingEndpoint;

impl Endpoint for WatchingEndpoint {
    type Output = Option<WatchingEntry>;

    fn path(&self) -> String {
        "users/me/watching".to_string()
    }
}

/// Next episode after the last watched one. Season length is unknown, so a
/// finished season becomes `(season + 1, 1)`.
pub fn next_up_after_watched(show: &WatchedShow) -> Option<(i64, i64)> {
    let season = show
        .seasons
        .iter()
        .max_by_key(|s| s.number)?;
    let last_ep = season
        .episodes
        .iter()
        .map(|e| e.number)
        .max()?;
    Some((season.number, last_ep + 1))
}

pub fn ids_from_parts(
    imdb: Option<String>,
    tmdb: Option<i64>,
    tvdb: Option<i64>,
) -> TraktItemIds {
    TraktItemIds {
        imdb,
        tmdb,
        tvdb,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_percent_of_runtime() {
        assert_eq!(
            playback_progress_percent(5_000_0000, Some(10_000_0000)),
            50.0
        );
        assert_eq!(playback_progress_percent(0, Some(10_000_0000)), 0.0);
        assert_eq!(
            playback_progress_percent(20_000_0000, Some(10_000_0000)),
            100.0
        );
        assert_eq!(playback_progress_percent(1, None), 0.0);
        assert_eq!(playback_progress_percent(1, Some(0)), 0.0);
    }

    #[test]
    fn device_pending_is_not_a_hard_failure() {
        let err = ClientError::Http {
            status: 400,
            message: r#"{"error":"authorization_pending"}"#.into(),
            endpoint: None,
            body: None,
        };
        assert_eq!(
            device_token_status_from_error(&err),
            Some(DeviceTokenStatus::Pending)
        );
    }

    #[test]
    fn device_denied_and_expired_are_terminal() {
        let denied = ClientError::Http {
            status: 400,
            message: r#"{"error":"access_denied"}"#.into(),
            endpoint: None,
            body: None,
        };
        let expired = ClientError::Http {
            status: 400,
            message: r#"{"error":"expired_token"}"#.into(),
            endpoint: None,
            body: None,
        };
        assert_eq!(
            device_token_status_from_error(&denied),
            Some(DeviceTokenStatus::Denied)
        );
        assert_eq!(
            device_token_status_from_error(&expired),
            Some(DeviceTokenStatus::Expired)
        );
    }

    #[test]
    fn scrobble_movie_body_omits_empty_ids() {
        let body = ScrobbleBody {
            movie: Some(TraktMovie {
                title: Some("Heat".into()),
                year: Some(1995),
                ids: ids_from_parts(Some("tt0113277".into()), Some(949), None),
            }),
            show: None,
            episode: None,
            progress: 12.5,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["progress"], 12.5);
        assert_eq!(json["movie"]["ids"]["imdb"], "tt0113277");
        assert_eq!(json["movie"]["ids"]["tmdb"], 949);
        assert!(
            json["movie"]["ids"]
                .get("tvdb")
                .is_none()
        );
        assert!(
            json.get("episode")
                .is_none()
        );
    }

    #[test]
    fn next_up_is_episode_after_last_watched() {
        let show = WatchedShow {
            plays: 3,
            last_watched_at: None,
            show: TraktShow::default(),
            seasons: vec![WatchedSeason {
                number: 1,
                episodes: vec![
                    WatchedEpisode {
                        number: 1,
                        last_watched_at: None,
                    },
                    WatchedEpisode {
                        number: 2,
                        last_watched_at: None,
                    },
                ],
            }],
        };
        assert_eq!(next_up_after_watched(&show), Some((1, 3)));
    }

    #[test]
    fn token_response_round_trips() {
        let json = r#"{
            "access_token": "at",
            "token_type": "bearer",
            "expires_in": 7200,
            "refresh_token": "rt",
            "scope": "public",
            "created_at": 1700000000
        }"#;
        let token: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(token.access_token, "at");
        assert_eq!(token.refresh_token, "rt");
        assert_eq!(token.expires_in, 7200);
    }
}
