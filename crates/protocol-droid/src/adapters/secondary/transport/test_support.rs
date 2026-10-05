//! A server that answers whatever a test tells it to, shared by the two HTTP
//! transport tests.
//!
//! Exists because the client and the detector need the *same* fixture for
//! different claims: the client needs a peer that sends a particular first
//! frame, and the detector needs a peer that answers a particular status. Two
//! copies would drift, and a drift between them would show up as one of the two
//! suites passing against a server the other no longer matches.
//!
//! Deliberately not a fixture in `tests/`. An integration test cannot see a
//! `#[cfg(test)]` module, so a shared one has to live inside the crate — which is
//! the same reason the detection tests are inline rather than beside the client
//! ones.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;

/// The status and body a [`ScriptedServer`] answers the `POST` with.
#[derive(Clone)]
struct Script {
    status: StatusCode,
    content_type: &'static str,
    body: String,
}

/// A server with no protocol behind it: it answers a scripted status and streams
/// scripted frames.
///
/// Every field is shared rather than copied because the router's state and the
/// test's handle have to be the same object — a `Clone` that copied the counters
/// would leave the test reading a number nothing ever increments, and every
/// "that request did not happen" assertion would pass forever.
#[derive(Clone)]
pub(super) struct ScriptedServer {
    url: String,
    stream: Arc<std::sync::Mutex<Vec<String>>>,
    script: Arc<std::sync::Mutex<Script>>,
    get_status: Arc<std::sync::Mutex<(StatusCode, &'static str)>>,
    gets: Arc<AtomicU64>,
    last_get: Arc<std::sync::Mutex<String>>,
}

impl ScriptedServer {
    /// Binds a server that streams `stream` on a `GET` and answers `200` to a
    /// `POST` until a test says otherwise.
    ///
    /// # Errors
    ///
    /// Anything the listener can fail with, which on a loopback port is a
    /// failure to bind rather than anything about the test.
    pub(super) async fn start(stream: Vec<String>) -> Result<Self, Box<dyn std::error::Error>> {
        let state = Self {
            url: String::new(),
            stream: Arc::new(std::sync::Mutex::new(stream)),
            script: Arc::new(std::sync::Mutex::new(Script {
                status: StatusCode::OK,
                content_type: "application/json",
                body: String::new(),
            })),
            get_status: Arc::new(std::sync::Mutex::new((StatusCode::OK, "text/event-stream"))),
            gets: Arc::new(AtomicU64::new(0)),
            last_get: Arc::new(std::sync::Mutex::new(String::new())),
        };

        let router = Router::new()
            .route(
                "/probe",
                post({
                    let state = state.clone();
                    move || {
                        let state = state.clone();
                        async move { state.post() }
                    }
                })
                .get({
                    let state = state.clone();
                    move |request: Request| {
                        let state = state.clone();
                        async move { state.get(request.uri().path()) }
                    }
                }),
            )
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        Ok(Self {
            url: format!("http://127.0.0.1:{port}/probe"),
            ..state
        })
    }

    /// The URL the client should be pointed at.
    pub(super) fn url(&self) -> &str {
        &self.url
    }

    /// Sets what the `POST` answers with.
    pub(super) fn with_post_status(
        self,
        status: StatusCode,
        content_type: &'static str,
        body: &str,
    ) -> Self {
        *self
            .script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Script {
            status,
            content_type,
            body: body.to_owned(),
        };
        self
    }

    /// Sets what the `GET` answers with.
    pub(super) fn with_get_status(self, status: StatusCode, content_type: &'static str) -> Self {
        *self
            .get_status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (status, content_type);
        self
    }

    /// How many `GET`s reached this server.
    pub(super) fn gets(&self) -> u64 {
        self.gets.load(Ordering::SeqCst)
    }

    /// The path of the last `GET`, so a test can say *where* it was sent.
    pub(super) fn last_get_path(&self) -> String {
        self.last_get
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn post(&self) -> axum::response::Response {
        let script = self
            .script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        (
            script.status,
            [(axum::http::header::CONTENT_TYPE, script.content_type)],
            script.body,
        )
            .into_response()
    }

    fn get(&self, path: &str) -> axum::response::Response {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let path = path.to_owned();
        *self
            .last_get
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = path;
        let (status, content_type) = *self
            .get_status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let body = self
            .stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .join("");
        (
            status,
            [(axum::http::header::CONTENT_TYPE, content_type)],
            body,
        )
            .into_response()
    }
}
