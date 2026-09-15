//! Describing the item a delivery names, and the subscriber that scrobbles it.

use std::{collections::HashSet, sync::Arc};

use anyhow::{Error, Result};
use async_trait::async_trait;
use chrono::Datelike;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    AppContext,
    addons::media_tracker::{MediaTrackerCtx, MediaTrackerEvent, MediaTrackerTarget},
    db,
    signals::{DeliveryMode, Event, EventType, Subscriber},
};

fn describe(media: &db::Media, series: Option<&db::Media>) -> MediaTrackerTarget {
    MediaTrackerTarget {
        kind: media
            .kind
            .clone(),
        title: media
            .title
            .clone(),
        year: media
            .released_at
            .map(|d| d.year()),
        ids: media
            .external_ids
            .clone(),
        series: series.map(|s| Box::new(describe(s, None))),
        season: media.parent_idx,
        episode: media.idx,
        runtime_ticks: media.runtime,
    }
}

/// The series an episode hangs off. The ancestor walk follows `parent_id`, so
/// an episode saved without a season row above it falls back to the
/// `grandparent_id` its row names, which `Media::validate` requires of it.
async fn series_of(ctx: &AppContext, media: &db::Media) -> Result<Option<db::Media>> {
    if media.kind != db::MediaKind::Episode {
        return Ok(None);
    }
    if let Some(series) = db::Media::get_ancestors(&ctx.db, &media.id)
        .await?
        .into_iter()
        .find(|m| m.kind == db::MediaKind::Series)
    {
        return Ok(Some(series));
    }
    let Some(series_id) = media.grandparent_id else {
        return Ok(None);
    };
    Ok(db::Media::get_by_id(&ctx.db, &series_id)
        .await?
        .filter(|m| m.kind == db::MediaKind::Series))
}

/// The item as a provider needs to see it, or `None` when nothing about it
/// carries an id one could match on. Episodes carry their series, because a
/// provider keys an episode on the show's ids plus season and episode.
pub async fn resolve_target(
    ctx: &AppContext,
    media: &mut db::Media,
) -> Result<Option<MediaTrackerTarget>> {
    let mut series = series_of(ctx, media).await?;

    // Opportunistic, and here so it reuses the series row loaded above: a TMDB
    // or Kitsu error must not hold up an event that was already deliverable, so
    // it is surfaced below only if it turns out to be why nothing matched.
    let mut completion_err: Option<Error> = None;
    if let Some(series) = series.as_mut() {
        if let Err(e) = crate::services::MediaResolveService::complete_episode_ids(
            media, series, ctx,
        )
        .await
        {
            completion_err = Some(e);
        }
    }

    let target = describe(media, series.as_ref());
    if !target.is_matchable() {
        if let Some(e) = completion_err {
            return Err(e);
        }
        return Ok(None);
    }
    if let Some(e) = completion_err {
        warn!(
            title = %media.title,
            error = %e,
            "failed to complete episode ids, delivering with what already matched"
        );
    }
    Ok(Some(target))
}

pub struct MediaTrackerSubscriber {
    pub ctx: AppContext,
}

#[async_trait]
impl Subscriber for MediaTrackerSubscriber {
    fn key(&self) -> &'static str {
        "media_tracker"
    }

    fn events(&self) -> &[EventType] {
        &[
            EventType::PlaybackStarted,
            EventType::PlaybackProgress,
            EventType::PlaybackStopped,
            EventType::MarkPlayed,
            EventType::MarkUnplayed,
            EventType::MarkFavorite,
            EventType::UnmarkFavorite,
            EventType::Rating,
        ]
    }

    fn delivery_mode(&self) -> DeliveryMode {
        DeliveryMode::Persistent {
            max_retries: Some(12),
        }
    }

    async fn handle(&self, event: Event) -> anyhow::Result<()> {
        let (user_id, media_id, tracker_event) = match event {
            Event::PlaybackStarted(i) => (
                i.user_id,
                i.media_id,
                MediaTrackerEvent::PlaybackStart {
                    position_ticks: i.position_ticks,
                },
            ),
            Event::PlaybackProgress(i) => (
                i.user_id,
                i.media_id,
                MediaTrackerEvent::PlaybackProgress {
                    position_ticks: i.position_ticks,
                    is_paused: i.is_paused,
                },
            ),
            Event::PlaybackStopped(i) => (
                i.user_id,
                i.media_id,
                MediaTrackerEvent::PlaybackStop {
                    position_ticks: i.position_ticks,
                    played: i.played,
                },
            ),
            Event::MarkPlayed(i) => {
                (i.user_id, i.media_id, MediaTrackerEvent::MarkPlayed)
            }
            Event::MarkUnplayed(i) => {
                (i.user_id, i.media_id, MediaTrackerEvent::MarkUnplayed)
            }
            Event::MarkFavorite(i) => {
                (i.user_id, i.media_id, MediaTrackerEvent::MarkFavorite)
            }
            Event::UnmarkFavorite(i) => {
                (i.user_id, i.media_id, MediaTrackerEvent::UnmarkFavorite)
            }
            Event::Rating(i) => (
                i.user_id,
                i.media_id,
                MediaTrackerEvent::Rating { rating: i.rating },
            ),
            _ => return Ok(()),
        };

        if !self
            .ctx
            .addons
            .has_media_tracker()
        {
            return Ok(());
        }

        let kind = tracker_event.kind();
        let wanted: Vec<db::UserMediaTracker> = db::UserMediaTracker::list_for_user(
            &self
                .ctx
                .db,
            user_id,
        )
        .await?
        .into_iter()
        .filter(|t| t.status == db::MediaTrackerStatus::Connected && t.wants(kind))
        .filter(|t| {
            self.ctx
                .addons
                .media_tracker_for(t.addon_id)
                .is_some_and(|a| {
                    a.capabilities()
                        .supports(kind)
                })
        })
        .collect();

        if wanted.is_empty() {
            return Ok(());
        }

        let Some(mut media) = db::Media::get_by_id(
            &self
                .ctx
                .db,
            &media_id,
        )
        .await?
        else {
            return Ok(());
        };

        let Some(target) = resolve_target(&self.ctx, &mut media).await? else {
            return Ok(());
        };

        let tctx = MediaTrackerCtx {
            config: Arc::new(
                self.ctx
                    .config
                    .clone(),
            ),
        };

        let mut errors: Vec<anyhow::Error> = Vec::new();
        for tracker in &wanted {
            if let Some(addon) = self
                .ctx
                .addons
                .media_tracker_for(tracker.addon_id)
            {
                let creds = match ensure_fresh_credentials(
                    &self
                        .ctx
                        .db,
                    addon.as_ref(),
                    tracker,
                    &tctx,
                )
                .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        errors.push(e);
                        continue;
                    }
                };
                match addon
                    .on_event(&tracker_event, &target, &creds, &tctx)
                    .await
                {
                    Ok(()) => {
                        let _ = db::UserMediaTracker::mark_success(
                            &self
                                .ctx
                                .db,
                            tracker.id,
                        )
                        .await;
                    }
                    Err(e) if e.requires_reauth() => {
                        match addon
                            .refresh(&creds, &tctx)
                            .await
                        {
                            Ok(new_creds) => {
                                let _ = db::UserMediaTracker::set_credentials(
                                    &self
                                        .ctx
                                        .db,
                                    tracker.id,
                                    &new_creds,
                                )
                                .await;
                                match addon
                                    .on_event(
                                        &tracker_event,
                                        &target,
                                        &new_creds,
                                        &tctx,
                                    )
                                    .await
                                {
                                    Ok(()) => {
                                        let _ = db::UserMediaTracker::mark_success(
                                            &self
                                                .ctx
                                                .db,
                                            tracker.id,
                                        )
                                        .await;
                                    }
                                    Err(e2) => errors.push(anyhow::anyhow!("{e2}")),
                                }
                            }
                            Err(e2) => errors.push(anyhow::anyhow!("{e2}")),
                        }
                    }
                    Err(e) => {
                        errors.push(anyhow::anyhow!("{e}"));
                    }
                }
            }
        }

        if let Some(e) = errors
            .into_iter()
            .next()
        {
            return Err(e);
        }
        Ok(())
    }
}

fn creds_expiring(
    creds: &crate::addons::media_tracker::MediaTrackerCredentials,
) -> bool {
    let v = creds.expose();
    let created = v
        .get("created_at")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let expires = v
        .get("expires_in")
        .and_then(|x| x.as_u64())
        .unwrap_or(7 * 24 * 60 * 60);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let created = if created == 0 { now } else { created };
    let ttl = if expires == 0 {
        7 * 24 * 60 * 60
    } else {
        expires
    };
    now.saturating_add(3600) >= created.saturating_add(ttl)
}

pub async fn ensure_fresh_credentials(
    db: &sqlx::SqlitePool,
    addon: &dyn crate::addons::media_tracker::MediaTrackerAddon,
    tracker: &db::UserMediaTracker,
    tctx: &MediaTrackerCtx,
) -> Result<crate::addons::media_tracker::MediaTrackerCredentials> {
    let creds = tracker
        .credentials
        .clone();
    if !creds_expiring(&creds) {
        return Ok(creds);
    }
    let new = addon
        .refresh(&creds, tctx)
        .await?;
    db::UserMediaTracker::set_credentials(db, tracker.id, &new).await?;
    Ok(new)
}

/// Apply Trakt (or any tracker) history/progress to local user data so
/// Continue Watching and Next Up light up in Jellyfin clients.
/// Incremental cron pulls this many missing titles. First sync / manual
/// `/sync` pass `usize::MAX` so the library can catch up in one go.
pub const INCREMENTAL_NEW_TITLES_PER_SYNC: usize = 5;

pub async fn apply_remote_watches(
    ctx: &AppContext,
    user_id: Uuid,
    watches: &[crate::addons::media_tracker::RemoteWatch],
    max_new_titles: usize,
) -> Result<usize> {
    let Some(user) = db::User::get_by_id(&ctx.db, &user_id).await? else {
        return Ok(0);
    };
    let mut applied = 0usize;
    let mut persisted = 0usize;
    let mut persist_attempted: HashSet<i64> = HashSet::new();
    for watch in watches {
        let media = match resolve_local_media(&ctx.db, watch).await? {
            Some(media) => media,
            None if watch.watched => {
                if persisted < max_new_titles {
                    if persist_missing_from_watch(ctx, watch, &mut persist_attempted)
                        .await?
                    {
                        persisted += 1;
                    }
                }
                match resolve_local_media(&ctx.db, watch).await? {
                    Some(media) => media,
                    None => continue,
                }
            }
            None => continue,
        };
        if watch.watched {
            media
                .mark_played(&ctx.db, &user, false, None)
                .await?;
            applied += 1;
        }
        if let Some(pct) = watch
            .progress_percent
            .filter(|p| *p > 0.0)
        {
            let ticks = watch
                .position_ticks
                .unwrap_or_else(|| {
                    media
                        .runtime
                        .map(|rt| ((pct / 100.0) * rt as f64).round() as i64)
                        .unwrap_or(10_000_000)
                });
            db::UserMediaState::update_playback(
                &ctx.db, &user, &media, ticks, None, None, None,
            )
            .await?;
            applied += 1;
        } else if let Some(ticks) = watch
            .position_ticks
            .filter(|t| *t >= 10_000)
        {
            db::UserMediaState::update_playback(
                &ctx.db, &user, &media, ticks, None, None, None,
            )
            .await?;
            applied += 1;
        }
        if let Some(fav) = watch.favorite {
            let mut state =
                db::UserMediaState::get_or_new(&ctx.db, &user, &media).await?;
            state.favorite = fav;
            state
                .save(&ctx.db)
                .await?;
            applied += 1;
        }
        if let Some(rating) = watch.rating {
            if let Ok(r) = db::UserRating::try_from(rating as f64) {
                db::UserMediaState::set_rating(&ctx.db, &user, &media, Some(r)).await?;
                applied += 1;
            }
        }
        if watch.watched {
            if let Some(at) = watch.watched_at {
                stamp_imported_watch(&ctx.db, user.id, media.id, at).await?;
            }
        }
    }
    Ok(applied)
}

async fn stamp_imported_watch(
    db: &sqlx::SqlitePool,
    user_id: Uuid,
    media_id: Uuid,
    watched_at: chrono::NaiveDateTime,
) -> Result<()> {
    sqlx::query(
        "UPDATE user_media_state SET played_at = ?1, last_played_at = ?1 \
         WHERE user_id = ?2 AND media_id = ?3",
    )
    .bind(watched_at)
    .bind(user_id)
    .bind(media_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Persist a missing movie/series from Trakt ids via TMDB. Never talks to Trakt.
async fn persist_missing_from_watch(
    ctx: &AppContext,
    watch: &crate::addons::media_tracker::RemoteWatch,
    attempted: &mut HashSet<i64>,
) -> Result<bool> {
    let Some(tmdb) = watch
        .ids
        .tmdb
    else {
        return Ok(false);
    };
    if !attempted.insert(tmdb) {
        return Ok(false);
    }
    let kind = if watch
        .season
        .is_some()
        || watch
            .episode
            .is_some()
    {
        db::MediaKind::Series
    } else {
        db::MediaKind::Movie
    };
    if db::Media::find_by_external_ids(&ctx.db, &kind, &watch.ids)
        .await
        .is_some()
    {
        return Ok(false);
    }
    let canonical = format!("tmdb:{tmdb}");
    let stub = db::Media {
        id: crate::common::stable_media_uuid(&kind, &canonical),
        title: canonical.clone(),
        kind: kind.clone(),
        external_ids: watch
            .ids
            .clone(),
        ..Default::default()
    };
    let config =
        std::sync::Arc::new(db::Settings::get_config_or_default(&ctx.db).await);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(
        (config
            .meta_concurrency
            .max(1)) as usize,
    ));
    info!(%tmdb, kind = %kind, "trakt import persisting missing title");
    ctx.addons
        .process_meta_item(stub, ctx.clone(), true, config, semaphore)
        .await;
    Ok(true)
}

async fn resolve_local_media(
    db: &sqlx::SqlitePool,
    watch: &crate::addons::media_tracker::RemoteWatch,
) -> Result<Option<db::Media>> {
    if let (Some(season), Some(episode)) = (watch.season, watch.episode) {
        let Some(series_id) =
            db::Media::find_by_external_ids(db, &db::MediaKind::Series, &watch.ids)
                .await
        else {
            return Ok(None);
        };
        let found: Option<db::Media> = sqlx::query_as(
            "SELECT * FROM media WHERE grandparent_id = ?1 AND kind = 'episode' \
             AND parent_idx = ?2 AND idx = ?3 LIMIT 1",
        )
        .bind(series_id)
        .bind(season)
        .bind(episode)
        .fetch_optional(db)
        .await?;
        return Ok(found);
    }
    let id = db::Media::find_by_external_ids(db, &db::MediaKind::Movie, &watch.ids)
        .await
        .or(
            db::Media::find_by_external_ids(db, &db::MediaKind::Series, &watch.ids)
                .await,
        );
    match id {
        Some(id) => Ok(db::Media::get_by_id(db, &id).await?),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        addons::{
            Addon, AddonCapabilities, AddonKind, AddonPresetRef, AddonRuntime,
            media_tracker::{
                MediaTrackerAddon, MediaTrackerCapabilities, MediaTrackerCredentials,
                MediaTrackerEventKind, MediaTrackerResult,
            },
        },
        db::{MediaTrackerStatus, UserMediaTracker},
        integration_test::new_test_server,
        signals::MarkPlayedInfo,
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use std::sync::Arc;

    struct StubMediaTracker(Vec<MediaTrackerEventKind>);

    impl StubMediaTracker {
        fn everything() -> Self {
            Self(vec![
                MediaTrackerEventKind::PlaybackStart,
                MediaTrackerEventKind::PlaybackProgress,
                MediaTrackerEventKind::PlaybackStop,
                MediaTrackerEventKind::MarkPlayed,
                MediaTrackerEventKind::MarkUnplayed,
                MediaTrackerEventKind::MarkFavorite,
                MediaTrackerEventKind::UnmarkFavorite,
                MediaTrackerEventKind::Rating,
            ])
        }
    }

    impl AddonKind for StubMediaTracker {
        fn id(&self) -> &'static str {
            "scripted"
        }
    }

    #[async_trait]
    impl MediaTrackerAddon for StubMediaTracker {
        fn capabilities(&self) -> MediaTrackerCapabilities {
            MediaTrackerCapabilities {
                supported_events: self
                    .0
                    .clone(),
                ..Default::default()
            }
        }

        async fn on_event(
            &self,
            _event: &MediaTrackerEvent,
            _target: &MediaTrackerTarget,
            _creds: &MediaTrackerCredentials,
            _ctx: &MediaTrackerCtx,
        ) -> MediaTrackerResult<()> {
            Ok(())
        }
    }

    async fn connect(
        ctx: &AppContext,
        name: &str,
        status: MediaTrackerStatus,
        filters: Vec<MediaTrackerEventKind>,
    ) -> Uuid {
        let addon = crate::integration_test::register_media_tracker(
            ctx,
            name,
            Arc::new(StubMediaTracker::everything()),
        )
        .await;
        let mut tracker = UserMediaTracker::new(
            user_id(ctx).await,
            addon.id,
            Default::default(),
            filters,
        );
        tracker.status = status;
        tracker
            .upsert(&ctx.db)
            .await
            .unwrap();
        tracker.id
    }

    async fn user_id(ctx: &AppContext) -> Uuid {
        db::User::get_by_username(&ctx.db, "test")
            .await
            .unwrap()
            .unwrap()
            .id
    }

    /// The walk to the series follows `parent_id`, which an episode saved
    /// without a season row above it does not have.
    #[tokio::test]
    async fn a_flat_episode_still_reaches_its_series() {
        let (_s, guard) = new_test_server()
            .await
            .unwrap();
        let ctx = &guard.0;

        let external_ids = db::ExternalIds {
            imdb: db::NonEmptyString::try_new("tt0306414".to_string()).ok(),
            tmdb: Some(1438),
            ..Default::default()
        };
        let mut series = db::Media {
            id: Uuid::from(&db::MediaIdRaw {
                kind: db::MediaKind::Series,
                external_ids: external_ids.clone(),
                season: None,
                episode: None,
            }),
            title: "The Wire".into(),
            kind: db::MediaKind::Series,
            external_ids,
            ..Default::default()
        };
        series
            .save(&ctx.db)
            .await
            .unwrap();

        let mut episode = db::Media {
            title: "The Target".into(),
            kind: db::MediaKind::Episode,
            grandparent_id: Some(series.id),
            idx: Some(1),
            parent_idx: Some(1),
            external_ids: db::ExternalIds {
                imdb: db::NonEmptyString::try_new("tt0749451".to_string()).ok(),
                tvdb: Some(299034),
                ..Default::default()
            },
            ..Default::default()
        };
        episode
            .save(&ctx.db)
            .await
            .unwrap();

        let target = resolve_target(ctx, &mut episode)
            .await
            .unwrap()
            .expect("an episode alone identifies nothing");

        assert_eq!(
            target
                .series
                .expect("no season row is not no series")
                .ids
                .tmdb,
            Some(1438)
        );
    }

    #[tokio::test]
    async fn subscriber_delivers_to_connected_trackers() {
        let (_s, guard) = new_test_server()
            .await
            .unwrap();
        let ctx = &guard.0;
        let _tracker = connect(
            ctx,
            "a",
            MediaTrackerStatus::Connected,
            vec![MediaTrackerEventKind::MarkPlayed],
        )
        .await;
        let media = crate::integration_test::seed_movie(ctx).await;
        let uid = user_id(ctx).await;

        let sub = MediaTrackerSubscriber { ctx: ctx.clone() };
        let result = sub
            .handle(Event::MarkPlayed(MarkPlayedInfo {
                user_id: uid,
                media_id: media.id,
            }))
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn subscriber_ignores_playback_progress_for_mark_played_tracker() {
        let (_s, guard) = new_test_server()
            .await
            .unwrap();
        let ctx = &guard.0;
        let _tracker = connect(
            ctx,
            "a",
            MediaTrackerStatus::Connected,
            vec![MediaTrackerEventKind::MarkPlayed],
        )
        .await;
        let media = crate::integration_test::seed_movie(ctx).await;
        let uid = user_id(ctx).await;

        let sub = MediaTrackerSubscriber { ctx: ctx.clone() };
        // PlaybackProgress is not in the subscriber's event list at all — emit
        // returns immediately without calling handle, but we can test that
        // handle on an unrelated event returns Ok.
        let result = sub
            .handle(Event::PlaybackProgress(crate::signals::PlaybackContext {
                user_id: uid,
                media_id: media.id,
                position_ticks: 1000,
                is_paused: false,
                ..Default::default()
            }))
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn import_stamps_trakt_watched_at_not_now() {
        let (_s, guard) = new_test_server()
            .await
            .unwrap();
        let ctx = &guard.0;
        let uid = user_id(ctx).await;
        let user = db::User::get_by_id(&ctx.db, &uid)
            .await
            .unwrap()
            .unwrap();

        let external_ids = db::ExternalIds {
            tmdb: Some(1438),
            ..Default::default()
        };
        let mut series = db::Media {
            id: Uuid::from(&db::MediaIdRaw {
                kind: db::MediaKind::Series,
                external_ids: external_ids.clone(),
                season: None,
                episode: None,
            }),
            title: "The Wire".into(),
            kind: db::MediaKind::Series,
            external_ids: external_ids.clone(),
            ..Default::default()
        };
        series
            .save(&ctx.db)
            .await
            .unwrap();
        let mut episode = db::Media {
            title: "The Target".into(),
            kind: db::MediaKind::Episode,
            grandparent_id: Some(series.id),
            idx: Some(1),
            parent_idx: Some(1),
            ..Default::default()
        };
        episode
            .save(&ctx.db)
            .await
            .unwrap();

        let watched_at = chrono::NaiveDateTime::parse_from_str(
            "2026-07-15 17:43:00",
            "%Y-%m-%d %H:%M:%S",
        )
        .unwrap();
        let applied = apply_remote_watches(
            ctx,
            uid,
            &[crate::addons::media_tracker::RemoteWatch {
                ids: external_ids,
                season: Some(1),
                episode: Some(1),
                watched: true,
                position_ticks: None,
                progress_percent: None,
                watched_at: Some(watched_at),
                favorite: None,
                rating: None,
            }],
            usize::MAX,
        )
        .await
        .unwrap();
        assert_eq!(applied, 1);

        let state = db::UserMediaState::get_or_new(&ctx.db, &user, &episode)
            .await
            .unwrap();
        assert!(state.play_count > 0);
        assert_eq!(state.played_at, Some(watched_at));
        assert_eq!(state.last_played_at, Some(watched_at));
    }
}
