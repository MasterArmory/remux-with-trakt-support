use crate::{components::ErrorAlert, state::AppState};
use dioxus::prelude::*;
use remux_sdks::remux::{
    AddonDto, BeginMediaTrackerDeviceAuth, GetCurrentUser, ListAddons,
    ListMediaTrackers, MediaTrackerDto, PollMediaTrackerDeviceAuth, SyncMediaTracker,
};
use uuid::Uuid;

#[component]
pub fn MediaTrackersPage(app_state: AppState) -> Element {
    let mut addons: Signal<Vec<AddonDto>> = use_signal(Vec::new);
    let mut connections: Signal<Vec<MediaTrackerDto>> = use_signal(Vec::new);
    let mut user_id: Signal<Option<Uuid>> = use_signal(|| None);
    let mut loading = use_signal(|| true);
    let mut error: Signal<Option<String>> = use_signal(|| None);
    let mut user_code: Signal<Option<String>> = use_signal(|| None);
    let mut verification_url: Signal<Option<String>> = use_signal(|| None);
    let mut connecting = use_signal(|| false);
    let mut syncing = use_signal(|| false);
    let mut refresh = use_signal(|| 0_u32);

    let app_state_effect = app_state.clone();
    use_effect(move || {
        let _r = *refresh.read();
        loading.set(true);
        let client = app_state_effect.clone();
        spawn(async move {
            match client
                .execute(GetCurrentUser)
                .await
            {
                Ok(me) => {
                    let uid = me.id;
                    user_id.set(Some(uid));
                    let addons_res = client
                        .execute(ListAddons)
                        .await;
                    let trackers_res = client
                        .execute(ListMediaTrackers { user_id: uid })
                        .await;
                    match (addons_res, trackers_res) {
                        (Ok(a), Ok(t)) => {
                            addons.set(a);
                            connections.set(t);
                            error.set(None);
                        }
                        (Err(e), _) | (_, Err(e)) => {
                            error.set(Some(e.to_string()));
                        }
                    }
                }
                Err(e) => error.set(Some(e.to_string())),
            }
            loading.set(false);
        });
    });

    let trakt = addons
        .read()
        .iter()
        .find(|a| a.kind == "trakt")
        .cloned();
    let connected = connections
        .read()
        .iter()
        .any(|c| c.status == "connected");

    rsx! {
        div { class: "page",
            h1 { "Trakt" }
            p { class: "muted",
                "Connect your Trakt account with device login, then Remux will scrobble and pull history every 5 minutes."
            }
            if let Some(err) = error.read().as_ref() {
                ErrorAlert { message: err.clone() }
            }
            if *loading.read() {
                p { "Loading…" }
            } else if let Some(addon) = trakt {
                if connected {
                    p { "Status: connected" }
                    button {
                        disabled: *syncing.read(),
                        onclick: {
                            let client = app_state.clone();
                            let addon_id = addon.id;
                            move |_| {
                                let Some(uid) = *user_id.read() else { return };
                                syncing.set(true);
                                let client = client.clone();
                                spawn(async move {
                                    match client
                                        .execute(SyncMediaTracker {
                                            user_id: uid,
                                            addon_id,
                                        })
                                        .await
                                    {
                                        Ok(r) => error.set(Some(format!("Imported {} items", r.applied))),
                                        Err(e) => error.set(Some(e.to_string())),
                                    }
                                    syncing.set(false);
                                    let n = *refresh.peek();
                                    refresh.set(n + 1);
                                });
                            }
                        },
                        if *syncing.read() { "Syncing…" } else { "Sync now" }
                    }
                } else if let Some(code) = user_code.read().clone() {
                    p { "Open the Trakt activate page and enter this code:" }
                    p { class: "code", "{code}" }
                    if let Some(url) = verification_url.read().as_ref() {
                        a { href: url.clone(), target: "_blank", "{url}" }
                    }
                    p { "This page polls automatically until you approve." }
                } else {
                    button {
                        disabled: *connecting.read(),
                        onclick: {
                            let client = app_state.clone();
                            let addon_id = addon.id;
                            move |_| {
                                let Some(uid) = *user_id.read() else { return };
                                connecting.set(true);
                                error.set(None);
                                let client = client.clone();
                                spawn(async move {
                                    match client
                                        .execute(BeginMediaTrackerDeviceAuth {
                                            user_id: uid,
                                            addon_id,
                                        })
                                        .await
                                    {
                                        Ok(start) => {
                                            user_code.set(Some(start.user_code.clone()));
                                            verification_url.set(Some(start.verification_url.clone()));
                                            let poll_token = start.poll_token;
                                            let interval = start.interval_seconds.max(5);
                                            loop {
                                                gloo_timers::future::TimeoutFuture::new(
                                                    (interval as u32) * 1000,
                                                )
                                                .await;
                                                match client
                                                    .execute(PollMediaTrackerDeviceAuth {
                                                        user_id: uid,
                                                        addon_id,
                                                        poll_token: poll_token.clone(),
                                                    })
                                                    .await
                                                {
                                                    Ok(poll) if poll.status == "approved" => {
                                                        user_code.set(None);
                                                        connecting.set(false);
                                                        let n = *refresh.peek();
                                                        refresh.set(n + 1);
                                                        break;
                                                    }
                                                    Ok(poll) if poll.status == "denied" => {
                                                        error.set(Some("Trakt login was denied or expired.".into()));
                                                        user_code.set(None);
                                                        connecting.set(false);
                                                        break;
                                                    }
                                                    Ok(_) => {}
                                                    Err(e) => {
                                                        error.set(Some(e.to_string()));
                                                        connecting.set(false);
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error.set(Some(e.to_string()));
                                            connecting.set(false);
                                        }
                                    }
                                });
                            }
                        },
                        if *connecting.read() { "Starting…" } else { "Connect Trakt" }
                    }
                }
            } else {
                p { "Add the Trakt addon first (Addons → Trakt), then come back here to connect your account." }
            }
        }
    }
}
