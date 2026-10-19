//! A Google that answers exactly what a test scripts.
//!
//! Every endpoint in this module is injectable through [`super::Endpoints`]
//! for this one reason: there is no Google account here to test against, and
//! there must never be one. So the tests run against a plain-HTTP listener on
//! loopback that answers whatever the test says, and records every request so
//! the test can assert on what Geode actually sent — the same trick
//! [`crate::share::trust`] plays with its client and the portmap tests play
//! with a fake router.
//!
//! What this proves is Geode's half: the parameters it sends, the headers it
//! reads, how it resumes, what it deletes when something goes wrong, and the
//! words a failure comes back as. It proves nothing about Google's behaviour,
//! which is why [`super`] says plainly what stays unverified until the owner
//! connects an account.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;

use super::Endpoints;

/// One request as it arrived.
#[derive(Debug, Clone)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Call {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// One query parameter.
    pub fn param(&self, key: &str) -> Option<String> {
        url::form_urlencoded::parse(self.query.as_bytes())
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    /// A form-encoded body as its fields.
    pub fn form(&self) -> HashMap<String, String> {
        url::form_urlencoded::parse(&self.body)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

/// What the fake answers with.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn bytes(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn json(status: u16, value: serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: serde_json::to_vec(&value).unwrap(),
        }
    }

    pub fn with(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    /// The shape a Drive error comes back in.
    pub fn error(status: u16, reason: &str, message: &str) -> Self {
        Reply::json(
            status,
            serde_json::json!({
                "error": {
                    "code": status,
                    "message": message,
                    "errors": [{ "domain": "global", "reason": reason, "message": message }],
                }
            }),
        )
    }
}

/// A listener that answers through one closure and records every call.
pub struct Fake {
    pub endpoints: Endpoints,
    seen: Arc<Mutex<Vec<Call>>>,
}

impl Fake {
    /// Start answering. `make` is handed the base URL, so a handler can put
    /// an absolute URL of its own — an upload session URI — in a header.
    pub async fn start<H>(make: impl FnOnce(String) -> H) -> Self
    where
        H: Fn(&Call) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let handler: Arc<dyn Fn(&Call) -> Reply + Send + Sync> = Arc::new(make(base.clone()));
        let seen = Arc::new(Mutex::new(Vec::new()));

        let (loop_handler, loop_seen) = (handler.clone(), seen.clone());
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let (handler, seen) = (loop_handler.clone(), loop_seen.clone());
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                        let (handler, seen) = (handler.clone(), seen.clone());
                        async move {
                            let call = record(request).await;
                            let reply = handler(&call);
                            seen.lock().unwrap().push(call);
                            let mut builder = Response::builder().status(reply.status);
                            for (name, value) in &reply.headers {
                                builder = builder.header(name, value);
                            }
                            Ok::<_, Infallible>(
                                builder.body(Full::new(Bytes::from(reply.body))).unwrap(),
                            )
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .timer(TokioTimer::new())
                        .serve_connection(TokioIo::new(tcp), service)
                        .await;
                });
            }
        });

        Self {
            endpoints: Endpoints {
                auth: format!("{base}/auth"),
                token: format!("{base}/token"),
                revoke: format!("{base}/revoke"),
                drive: format!("{base}/drive/v3"),
                upload: format!("{base}/upload/drive/v3"),
                public: Some(format!("{base}/public")),
            },
            seen,
        }
    }

    pub fn calls(&self) -> Vec<Call> {
        self.seen.lock().unwrap().clone()
    }

    /// Every call to one path, in order.
    pub fn to_path(&self, path: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| call.path == path)
            .collect()
    }
}

async fn record(request: Request<Incoming>) -> Call {
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let query = request.uri().query().unwrap_or("").to_string();
    let headers = request
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = request
        .into_body()
        .collect()
        .await
        .map(|collected| collected.to_bytes().to_vec())
        .unwrap_or_default();
    Call {
        method,
        path,
        query,
        headers,
        body,
    }
}
