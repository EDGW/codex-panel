use super::registry::{CreateContext, DestinationFactory};
use super::{
    Credentials, Destination, DestinationConfig, PricingInterface, Result, SessionContext,
    SessionPricing, Stats,
};
use crate::url::http_url;
use reqwest::blocking::{Client, Response};
use reqwest::cookie::{CookieStore, Jar};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialConfig {
    hub_url: Option<String>,
}

struct HubConfig {
    hub_url: String,
}

impl DestinationFactory for HubConfig {
    fn create(&self, context: CreateContext) -> Result<Arc<dyn Destination>> {
        Ok(Arc::new(ClaudeCodeHub {
            name: context.name,
            hub_url: self.hub_url.clone(),
            credentials: context.credentials,
            billing: Mutex::new(HashMap::new()),
        }))
    }
}

pub struct ClaudeCodeHub {
    name: String,
    hub_url: String,
    credentials: Arc<dyn Credentials>,
    billing: Mutex<HashMap<Option<String>, BillingClient>>,
}

impl ClaudeCodeHub {
    pub const TYPE: &'static str = "claude-code-hub";

    pub fn parse_config(
        value: &toml::Value,
        complete: bool,
    ) -> Result<Option<Arc<dyn DestinationFactory>>> {
        let config: PartialConfig = value
            .clone()
            .try_into()
            .map_err(|e| format!("config: {e}"))?;
        if let Some(base) = config.hub_url {
            validate_origin(&base).map_err(|e| format!("config.hub_url: {e}"))?;
            Ok(Some(Arc::new(HubConfig { hub_url: base })))
        } else if complete {
            Err("config.hub_url: missing required field".into())
        } else {
            Ok(None)
        }
    }
}

fn validate_origin(base: &str) -> Result<Url> {
    let url = http_url(base, false)?;
    if url.path() != "/" {
        return Err("must be an HTTP(S) origin without a path".into());
    }
    Ok(url)
}

pub struct ClaudeCodeHubSettings {
    hub_url: String,
}

impl super::DestinationSettings for ClaudeCodeHubSettings {
    fn height(&self, width: u16) -> u16 {
        crate::panel::card::height(&self.card(), width)
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
        frame.render_widget(self.card(), area);
    }
}

impl ClaudeCodeHubSettings {
    fn card(&self) -> ratatui::widgets::Paragraph<'_> {
        use crate::panel::card;
        crate::panel::card::paragraph(
            " Claude Code Hub ",
            vec![
                card::field("Hub", &self.hub_url, card::primary()),
                ratatui::text::Line::styled("Configured in destinations.toml", card::secondary()),
            ],
        )
    }
}

impl Destination for ClaudeCodeHub {
    fn settings(&self) -> Option<Arc<dyn super::DestinationSettings>> {
        Some(Arc::new(ClaudeCodeHubSettings {
            hub_url: self.hub_url.clone(),
        }))
    }

    fn config(&self) -> DestinationConfig {
        DestinationConfig {
            name: self.name.clone(),
            billing_currency: "USD".into(),
        }
    }

    fn pricing(&self) -> PricingInterface<'_> {
        PricingInterface::SessionTotals(self)
    }
}

impl SessionPricing for ClaudeCodeHub {
    fn session_totals(&self, context: &SessionContext) -> Result<(Stats, bool)> {
        let mut clients = self.billing.lock().map_err(|_| "billing lock poisoned")?;
        if !clients.contains_key(&context.credential_profile) {
            clients.insert(
                context.credential_profile.clone(),
                BillingClient::new(&self.hub_url, self.credentials.clone())?,
            );
        }
        clients
            .get_mut(&context.credential_profile)
            .expect("profile client initialized")
            .query(context)
    }
}

fn stats_from_json(value: Value) -> Result<Stats> {
    let cost = value.get("totalCost").ok_or("missing totalCost")?;
    let amount = cost
        .as_f64()
        .or_else(|| cost.as_str()?.parse().ok())
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .ok_or("invalid totalCost")?;
    let requests = value
        .get("totalRequests")
        .and_then(Value::as_u64)
        .ok_or("missing totalRequests")?;
    Ok(Stats {
        amount,
        requests: Some(requests),
    })
}

struct BillingClient {
    client: Client,
    jar: Arc<Jar>,
    base: Url,
    credentials: Arc<dyn Credentials>,
    valid_until: Option<SystemTime>,
    authenticated: bool,
    provider: Option<String>,
}

impl BillingClient {
    fn new(base: &str, credentials: Arc<dyn Credentials>) -> Result<Self> {
        let base = validate_origin(base)?;
        let jar = Arc::new(Jar::default());
        let client = Client::builder()
            .cookie_provider(jar.clone())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .user_agent("Mozilla/5.0 codex-panel/0.1")
            .build()
            .map_err(|_| "cannot initialize relay HTTP client")?;
        Ok(Self {
            client,
            jar,
            base,
            credentials,
            provider: None,
            valid_until: None,
            authenticated: false,
        })
    }

    fn endpoint(&self, path: &str) -> Url {
        self.base.join(path).expect("constant relay endpoint")
    }

    fn auth_valid(&self) -> bool {
        self.authenticated
            && self
                .jar
                .cookies(&self.endpoint("api/v1/usage-logs/stats"))
                .is_some()
            && self
                .valid_until
                .is_none_or(|expiry| expiry > SystemTime::now() + Duration::from_secs(60))
    }

    fn login(&mut self, provider: Option<&str>) -> Result<()> {
        self.authenticated = false;
        let key = self.credentials.api_key(provider)?;
        let response = self
            .client
            .post(self.endpoint("api/auth/login"))
            .header("Origin", self.base.as_str().trim_end_matches('/'))
            .header("Referer", self.endpoint("zh-CN/login").as_str())
            .json(&json!({"key": key}))
            .send()
            .map_err(|_| "relay login network error")?;
        if !response.status().is_success() {
            return Err(format!("relay login HTTP {}", response.status().as_u16()));
        }
        let cookie = response
            .cookies()
            .find(|cookie| cookie.name() == "auth-token")
            .ok_or("relay login returned no auth-token")?;
        self.valid_until = cookie
            .max_age()
            .and_then(|age| SystemTime::now().checked_add(age))
            .or_else(|| cookie.expires());
        let result: Value = response
            .json()
            .map_err(|_| "invalid relay login response")?;
        if result.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err("relay login rejected".into());
        }
        self.authenticated = true;
        if self
            .jar
            .cookies(&self.endpoint("api/v1/usage-logs/stats"))
            .is_none()
        {
            self.authenticated = false;
            return Err("relay auth-token cookie is not usable for this URL".into());
        }
        Ok(())
    }

    fn usage(&self, thread_id: &str) -> Result<Response> {
        let mut url = self.endpoint("api/v1/usage-logs/stats");
        url.query_pairs_mut()
            .append_pair("sessionId", thread_id)
            .append_pair("excludeStatusCode200", "false");
        self.client
            .get(url)
            .header("Accept", "application/json")
            .send()
            .map_err(|_| "relay cost query network error".to_owned())
    }

    fn query(&mut self, target: &SessionContext) -> Result<(Stats, bool)> {
        if self.provider != target.credential_profile {
            self.authenticated = false;
            self.provider = target.credential_profile.clone();
        }
        if !self.auth_valid() {
            self.login(target.credential_profile.as_deref())?;
        }
        let mut recovered = false;
        let mut response = self.usage(&target.session_id)?;
        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            recovered = true;
            // Re-read the key on renewal, including when the user changed auth.json.
            self.login(target.credential_profile.as_deref())?;
            response = self.usage(&target.session_id)?;
        }
        if !response.status().is_success() {
            return Err(format!(
                "relay cost query HTTP {}",
                response.status().as_u16()
            ));
        }
        let value = response.json().map_err(|_| "invalid relay cost response")?;
        stats_from_json(value).map(|stats| (stats, recovered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn settings_card_wraps_hub_configuration_within_its_reported_height() {
        use super::super::DestinationSettings;
        use ratatui::{Terminal, backend::TestBackend};
        let settings = ClaudeCodeHubSettings {
            hub_url: "https://billing.example.com".into(),
        };
        for width in [24, 100] {
            let height = settings.height(width);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| settings.render(frame, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("Claude Code Hub"));
            assert_eq!(buffer[(0, 0)].symbol(), "╭");
            assert_eq!(buffer[(0, height - 1)].symbol(), "╰");
            let body: String = (1..height - 1)
                .flat_map(|y| (1..width - 1).map(move |x| (x, y)))
                .map(|position| buffer[position].symbol())
                .collect::<String>()
                .replace(' ', "");
            assert!(body.contains("Hub:https://billing.example.com"));
            assert!(body.contains("Configuredindestinations.toml"));
        }
    }
    struct TestCredentials;
    impl Credentials for TestCredentials {
        fn api_key(&self, _: Option<&str>) -> Result<String> {
            Ok("test-only-key".into())
        }
    }
    #[test]
    fn relay_renews_rejected_and_expiring_tokens_and_uses_cookie_jar() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            for (i, code) in [200, 401, 200, 200, 200, 200].into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                let size = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length: ")?
                            .parse::<usize>()
                            .ok()
                    })
                    .unwrap_or(0);
                let mut body = vec![0; size];
                reader.read_exact(&mut body).unwrap();
                let login = matches!(i, 0 | 2 | 4);
                let (cookie, body) = if login {
                    assert!(first.starts_with("POST /api/auth/login"));
                    (
                        "Set-Cookie: auth-token=test-cookie; Path=/; HttpOnly; Max-Age=3600\r\n",
                        r#"{"ok":true}"#,
                    )
                } else {
                    assert!(first.contains("sessionId=chat"));
                    assert!(
                        headers
                            .to_lowercase()
                            .contains("cookie: auth-token=test-cookie")
                    );
                    ("", r#"{"totalCost":0.25,"totalRequests":3}"#)
                };
                write!(stream, "HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{cookie}Connection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let mut relay = BillingClient::new(&base, Arc::new(TestCredentials)).unwrap();
        let target = SessionContext {
            session_id: "chat".into(),
            credential_profile: None,
        };
        let (stats, reconnected) = relay.query(&target).unwrap();
        assert!(reconnected);
        assert_eq!(stats.requests, Some(3));
        relay.valid_until = Some(SystemTime::now() + Duration::from_secs(30));
        let (_, reconnected) = relay.query(&target).unwrap();
        assert!(!reconnected, "planned expiry renewal is not a failure");
        worker.join().unwrap();
    }

    #[test]
    fn cost_uses_billed_total_and_rejects_missing_or_invalid_data() {
        assert_eq!(
            stats_from_json(json!({"totalCost":"0.00167324","totalRequests":2})).unwrap(),
            Stats {
                amount: 0.00167324,
                requests: Some(2)
            }
        );
        assert!(stats_from_json(json!({"totalRequests":0})).is_err());
        assert!(stats_from_json(json!({"totalCost":"NaN","totalRequests":0})).is_err());
    }
}
