// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Server-sent-events subscription for a run (`docs/SERVER_CLIENT_PLAN.md` §4.1).
//!
//! Wraps a `web_sys::EventSource` on `GET /api/v1/runs/{id}/events`. Unnamed messages are
//! [`dbsloc_api::RunEvent`]s tagged with the SSE `id` (a sequence number the app uses to drop
//! replayed events); the named [`dbsloc_api::SSE_END_EVENT`] carries the terminal
//! [`dbsloc_api::RunStatus`] after which the server closes the stream. A closed stream is reported
//! so the app can reopen it while the run is still active.

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use dbsloc_api::{RunEvent, RunStatus, SSE_END_EVENT};

use crate::api::{ApiClient, ApiMsg};

const READY_STATE_CLOSED: u16 = 2;

/// A live run event stream. Dropping it detaches the callbacks and closes the `EventSource`.
pub struct SseClient {
    source: web_sys::EventSource,
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_end: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_error: Closure<dyn FnMut(web_sys::Event)>,
}

impl SseClient {
    /// Subscribe to `GET {base}/api/v1/runs/{run_id}/events`, resuming after `last_event_id`.
    pub fn connect(
        api: &ApiClient,
        run_id: &str,
        last_event_id: Option<u64>,
    ) -> Result<Self, String> {
        let mut url = format!(
            "{}/api/v1/runs/{}/events",
            api.base().trim_end_matches('/'),
            run_id
        );
        if let Some(id) = last_event_id {
            url.push_str(&format!("?last_event_id={id}"));
        }
        let source = web_sys::EventSource::new(&url).map_err(|e| format!("EventSource: {e:?}"))?;

        let on_message = {
            let api = api.clone();
            let run_id = run_id.to_owned();
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |ev: web_sys::MessageEvent| {
                let Some(data) = ev.data().as_string() else {
                    return;
                };
                if let Ok(event) = serde_json::from_str::<RunEvent>(&data) {
                    api.emit(ApiMsg::RunEvent {
                        run_id: run_id.clone(),
                        id: ev.last_event_id().parse().ok(),
                        event,
                    });
                }
            })
        };
        source.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let on_end = {
            let api = api.clone();
            let run_id = run_id.to_owned();
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |ev: web_sys::MessageEvent| {
                let Some(data) = ev.data().as_string() else {
                    return;
                };
                if let Ok(status) = serde_json::from_str::<RunStatus>(&data) {
                    api.emit(ApiMsg::RunEnd {
                        run_id: run_id.clone(),
                        status,
                    });
                }
            })
        };
        source
            .add_event_listener_with_callback(SSE_END_EVENT, on_end.as_ref().unchecked_ref())
            .map_err(|e| format!("EventSource end listener: {e:?}"))?;

        let on_error = {
            let api = api.clone();
            let run_id = run_id.to_owned();
            let source = source.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                if source.ready_state() == READY_STATE_CLOSED {
                    api.emit(ApiMsg::SseClosed {
                        run_id: run_id.clone(),
                    });
                }
            })
        };
        source.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        Ok(Self {
            source,
            _on_message: on_message,
            _on_end: on_end,
            _on_error: on_error,
        })
    }
}

impl Drop for SseClient {
    fn drop(&mut self) {
        self.source.set_onmessage(None);
        self.source.set_onerror(None);
        let _ = self.source.remove_event_listener_with_callback(
            SSE_END_EVENT,
            self._on_end.as_ref().unchecked_ref(),
        );
        self.source.close();
    }
}
