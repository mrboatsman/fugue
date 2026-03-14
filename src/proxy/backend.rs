use md5::{Digest, Md5};
use rand::Rng;
use reqwest::Client;
use serde_json::Value;
use tracing::{debug, error};

use crate::error::FugueError;

#[derive(Clone)]
pub struct BackendClient {
    pub index: usize,
    pub name: String,
    pub base_url: String,
    pub username: String,
    pub password: String,
    pub weight: i32,
    http: Client,
}

impl BackendClient {
    pub fn new(
        index: usize,
        name: String,
        base_url: String,
        username: String,
        password: String,
        weight: i32,
    ) -> Self {
        let http = Client::builder()
            .pool_max_idle_per_host(4)
            .build()
            .expect("Failed to build HTTP client");

        Self {
            index,
            name,
            base_url: base_url.trim_end_matches('/').to_string(),
            username,
            password,
            weight,
            http,
        }
    }

    /// Build the full URL with auth params and extra params encoded in the query string.
    fn build_url(&self, endpoint: &str, extra_params: &[(&str, &str)]) -> String {
        let salt: String = rand::rng()
            .sample_iter(&rand::distr::Alphanumeric)
            .take(16)
            .map(char::from)
            .collect();

        let mut hasher = Md5::new();
        hasher.update(self.password.as_bytes());
        hasher.update(salt.as_bytes());
        let token = hex::encode(hasher.finalize());

        let mut pairs: Vec<(&str, String)> = vec![
            ("u", self.username.clone()),
            ("t", token),
            ("s", salt),
            ("v", "1.16.1".into()),
            ("c", "fugue".into()),
            ("f", "json".into()),
        ];
        for (k, v) in extra_params {
            pairs.push((k, v.to_string()));
        }

        let query = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs.iter().map(|(k, v)| (*k, v.as_str())))
            .finish();

        format!("{}/rest/{}?{}", self.base_url, endpoint, query)
    }

    /// Make a request to the backend and parse the JSON response.
    /// Returns the inner subsonic-response object.
    pub async fn request_json(
        &self,
        endpoint: &str,
        extra_params: &[(&str, &str)],
    ) -> Result<Value, FugueError> {
        let url = self.build_url(endpoint, extra_params);
        debug!("request_json endpoint={} backend={}", endpoint, self.name);

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                error!("backend {} request failed: {}", self.name, e);
                FugueError::Backend(format!("{}: {e}", self.name))
            })?;

        if !resp.status().is_success() {
            error!("backend {} returned HTTP {}", self.name, resp.status());
            return Err(FugueError::Backend(format!(
                "{}: HTTP {}",
                self.name,
                resp.status()
            )));
        }

        let json: Value = resp
            .json::<Value>()
            .await
            .map_err(|e| {
                error!("backend {} JSON parse error: {}", self.name, e);
                FugueError::Backend(format!("{}: parse error: {e}", self.name))
            })?;

        // Navidrome wraps in {"subsonic-response": {...}}
        let inner = json
            .get("subsonic-response")
            .cloned()
            .unwrap_or(json);

        // Check for subsonic error
        if inner.get("status").and_then(|s: &Value| s.as_str()) == Some("failed") {
            if let Some(err) = inner.get("error") {
                let code = err.get("code").and_then(|c: &Value| c.as_u64()).unwrap_or(0) as u32;
                let message = err
                    .get("message")
                    .and_then(|m: &Value| m.as_str())
                    .unwrap_or("Unknown error")
                    .to_string();
                error!("backend {} subsonic error code={}: {}", self.name, code, message);
                return Err(FugueError::Subsonic { code, message });
            }
        }

        Ok(inner)
    }

    /// Make a request to the backend and return the raw response for streaming.
    pub async fn request_stream(
        &self,
        endpoint: &str,
        extra_params: &[(&str, &str)],
    ) -> Result<reqwest::Response, FugueError> {
        let url = self.build_url(endpoint, extra_params);
        debug!("request_stream endpoint={} backend={}", endpoint, self.name);

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| {
                error!("backend {} stream request failed: {}", self.name, e);
                FugueError::Backend(format!("{}: {e}", self.name))
            })?;

        if !resp.status().is_success() {
            error!("backend {} stream returned HTTP {}", self.name, resp.status());
            return Err(FugueError::Backend(format!(
                "{}: HTTP {}",
                self.name,
                resp.status()
            )));
        }

        Ok(resp)
    }
}
