//! Official Trakt.tv media tracker: device-code OAuth, scrobble, history,
//! ratings, and watchlist.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

use super::{
    AddonCapabilities, AddonKind, AddonMetadata, AddonOption, AddonOptionType,
    AddonPreset, AddonPresetRegistration, MediaKind, ResourceType,
    media_tracker::{
        AuthFlow, DeviceAuthPoll, DeviceAuthStart, MediaTrackerAddon,
        MediaTrackerCapabilities, MediaTrackerCredentials, MediaTrackerCtx,
        MediaTrackerError, MediaTrackerEvent, MediaTrackerEventKind,
        MediaTrackerResult, MediaTrackerTarget, RemoteWatch, SyncDirection,
    },
};
use crate::{
    db,
    sdks::{
        self, ClientError,
        trakt::{
            self, DeviceCodeEndpoint, DeviceTokenEndpoint, DeviceTokenStatus,
            HistoryEpisodesEndpoint, LastActivitiesEndpoint, PlaybackEndpoint,
            RatedEntry, RatedEpisode, RatedMovie, RatedShow, RatingsEndpoint,
            RatingsItems, RefreshTokenEndpoint, RevokeEndpoint, ScrobbleAction,
            ScrobbleBody, ScrobbleEndpoint, SyncFavoritesEndpoint, SyncHistoryEndpoint,
            SyncItems, SyncRatingsEndpoint, SyncWatchlistEndpoint, TokenResponse,
            TraktEpisode, TraktItemIds, TraktMovie, TraktShow, UserSettingsEndpoint,
            WatchedMoviesEndpoint, WatchedShowsEndpoint, WatchingEndpoint,
            WatchlistEndpoint, WatchlistEntry, playback_progress_percent,
            trakt_user_client,
        },
    },
};

pub const DEFAULT_BASE_URL: &str = trakt::DEFAULT_BASE_URL;

pub struct TraktPreset;

impl AddonPreset for TraktPreset {
    fn id(&self) -> &'static str {
        "trakt"
    }

    fn metadata(&self) -> AddonMetadata {
        AddonMetadata {
            id: "trakt".to_string(),
            display_name: "Trakt".to_string(),
            description: "Scrobble playback to Trakt.tv, import watch history, \
                 and sync ratings and watchlists. Create an API app at \
                 https://trakt.tv/oauth/applications (device code / OOB) and \
                 paste the client id and secret here."
                .to_string(),
            icon: None,
            supported_resources: vec![AddonMetadata::simple_resource(
                ResourceType::Tracking,
            )],
            supported_types: vec![
                MediaKind::Movie,
                MediaKind::Series,
                MediaKind::Episode,
            ],
            supported_resources_user: vec![ResourceType::Tracking],
            supported_types_user: vec![
                MediaKind::Movie,
                MediaKind::Series,
                MediaKind::Episode,
            ],
            options: vec![
                AddonOption {
                    id: "client_id".to_string(),
                    name: "Client ID".to_string(),
                    description: Some(
                        "Trakt API client id from your OAuth application.".to_string(),
                    ),
                    required: true,
                    default: None,
                    kind: AddonOptionType::String,
                },
                AddonOption {
                    id: "client_secret".to_string(),
                    name: "Client Secret".to_string(),
                    description: Some(
                        "Trakt API client secret from your OAuth application."
                            .to_string(),
                    ),
                    required: true,
                    default: None,
                    kind: AddonOptionType::Password,
                },
                AddonOption {
                    id: "base_url".to_string(),
                    name: "API Base URL".to_string(),
                    description: Some(
                        "Override only for tests or a Trakt-compatible proxy."
                            .to_string(),
                    ),
                    required: false,
                    default: Some(serde_json::Value::String(
                        DEFAULT_BASE_URL.to_string(),
                    )),
                    kind: AddonOptionType::Url,
                },
            ],
        }
    }

    fn from_cfg(
        &self,
        _addon_id: Uuid,
        cfg: &serde_json::Value,
        _config: &crate::Config,
    ) -> Result<AddonCapabilities> {
        let client_id = cfg
            .get("client_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("Trakt client_id is required"))?
            .to_string();
        let client_secret = cfg
            .get("client_secret")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("Trakt client_secret is required"))?
            .to_string();
        let base_url = cfg
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_string();
        let addon = Arc::new(TraktAddon {
            client_id,
            client_secret,
            base_url,
        });
        Ok(AddonCapabilities {
            kind: Some(addon.clone()),
            media_tracker: Some(addon),
            ..Default::default()
        })
    }
}

inventory::submit! {
    AddonPresetRegistration(|| Box::new(TraktPreset))
}

pub struct TraktAddon {
    client_id: String,
    client_secret: String,
    base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTokens {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    token_type: String,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    expires_in: u64,
}

impl From<TokenResponse> for StoredTokens {
    fn from(token: TokenResponse) -> Self {
        Self {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            token_type: token.token_type,
            created_at: token.created_at,
            expires_in: token.expires_in,
        }
    }
}

impl StoredTokens {
    fn needs_refresh(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let created = if self.created_at == 0 {
            now
        } else {
            self.created_at
        };
        let ttl = if self.expires_in == 0 {
            7 * 24 * 60 * 60
        } else {
            self.expires_in
        };
        now.saturating_add(3600) >= created.saturating_add(ttl)
    }

    fn from_creds(creds: &MediaTrackerCredentials) -> MediaTrackerResult<Self> {
        serde_json::from_value(
            creds
                .expose()
                .clone(),
        )
        .map_err(|e| {
            MediaTrackerError::reauth(format!("invalid stored Trakt credentials: {e}"))
        })
    }

    fn into_creds(self) -> MediaTrackerResult<MediaTrackerCredentials> {
        serde_json::to_value(self)
            .map(MediaTrackerCredentials::new)
            .map_err(|e| MediaTrackerError::permanent(format!("serialize tokens: {e}")))
    }
}

fn all_events() -> Vec<MediaTrackerEventKind> {
    vec![
        MediaTrackerEventKind::PlaybackStart,
        MediaTrackerEventKind::PlaybackProgress,
        MediaTrackerEventKind::PlaybackStop,
        MediaTrackerEventKind::MarkPlayed,
        MediaTrackerEventKind::MarkUnplayed,
        MediaTrackerEventKind::MarkFavorite,
        MediaTrackerEventKind::UnmarkFavorite,
        MediaTrackerEventKind::Rating,
    ]
}

fn playback_events() -> Vec<MediaTrackerEventKind> {
    vec![
        MediaTrackerEventKind::PlaybackStart,
        MediaTrackerEventKind::PlaybackProgress,
        MediaTrackerEventKind::PlaybackStop,
    ]
}

fn map_error(err: ClientError) -> MediaTrackerError {
    match err {
        ClientError::Unauthorized => {
            MediaTrackerError::reauth("Trakt rejected the token")
        }
        ClientError::RateLimited { retry_after_secs } => {
            MediaTrackerError::retry_after(
                "Trakt rate limited",
                Duration::from_secs(retry_after_secs),
            )
        }
        ClientError::Http { status: 409, .. } => {
            // Already scrobbling / already in history.
            MediaTrackerError::retryable("trakt conflict — treated as success")
        }
        ClientError::Http {
            status: 404,
            message,
            ..
        } => MediaTrackerError::permanent(format!(
            "Trakt could not match item: {message}"
        )),
        other => {
            if other
                .to_string()
                .contains("transport")
                || matches!(other, ClientError::Transport(_))
            {
                MediaTrackerError::retryable(other.user_message())
            } else {
                MediaTrackerError::retryable(other.user_message())
            }
        }
    }
}

fn ignore_conflict(result: Result<(), MediaTrackerError>) -> MediaTrackerResult<()> {
    match result {
        Err(MediaTrackerError::Retryable { message, .. })
            if message.contains("conflict") =>
        {
            Ok(())
        }
        other => other,
    }
}

fn ids_of(target: &MediaTrackerTarget) -> TraktItemIds {
    trakt::ids_from_parts(
        target
            .ids
            .imdb
            .as_ref()
            .map(|s| s.to_string()),
        target
            .ids
            .tmdb,
        target
            .ids
            .tvdb,
    )
}

fn movie_of(target: &MediaTrackerTarget) -> TraktMovie {
    TraktMovie {
        title: Some(
            target
                .title
                .clone(),
        ),
        year: target.year,
        ids: ids_of(target),
    }
}

fn show_of(target: &MediaTrackerTarget) -> TraktShow {
    let src = target
        .series
        .as_deref()
        .unwrap_or(target);
    TraktShow {
        title: Some(
            src.title
                .clone(),
        ),
        year: src.year,
        ids: ids_of(src),
    }
}

fn episode_of(target: &MediaTrackerTarget) -> TraktEpisode {
    TraktEpisode {
        season: target.season,
        number: target.episode,
        ids: ids_of(target),
    }
}

fn scrobble_body(
    target: &MediaTrackerTarget,
    progress: f64,
) -> MediaTrackerResult<ScrobbleBody> {
    match target.kind {
        db::MediaKind::Movie => Ok(ScrobbleBody {
            movie: Some(movie_of(target)),
            show: None,
            episode: None,
            progress,
        }),
        db::MediaKind::Episode => Ok(ScrobbleBody {
            movie: None,
            show: Some(show_of(target)),
            episode: Some(episode_of(target)),
            progress,
        }),
        _ => Err(MediaTrackerError::permanent(format!(
            "Trakt does not scrobble {:?}",
            target.kind
        ))),
    }
}

fn sync_items(target: &MediaTrackerTarget) -> MediaTrackerResult<SyncItems> {
    match target.kind {
        db::MediaKind::Movie => Ok(SyncItems {
            movies: vec![movie_of(target)],
            ..Default::default()
        }),
        db::MediaKind::Episode => Ok(SyncItems {
            episodes: vec![episode_of(target)],
            shows: vec![show_of(target)],
            ..Default::default()
        }),
        db::MediaKind::Series => Ok(SyncItems {
            shows: vec![show_of(target)],
            ..Default::default()
        }),
        _ => Err(MediaTrackerError::permanent(format!(
            "Trakt cannot sync {:?}",
            target.kind
        ))),
    }
}

fn favorite_items(target: &MediaTrackerTarget) -> MediaTrackerResult<SyncItems> {
    match target.kind {
        db::MediaKind::Movie => Ok(SyncItems {
            movies: vec![movie_of(target)],
            ..Default::default()
        }),
        db::MediaKind::Episode | db::MediaKind::Series => Ok(SyncItems {
            shows: vec![show_of(target)],
            ..Default::default()
        }),
        _ => Err(MediaTrackerError::permanent(format!(
            "Trakt cannot favorite {:?}",
            target.kind
        ))),
    }
}

fn ratings_items(
    target: &MediaTrackerTarget,
    rating: u8,
) -> MediaTrackerResult<RatingsItems> {
    match target.kind {
        db::MediaKind::Movie => Ok(RatingsItems {
            movies: vec![RatedMovie {
                rating,
                movie: movie_of(target),
            }],
            ..Default::default()
        }),
        db::MediaKind::Episode => Ok(RatingsItems {
            episodes: vec![RatedEpisode {
                rating,
                episode: episode_of(target),
            }],
            ..Default::default()
        }),
        db::MediaKind::Series => Ok(RatingsItems {
            shows: vec![RatedShow {
                rating,
                show: show_of(target),
            }],
            ..Default::default()
        }),
        _ => Err(MediaTrackerError::permanent(format!(
            "Trakt cannot rate {:?}",
            target.kind
        ))),
    }
}

fn parse_watched_at(raw: Option<&str>) -> Option<NaiveDateTime> {
    let raw = raw?;
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.naive_utc())
        .or_else(|| NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.fZ").ok())
}

fn ids_to_external(ids: &TraktItemIds) -> db::ExternalIds {
    db::ExternalIds {
        imdb: ids
            .imdb
            .as_ref()
            .and_then(|s| db::NonEmptyString::try_new(s.clone()).ok()),
        tmdb: ids.tmdb,
        tvdb: ids.tvdb,
        ..Default::default()
    }
}

fn progress_to_ticks(percent: f64, runtime_ticks: Option<i64>) -> Option<i64> {
    let runtime = runtime_ticks.filter(|r| *r > 0)?;
    Some(((percent / 100.0) * runtime as f64).round() as i64)
}

impl TraktAddon {
    fn client(
        &self,
        access_token: Option<&str>,
    ) -> MediaTrackerResult<sdks::RestClient<trakt::TraktAuth>> {
        trakt_user_client(&self.client_id, access_token, &self.base_url)
            .map_err(|e| MediaTrackerError::permanent(e.to_string()))
    }

    async fn execute<EP: sdks::Endpoint + Clone>(
        &self,
        access_token: Option<&str>,
        endpoint: EP,
    ) -> MediaTrackerResult<EP::Output> {
        self.client(access_token)?
            .execute(endpoint)
            .await
            .map_err(map_error)
    }

    fn progress(
        &self,
        position_ticks: i64,
        target: &MediaTrackerTarget,
        played: bool,
    ) -> f64 {
        if played {
            100.0
        } else {
            playback_progress_percent(position_ticks, target.runtime_ticks)
        }
    }
}

#[async_trait]
impl AddonKind for TraktAddon {
    fn id(&self) -> &'static str {
        "trakt"
    }
}

#[async_trait]
impl MediaTrackerAddon for TraktAddon {
    fn capabilities(&self) -> MediaTrackerCapabilities {
        MediaTrackerCapabilities {
            auth_flow: AuthFlow::OAuthDeviceCode,
            connect_fields: Vec::new(),
            supported_events: all_events(),
            // Import is GET-only. Do not push mark_played/history or a
            // remux apply will rewrite Trakt (2026-09-15 overwrite).
            default_event_filter: playback_events(),
            history_import: true,
            progress_import: true,
            watch_state_sync: SyncDirection::Pull,
            favorites: SyncDirection::Push,
            ratings: SyncDirection::Both,
            watchlist: SyncDirection::Both,
        }
    }

    async fn begin_device_auth(
        &self,
        _ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<DeviceAuthStart> {
        let code = self
            .execute(
                None,
                DeviceCodeEndpoint {
                    client_id: self
                        .client_id
                        .clone(),
                },
            )
            .await?;
        Ok(DeviceAuthStart {
            verification_url: code.verification_url,
            user_code: code.user_code,
            poll_token: code.device_code,
            interval: Duration::from_secs(
                code.interval
                    .max(1),
            ),
            expires_in: Duration::from_secs(
                code.expires_in
                    .max(1),
            ),
        })
    }

    async fn poll_device_auth(
        &self,
        poll_token: &str,
        _ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<DeviceAuthPoll> {
        let endpoint = DeviceTokenEndpoint {
            code: poll_token.to_string(),
            client_id: self
                .client_id
                .clone(),
            client_secret: self
                .client_secret
                .clone(),
        };
        match self
            .client(None)?
            .execute(endpoint)
            .await
        {
            Ok(token) => {
                let creds = StoredTokens::from(token).into_creds()?;
                Ok(DeviceAuthPoll::Approved(creds))
            }
            Err(err) => match trakt::device_token_status_from_error(&err) {
                Some(DeviceTokenStatus::Pending | DeviceTokenStatus::SlowDown) => {
                    Ok(DeviceAuthPoll::Pending)
                }
                Some(DeviceTokenStatus::Denied | DeviceTokenStatus::Expired) => {
                    Ok(DeviceAuthPoll::Denied)
                }
                Some(DeviceTokenStatus::Approved(_)) => unreachable!(),
                None => {
                    if matches!(err, ClientError::Http { status: 400, .. }) {
                        Ok(DeviceAuthPoll::Pending)
                    } else {
                        Err(map_error(err))
                    }
                }
            },
        }
    }

    async fn refresh(
        &self,
        creds: &MediaTrackerCredentials,
        _ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<MediaTrackerCredentials> {
        let stored = StoredTokens::from_creds(creds)?;
        let token = self
            .execute(
                None,
                RefreshTokenEndpoint {
                    refresh_token: stored.refresh_token,
                    client_id: self
                        .client_id
                        .clone(),
                    client_secret: self
                        .client_secret
                        .clone(),
                },
            )
            .await?;
        StoredTokens::from(token).into_creds()
    }

    async fn verify(
        &self,
        creds: &MediaTrackerCredentials,
        _ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<()> {
        let stored = StoredTokens::from_creds(creds)?;
        self.execute(Some(&stored.access_token), UserSettingsEndpoint)
            .await?;
        Ok(())
    }

    async fn disconnect(
        &self,
        creds: &MediaTrackerCredentials,
        _ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<()> {
        let stored = StoredTokens::from_creds(creds)?;
        match self
            .execute(
                Some(&stored.access_token),
                RevokeEndpoint {
                    token: stored
                        .access_token
                        .clone(),
                    client_id: self
                        .client_id
                        .clone(),
                    client_secret: self
                        .client_secret
                        .clone(),
                },
            )
            .await
        {
            Ok(())
            | Err(MediaTrackerError::Permanent {
                reauth_required: true,
                ..
            }) => Ok(()),
            other => other,
        }
    }

    async fn on_event(
        &self,
        event: &MediaTrackerEvent,
        target: &MediaTrackerTarget,
        creds: &MediaTrackerCredentials,
        _ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<()> {
        if !target.is_matchable() {
            return Ok(());
        }
        let stored = StoredTokens::from_creds(creds)?;
        let token = Some(
            stored
                .access_token
                .as_str(),
        );
        match event {
            MediaTrackerEvent::PlaybackStart { position_ticks } => {
                let progress = self.progress(*position_ticks, target, false);
                ignore_conflict(
                    self.execute(
                        token,
                        ScrobbleEndpoint {
                            action: ScrobbleAction::Start,
                            body: scrobble_body(target, progress)?,
                        },
                    )
                    .await
                    .map(|_| ()),
                )
            }
            MediaTrackerEvent::PlaybackProgress {
                position_ticks,
                is_paused,
            } => {
                let progress = self.progress(*position_ticks, target, false);
                let action = if *is_paused {
                    ScrobbleAction::Pause
                } else {
                    ScrobbleAction::Start
                };
                ignore_conflict(
                    self.execute(
                        token,
                        ScrobbleEndpoint {
                            action,
                            body: scrobble_body(target, progress)?,
                        },
                    )
                    .await
                    .map(|_| ()),
                )
            }
            MediaTrackerEvent::PlaybackStop {
                position_ticks,
                played,
            } => {
                let progress = self.progress(*position_ticks, target, *played);
                ignore_conflict(
                    self.execute(
                        token,
                        ScrobbleEndpoint {
                            action: ScrobbleAction::Stop,
                            body: scrobble_body(target, progress)?,
                        },
                    )
                    .await
                    .map(|_| ()),
                )
            }
            MediaTrackerEvent::MarkPlayed => ignore_conflict(
                self.execute(
                    token,
                    SyncHistoryEndpoint {
                        remove: false,
                        items: sync_items(target)?,
                    },
                )
                .await
                .map(|_| ()),
            ),
            MediaTrackerEvent::MarkUnplayed => ignore_conflict(
                self.execute(
                    token,
                    SyncHistoryEndpoint {
                        remove: true,
                        items: sync_items(target)?,
                    },
                )
                .await
                .map(|_| ()),
            ),
            MediaTrackerEvent::MarkFavorite => ignore_conflict(
                self.execute(
                    token,
                    SyncFavoritesEndpoint {
                        remove: false,
                        items: favorite_items(target)?,
                    },
                )
                .await
                .map(|_| ()),
            ),
            MediaTrackerEvent::UnmarkFavorite => ignore_conflict(
                self.execute(
                    token,
                    SyncFavoritesEndpoint {
                        remove: true,
                        items: favorite_items(target)?,
                    },
                )
                .await
                .map(|_| ()),
            ),
            MediaTrackerEvent::Rating { rating: None } => ignore_conflict(
                self.execute(
                    token,
                    SyncRatingsEndpoint {
                        remove: true,
                        items: ratings_items(target, 1)?,
                    },
                )
                .await
                .map(|_| ()),
            ),
            MediaTrackerEvent::Rating {
                rating: Some(rating),
            } => {
                let scaled = rating
                    .clamp(1.0, 10.0)
                    .round() as u8;
                ignore_conflict(
                    self.execute(
                        token,
                        SyncRatingsEndpoint {
                            remove: false,
                            items: ratings_items(target, scaled)?,
                        },
                    )
                    .await
                    .map(|_| ()),
                )
            }
        }
    }

    async fn import_history(
        &self,
        creds: &MediaTrackerCredentials,
        ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<Vec<RemoteWatch>> {
        self.pull_changes(None, creds, ctx)
            .await
    }

    async fn remote_activity_at(
        &self,
        creds: &MediaTrackerCredentials,
        _ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<Option<NaiveDateTime>> {
        let stored = StoredTokens::from_creds(creds)?;
        let acts = self
            .execute(Some(&stored.access_token), LastActivitiesEndpoint)
            .await?;
        Ok(parse_watched_at(
            acts.all
                .as_deref(),
        ))
    }

    async fn pull_changes(
        &self,
        _since: Option<NaiveDateTime>,
        creds: &MediaTrackerCredentials,
        _ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<Vec<RemoteWatch>> {
        let stored = StoredTokens::from_creds(creds)?;
        let token = Some(
            stored
                .access_token
                .as_str(),
        );
        let mut out = Vec::new();

        let movies = self
            .execute(
                token,
                WatchedMoviesEndpoint {
                    page: 1,
                    limit: 100,
                },
            )
            .await?;
        for item in movies {
            out.push(RemoteWatch {
                ids: ids_to_external(
                    &item
                        .movie
                        .ids,
                ),
                season: None,
                episode: None,
                watched: item.plays > 0,
                position_ticks: None,
                progress_percent: None,
                watched_at: parse_watched_at(
                    item.last_watched_at
                        .as_deref(),
                ),
                favorite: None,
                rating: None,
            });
        }

        let shows = self
            .execute(
                token,
                WatchedShowsEndpoint {
                    page: 1,
                    limit: 100,
                },
            )
            .await?;
        for show in shows {
            for season in show.seasons {
                for ep in season.episodes {
                    out.push(RemoteWatch {
                        ids: ids_to_external(
                            &show
                                .show
                                .ids,
                        ),
                        season: Some(season.number),
                        episode: Some(ep.number),
                        watched: true,
                        position_ticks: None,
                        progress_percent: None,
                        watched_at: parse_watched_at(
                            ep.last_watched_at
                                .as_deref(),
                        ),
                        favorite: None,
                        rating: None,
                    });
                }
            }
        }
        // Current Trakt /sync/watched/shows omits season/episode lists.
        // History is what actually names the last watched episode.
        for page in 1..=20 {
            let history = self
                .execute(token, HistoryEpisodesEndpoint { page, limit: 100 })
                .await?;
            if history.is_empty() {
                break;
            }
            let n = history.len();
            for item in history {
                let Some(show) = item.show else { continue };
                let Some(episode) = item.episode else {
                    continue;
                };
                let (Some(season), Some(number)) = (episode.season, episode.number)
                else {
                    continue;
                };
                out.push(RemoteWatch {
                    ids: ids_to_external(&show.ids),
                    season: Some(season),
                    episode: Some(number),
                    watched: true,
                    position_ticks: None,
                    progress_percent: None,
                    watched_at: parse_watched_at(
                        item.watched_at
                            .as_deref(),
                    ),
                    favorite: None,
                    rating: None,
                });
            }
            if n < 100 {
                break;
            }
        }

        let ratings: Vec<RatedEntry> = self
            .execute(
                token,
                RatingsEndpoint {
                    page: 1,
                    limit: 100,
                },
            )
            .await?;
        for rated in ratings {
            let (ids, season, episode) = match rated
                .kind
                .as_str()
            {
                "movie" => {
                    let Some(movie) = rated.movie else { continue };
                    (ids_to_external(&movie.ids), None, None)
                }
                "show" => {
                    let Some(show) = rated.show else { continue };
                    (ids_to_external(&show.ids), None, None)
                }
                "episode" => {
                    let Some(episode) = rated.episode else {
                        continue;
                    };
                    (
                        ids_to_external(&episode.ids),
                        episode.season,
                        episode.number,
                    )
                }
                _ => continue,
            };
            out.push(RemoteWatch {
                ids,
                season,
                episode,
                watched: false,
                position_ticks: None,
                progress_percent: None,
                watched_at: None,
                favorite: None,
                rating: Some(rated.rating as f32),
            });
        }

        let playback = self
            .execute(token, PlaybackEndpoint)
            .await?;
        for entry in playback {
            let (ids, season, episode) = match entry
                .kind
                .as_str()
            {
                "movie" => {
                    let Some(movie) = entry.movie else { continue };
                    (ids_to_external(&movie.ids), None, None)
                }
                "episode" => {
                    let episode = entry
                        .episode
                        .unwrap_or_default();
                    let ids = entry
                        .show
                        .as_ref()
                        .map(|s| ids_to_external(&s.ids))
                        .unwrap_or_else(|| ids_to_external(&episode.ids));
                    (ids, episode.season, episode.number)
                }
                _ => continue,
            };
            out.push(RemoteWatch {
                ids,
                season,
                episode,
                watched: false,
                position_ticks: None,
                progress_percent: Some(entry.progress),
                watched_at: None,
                favorite: None,
                rating: None,
            });
        }

        if let Ok(Some(watching)) = self
            .execute(token, WatchingEndpoint)
            .await
        {
            let (ids, season, episode) = match watching
                .kind
                .as_str()
            {
                "movie" => watching
                    .movie
                    .map(|movie| (ids_to_external(&movie.ids), None, None))
                    .unwrap_or_else(|| (db::ExternalIds::default(), None, None)),
                "episode" => {
                    let episode = watching
                        .episode
                        .unwrap_or_default();
                    let ids = watching
                        .show
                        .as_ref()
                        .map(|s| ids_to_external(&s.ids))
                        .unwrap_or_else(|| ids_to_external(&episode.ids));
                    (ids, episode.season, episode.number)
                }
                _ => (db::ExternalIds::default(), None, None),
            };
            if ids
                .imdb
                .is_some()
                || ids
                    .tmdb
                    .is_some()
                || ids
                    .tvdb
                    .is_some()
            {
                out.push(RemoteWatch {
                    ids,
                    season,
                    episode,
                    watched: false,
                    position_ticks: None,
                    progress_percent: watching
                        .progress
                        .or(Some(1.0)),
                    watched_at: None,
                    favorite: None,
                    rating: None,
                });
            }
        }

        Ok(out)
    }

    async fn pull_watchlist(
        &self,
        creds: &MediaTrackerCredentials,
        _ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<Vec<db::ExternalIds>> {
        let stored = StoredTokens::from_creds(creds)?;
        let entries: Vec<WatchlistEntry> = self
            .execute(
                Some(&stored.access_token),
                WatchlistEndpoint {
                    page: 1,
                    limit: 100,
                },
            )
            .await?;
        Ok(entries
            .into_iter()
            .filter_map(|entry| {
                entry
                    .movie
                    .map(|m| ids_to_external(&m.ids))
                    .or_else(|| {
                        entry
                            .show
                            .map(|s| ids_to_external(&s.ids))
                    })
            })
            .collect())
    }

    async fn push_watchlist(
        &self,
        target: &MediaTrackerTarget,
        add: bool,
        creds: &MediaTrackerCredentials,
        _ctx: &MediaTrackerCtx,
    ) -> MediaTrackerResult<()> {
        let stored = StoredTokens::from_creds(creds)?;
        ignore_conflict(
            self.execute(
                Some(&stored.access_token),
                SyncWatchlistEndpoint {
                    remove: !add,
                    items: sync_items(target)?,
                },
            )
            .await
            .map(|_| ()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addons::AddonPreset;

    fn cfg() -> serde_json::Value {
        serde_json::json!({
            "client_id": "id",
            "client_secret": "secret",
            "base_url": "https://example.test"
        })
    }

    #[test]
    fn preset_requires_client_credentials() {
        let err = TraktPreset.from_cfg(
            Uuid::nil(),
            &serde_json::json!({}),
            &crate::Config {
                ..Default::default()
            },
        );
        assert!(err.is_err());
    }

    #[test]
    fn preset_wires_media_tracker_capability() {
        let caps = TraktPreset
            .from_cfg(
                Uuid::nil(),
                &cfg(),
                &crate::Config {
                    ..Default::default()
                },
            )
            .unwrap();
        let tracker = caps
            .media_tracker
            .expect("trakt must expose media_tracker");
        let declared = tracker.capabilities();
        assert_eq!(declared.auth_flow, AuthFlow::OAuthDeviceCode);
        assert!(declared.history_import);
        assert!(declared.supports(MediaTrackerEventKind::PlaybackStop));
        assert_eq!(declared.watchlist, SyncDirection::Both);
        assert_eq!(declared.watch_state_sync, SyncDirection::Pull);
        assert!(
            !declared
                .default_event_filter
                .contains(&MediaTrackerEventKind::MarkPlayed)
        );
    }

    #[test]
    fn scrobble_body_for_episode_uses_series_ids() {
        let series = MediaTrackerTarget {
            kind: db::MediaKind::Series,
            title: "The Wire".into(),
            year: Some(2002),
            ids: db::ExternalIds {
                imdb: db::NonEmptyString::try_new("tt0306414").ok(),
                tmdb: Some(1438),
                ..Default::default()
            },
            series: None,
            season: None,
            episode: None,
            runtime_ticks: None,
        };
        let episode = MediaTrackerTarget {
            kind: db::MediaKind::Episode,
            title: "The Target".into(),
            year: None,
            ids: db::ExternalIds::default(),
            series: Some(Box::new(series)),
            season: Some(1),
            episode: Some(1),
            runtime_ticks: Some(10_000_0000),
        };
        let body = scrobble_body(&episode, 10.0).unwrap();
        assert_eq!(
            body.show
                .unwrap()
                .ids
                .imdb
                .as_deref(),
            Some("tt0306414")
        );
        assert_eq!(
            body.episode
                .unwrap()
                .season,
            Some(1)
        );
        assert!(
            body.movie
                .is_none()
        );
    }

    #[test]
    fn stored_tokens_round_trip_through_secret() {
        let token = TokenResponse {
            access_token: "at".into(),
            token_type: "bearer".into(),
            expires_in: 60,
            refresh_token: "rt".into(),
            scope: "public".into(),
            created_at: 1,
        };
        let creds = StoredTokens::from(token)
            .into_creds()
            .unwrap();
        let stored = StoredTokens::from_creds(&creds).unwrap();
        assert_eq!(stored.access_token, "at");
        assert_eq!(stored.refresh_token, "rt");
    }
}
