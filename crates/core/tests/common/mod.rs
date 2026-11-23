//! A tiny local HTTP server for tests, so nothing touches the internet.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn ok(ct: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), ct.into())],
            body: body.into(),
        }
    }
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: vec![],
            body: Vec::new(),
        }
    }
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub struct Server {
    pub base: String,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    pub fn start<F>(handler: F) -> Self
    where
        F: Fn(&str, &str) -> Reply + Send + Sync + 'static,
    {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let handler = Arc::new(handler);
        std::thread::spawn(move || {
            // One thread per request, so a slow or stalled request never
            // holds up the others (the app fetches assets in parallel).
            for mut req in server.incoming_requests() {
                let handler = handler.clone();
                let seen = seen2.clone();
                std::thread::spawn(move || {
                    let mut body = Vec::new();
                    let _ = req.as_reader().read_to_end(&mut body);
                    let method = req.method().to_string();
                    let url = req.url().to_string();
                    seen.lock().unwrap().push(Seen {
                        method: method.clone(),
                        url: url.clone(),
                        headers: req
                            .headers()
                            .iter()
                            .map(|h| (h.field.to_string(), h.value.to_string()))
                            .collect(),
                        body,
                    });
                    let r = handler(&method, &url);
                    let mut resp =
                        tiny_http::Response::from_data(r.body).with_status_code(r.status);
                    for (k, v) in r.headers {
                        resp.add_header(
                            tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap(),
                        );
                    }
                    let _ = req.respond(resp);
                });
            }
        });
        Self {
            base: format!("http://127.0.0.1:{port}"),
            seen,
        }
    }

    pub fn requests_to(&self, path_prefix: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.url.starts_with(path_prefix))
            .cloned()
            .collect()
    }
}

/// 1x1 transparent PNG.
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];
