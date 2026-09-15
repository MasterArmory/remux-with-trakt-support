use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{info, warn};

use super::{ProgressReporter, Task, TaskCategory, TaskService};
use crate::{
    AppContext,
    addons::media_tracker::MediaTrackerCtx,
    db,
    services::media_tracker::{apply_remote_watches, ensure_fresh_credentials},
};

pub struct MediaTrackerSyncTask;

#[async_trait]
impl Task for MediaTrackerSyncTask {
    fn key(&self) -> &str {
        "MediaTrackerSync"
    }
    fn name(&self) -> &str {
        "Sync Media Trackers"
    }
    fn description(&self) -> &str {
        "Pulls Trakt last_activities, history, playback, and watching into local Next Up."
    }
    fn short_description(&self) -> &str {
        "Keeps Trakt watch state in sync"
    }
    fn category(&self) -> TaskCategory {
        TaskCategory::Users
    }

    async fn run(
        &self,
        ctx: AppContext,
        _tasks: Arc<TaskService>,
        progress: ProgressReporter,
    ) -> Result<()> {
        let trackers = db::UserMediaTracker::list_connected(&ctx.db).await?;
        if trackers.is_empty() {
            progress.set(100.0);
            return Ok(());
        }
        let tctx = MediaTrackerCtx {
            config: Arc::new(
                ctx.config
                    .clone(),
            ),
        };
        let n = trackers
            .len()
            .max(1);
        for (i, tracker) in trackers
            .iter()
            .enumerate()
        {
            progress.set((i as f64 / n as f64) * 100.0);
            let Some(addon) = ctx
                .addons
                .media_tracker_for(tracker.addon_id)
            else {
                continue;
            };
            let creds =
                match ensure_fresh_credentials(&ctx.db, addon.as_ref(), tracker, &tctx)
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(error = %e, "media tracker token refresh failed");
                        continue;
                    }
                };
            // First successful pull is always a full import. last_activities
            // skipping is only for later incremental cron runs.
            let first_sync = tracker
                .last_success_at
                .is_none();
            if !first_sync {
                if let Ok(Some(remote_at)) = addon
                    .remote_activity_at(&creds, &tctx)
                    .await
                {
                    if let Some(local_at) = tracker.last_success_at {
                        if remote_at <= local_at {
                            continue;
                        }
                    }
                }
            }
            match addon
                .import_history(&creds, &tctx)
                .await
            {
                Ok(watches) => {
                    let persist_cap = if first_sync {
                        usize::MAX
                    } else {
                        crate::services::media_tracker::INCREMENTAL_NEW_TITLES_PER_SYNC
                    };
                    let applied = apply_remote_watches(
                        &ctx,
                        tracker.user_id,
                        &watches,
                        persist_cap,
                    )
                    .await?;
                    let _ =
                        db::UserMediaTracker::mark_success(&ctx.db, tracker.id).await;
                    info!(
                        user_id = %tracker.user_id,
                        applied,
                        "media tracker sync complete"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "media tracker import failed");
                    let _ = db::UserMediaTracker::mark_failure(&ctx.db, tracker.id, &e)
                        .await;
                }
            }
        }
        progress.set(100.0);
        Ok(())
    }
}
