//! A tiny blocking HTTP abstraction so pollers can be tested without a network.
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HttpRequest {
    /// "GET" or "POST".
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Raw request body (form-encoded by the caller).
    pub body: Option<String>,
}

impl HttpRequest {
    #[cfg(test)]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    /// URL after following redirects.
    pub url: String,
    pub body: String,
}

pub trait Http: Send {
    /// Err means the request never completed (network error, timeout). Non-2xx statuses are Ok.
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, String>;
}

/// Real implementation: 30 s timeout, follows redirects and reports the final URL, no cookie jar.
pub struct UreqHttp {
    agent: ureq::Agent,
}

impl UreqHttp {
    pub fn new() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(30))
                .redirects(5)
                .build(),
        }
    }
}

impl Default for UreqHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl Http for UreqHttp {
    fn request(&self, req: HttpRequest) -> Result<HttpResponse, String> {
        let mut r = self.agent.request(&req.method, &req.url);
        for (k, v) in &req.headers {
            r = r.set(k, v);
        }
        let res = match &req.body {
            Some(b) => r.send_string(b),
            None => r.call(),
        };
        match res {
            Ok(resp) | Err(ureq::Error::Status(_, resp)) => {
                let status = resp.status();
                let url = resp.get_url().to_string();
                let body = resp.into_string().map_err(|e| e.to_string())?;
                Ok(HttpResponse { status, url, body })
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Host part of a URL ("https://a.b/c" -> "a.b").
pub fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    auth.rsplit('@').next().unwrap_or("").to_string()
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub type Log = Arc<Mutex<Vec<HttpRequest>>>;
    type Handler = Box<dyn Fn(&HttpRequest) -> Result<HttpResponse, String> + Send>;

    /// Records every request and answers from a closure.
    pub struct MockHttp {
        log: Log,
        handler: Handler,
    }

    impl MockHttp {
        pub fn new(
            handler: impl Fn(&HttpRequest) -> Result<HttpResponse, String> + Send + 'static,
        ) -> (Self, Log) {
            let log: Log = Arc::default();
            (
                Self {
                    log: log.clone(),
                    handler: Box::new(handler),
                },
                log,
            )
        }
    }

    impl Http for MockHttp {
        fn request(&self, req: HttpRequest) -> Result<HttpResponse, String> {
            let r = (self.handler)(&req);
            self.log.lock().unwrap().push(req);
            r
        }
    }

    pub fn ok(url: &str, body: &str) -> Result<HttpResponse, String> {
        Ok(HttpResponse {
            status: 200,
            url: url.into(),
            body: body.into(),
        })
    }
}
