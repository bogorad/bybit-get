use crate::config::Settings;
use hmac::{Hmac, Mac};
use reqwest::blocking::Client;
use reqwest::header::HeaderMap;
use reqwest::Method;
use serde_json::{value::RawValue, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const CARD_PATH: &str = "/v5/card/transaction/query-asset-records";
pub const TIME_PATH: &str = "/v5/market/time";
pub const CATEGORIES: [&str; 4] = [
    "SIDE_QUERY_AUTH_ALL",
    "SIDE_QUERY_AUTH_REVERSAL",
    "SIDE_QUERY_FINANCIAL_ALL",
    "SIDE_QUERY_FINANCIAL_REFUND",
];
const MAX_RESPONSE: u64 = 8 << 20;
const MAX_PAGES: usize = 10000;

pub struct BybitClient {
    base_url: String,
    api_key: Vec<u8>,
    api_secret: Vec<u8>,
    http: Client,
    interrupted: Arc<AtomicBool>,
    min_card_interval: Duration,
    next_card_request: Instant,
    #[cfg(test)]
    test_timeout: Option<Duration>,
}

struct Reply {
    result: Option<Box<RawValue>>,
    time: Option<Box<RawValue>>,
    headers: HeaderMap,
}

pub struct Transaction {
    pub raw: Box<RawValue>,
    canonical: String,
    pub last4: String,
    created: i64,
}

impl BybitClient {
    pub fn from_settings(settings: Settings, interrupted: Arc<AtomicBool>) -> Result<Self, String> {
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(20))
            .pool_max_idle_per_host(1)
            .build()
            .map_err(|_| "cannot initialize the HTTPS client".to_string())?;
        Ok(Self {
            base_url: settings.base_url,
            api_key: settings.api_key,
            api_secret: settings.api_secret,
            http,
            interrupted,
            min_card_interval: Duration::from_millis(1050),
            next_card_request: Instant::now(),
            #[cfg(test)]
            test_timeout: None,
        })
    }

    fn check_interrupt(&self) -> Result<(), String> {
        if self.interrupted.load(Ordering::Relaxed) {
            Err("Bybit query was interrupted".into())
        } else {
            Ok(())
        }
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        body: Vec<u8>,
        stamp: i64,
    ) -> Result<Reply, String> {
        self.check_interrupt()?;
        let timeout = if path == TIME_PATH {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(20)
        };
        #[cfg(test)]
        let timeout = self.test_timeout.unwrap_or(timeout);
        let mut request = self
            .http
            .request(method.clone(), format!("{}{path}", self.base_url))
            .timeout(timeout)
            .header("Accept", "application/json");
        if method == Method::POST {
            request = request.header("Content-Type", "application/json");
        }
        if stamp > 0 {
            let stamp = stamp.to_string();
            let key = std::str::from_utf8(&self.api_key)
                .map_err(|_| "API key is not valid UTF-8".to_string())?;
            let mut signer = Hmac::<Sha256>::new_from_slice(&self.api_secret)
                .map_err(|_| "cannot initialize request signing".to_string())?;
            signer.update(stamp.as_bytes());
            signer.update(&self.api_key);
            signer.update(b"5000");
            signer.update(&body);
            let signature: String = signer
                .finalize()
                .into_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            request = request
                .header("X-BAPI-API-KEY", key)
                .header("X-BAPI-SIGN", signature)
                .header("X-BAPI-TIMESTAMP", stamp)
                .header("X-BAPI-RECV-WINDOW", "5000");
        }
        let request = request
            .body(body)
            .build()
            .map_err(|_| "cannot construct the Bybit request".to_string())?;
        let http = self.http.clone();
        let (sender, receiver) = mpsc::channel();
        // The blocking transport runs under its own fixed timeout. Polling here
        // permits signals to stop the caller during a stalled socket read.
        std::thread::spawn(move || {
            let reply: Result<(u16, HeaderMap, Vec<u8>), String> = (|| {
                let response = http.execute(request).map_err(|error| {
                    if error.is_timeout() {
                        "Bybit request timed out or was interrupted"
                    } else {
                        "Bybit connection failed"
                    }
                    .to_string()
                })?;
                let status = response.status().as_u16();
                let headers = response.headers().clone();
                let mut raw = Vec::new();
                response
                    .take(MAX_RESPONSE + 1)
                    .read_to_end(&mut raw)
                    .map_err(|_| "cannot read a bounded Bybit response".to_string())?;
                if raw.len() as u64 > MAX_RESPONSE {
                    return Err("cannot read a bounded Bybit response".into());
                }
                Ok((status, headers, raw))
            })();
            let _ = sender.send(reply);
        });
        let deadline = Instant::now() + timeout;
        let (status, headers, raw) = loop {
            self.check_interrupt()?;
            match receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(reply) => break reply?,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("Bybit connection failed".into())
                }
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    return Err("Bybit request timed out or was interrupted".into())
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        };
        let envelope = serde_json::from_slice::<BTreeMap<String, Box<RawValue>>>(&raw);
        let code = envelope
            .as_ref()
            .ok()
            .and_then(|map| map.get("retCode"))
            .and_then(|raw| strict_integer(raw));
        if !(200..300).contains(&status) {
            return Err(match code {
                Some(code) => format!("Bybit {path} returned HTTP {status}, business code {code}"),
                None => format!("Bybit {path} returned HTTP {status}"),
            });
        }
        let mut envelope =
            envelope.map_err(|_| "Bybit returned an invalid response envelope".to_string())?;
        let code = code.ok_or_else(|| "Bybit returned an invalid response envelope".to_string())?;
        if code != 0 {
            return Err(format!("Bybit {path} returned business code {code}"));
        }
        Ok(Reply {
            result: envelope.remove("result"),
            time: envelope.remove("time"),
            headers,
        })
    }

    fn server_time(&self) -> Result<i64, String> {
        let reply = self.request(Method::GET, TIME_PATH, Vec::new(), 0)?;
        if let Some(stamp) = reply
            .time
            .as_deref()
            .and_then(strict_integer)
            .filter(|stamp| *stamp > 0)
        {
            return Ok(stamp);
        }
        let result: BTreeMap<String, Box<RawValue>> = reply
            .result
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw.get()).ok())
            .ok_or_else(|| "Bybit server time has an invalid result".to_string())?;
        if let Some(nanos) = result
            .get("timeNano")
            .and_then(|raw| integer_or_string(raw))
            .filter(|stamp| *stamp > 0)
        {
            return Ok(nanos / 1_000_000);
        }
        if let Some(seconds) = result
            .get("timeSecond")
            .and_then(|raw| integer_or_string(raw))
            .filter(|stamp| *stamp > 0 && *stamp <= i64::MAX / 1000)
        {
            return Ok(seconds * 1000);
        }
        Err("Bybit server time is missing".into())
    }

    fn wait_for_card(&self) -> Result<(), String> {
        self.check_interrupt()?;
        let wait = self
            .next_card_request
            .saturating_duration_since(Instant::now());
        if wait > Duration::from_secs(60) {
            return Err(
                "Bybit requires a rate-limit wait longer than one minute; retry later".into(),
            );
        }
        while Instant::now() < self.next_card_request {
            self.check_interrupt()?;
            std::thread::sleep(
                self.next_card_request
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(25)),
            );
        }
        self.check_interrupt()
    }

    fn update_pacing(&mut self, headers: &HeaderMap) {
        let now = Instant::now();
        self.next_card_request = now + self.min_card_interval;
        let remaining = headers
            .get("X-Bapi-Limit-Status")
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.parse::<i64>().ok());
        if remaining != Some(0) {
            return;
        }
        let reset = headers
            .get("X-Bapi-Limit-Reset-Timestamp")
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.parse::<u64>().ok());
        if let Some(reset) = reset {
            if let Some(ready) = UNIX_EPOCH
                .checked_add(Duration::from_millis(reset))
                .and_then(|time| time.checked_add(Duration::from_millis(50)))
            {
                if let Ok(wait) = ready.duration_since(SystemTime::now()) {
                    if let Some(next) = now.checked_add(wait) {
                        self.next_card_request = self.next_card_request.max(next);
                    }
                }
            }
        }
    }

    fn fetch_category(
        &mut self,
        category: &str,
        filter: Option<&str>,
        end: i64,
    ) -> Result<Vec<Transaction>, String> {
        let mut records = Vec::new();
        let mut seen = HashSet::new();
        let mut expected = None;
        for page in 1..=MAX_PAGES {
            self.wait_for_card()?;
            let stamp = self.server_time()?;
            let mut payload = serde_json::json!({"page": page, "limit": 50, "type": category, "createEndTime": end});
            if let Some(filter) = filter {
                payload["pan4"] = Value::String(filter.into());
            }
            let body = serde_json::to_vec(&payload)
                .map_err(|_| "cannot encode the Card request".to_string())?;
            let reply = self.request(Method::POST, CARD_PATH, body, stamp)?;
            self.update_pacing(&reply.headers);
            let result: BTreeMap<String, Box<RawValue>> = reply
                .result
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw.get()).ok())
                .ok_or_else(|| format!("{category} page {page}: invalid result object"))?;
            let total = result
                .get("totalCount")
                .and_then(|raw| strict_integer(raw))
                .filter(|total| *total >= 0)
                .ok_or_else(|| format!("{category} page {page}: invalid totalCount"))?;
            if expected.is_some_and(|expected| expected != total) {
                return Err(format!(
                    "{category}: totalCount changed during pagination; retry the full query"
                ));
            }
            expected = Some(total);
            let data: Vec<Box<RawValue>> = result
                .get("data")
                .and_then(|raw| serde_json::from_str(raw.get()).ok())
                .ok_or_else(|| format!("{category} page {page}: missing or invalid data list"))?;
            let empty = data.is_empty();
            for raw in data {
                let record = parse_transaction(raw, filter)
                    .map_err(|message| format!("{category} page {page}: {message}"))?;
                let fingerprint = Sha256::digest(record.canonical.as_bytes());
                if !seen.insert(fingerprint) {
                    return Err(format!("{category}: a transaction repeated during pagination; retry the full query"));
                }
                records.push(record);
            }
            if records.len() as u64 > total as u64 {
                return Err(format!("{category}: returned more records than totalCount"));
            }
            if records.len() as u64 == total as u64 {
                return Ok(records);
            }
            if empty {
                return Err(format!(
                    "{category}: empty page before totalCount was reached"
                ));
            }
        }
        Err(format!("{category}: pagination exceeded the safety bound"))
    }

    pub fn fetch_all(&mut self, filter: Option<&str>) -> Result<Vec<Transaction>, String> {
        let end = self.server_time()?;
        let mut records = Vec::new();
        let mut seen = HashSet::new();
        for category in CATEGORIES {
            for record in self.fetch_category(category, filter, end)? {
                if seen.insert(Sha256::digest(record.canonical.as_bytes())) {
                    records.push(record);
                }
            }
        }
        records.sort_by(|a, b| {
            b.created
                .cmp(&a.created)
                .then_with(|| a.canonical.cmp(&b.canonical))
        });
        Ok(records)
    }
}

fn strict_integer(raw: &RawValue) -> Option<i64> {
    raw.get().parse::<i64>().ok()
}
fn integer_or_string(raw: &RawValue) -> Option<i64> {
    strict_integer(raw).or_else(|| serde_json::from_str::<String>(raw.get()).ok()?.parse().ok())
}

fn parse_transaction(raw: Box<RawValue>, filter: Option<&str>) -> Result<Transaction, String> {
    let fields: BTreeMap<String, Box<RawValue>> = serde_json::from_str(raw.get())
        .map_err(|_| "transaction record is not an object".to_string())?;
    let last4: String = fields
        .get("pan4")
        .and_then(|raw| serde_json::from_str(raw.get()).ok())
        .filter(|last4: &String| {
            last4.len() == 4 && last4.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(|| "transaction record has no valid four-digit card ending".to_string())?;
    if filter.is_some_and(|filter| filter != last4) {
        return Err("Bybit returned a transaction for a different card".into());
    }
    let created = fields
        .get("txnCreate")
        .and_then(|raw| integer_or_string(raw))
        .filter(|stamp| *stamp >= 0)
        .ok_or_else(|| "transaction record has no valid creation timestamp".to_string())?;
    let canonical = serde_json::from_str::<Value>(raw.get())
        .and_then(|value| serde_json::to_string(&value))
        .map_err(|_| "invalid transaction JSON".to_string())?;
    Ok(Transaction {
        raw,
        canonical,
        last4,
        created,
    })
}

#[cfg(test)]
impl BybitClient {
    pub(crate) fn test_fast(&mut self) {
        self.min_card_interval = Duration::ZERO;
    }
    pub(crate) fn test_server_time(&self) -> Result<i64, String> {
        self.server_time()
    }
    pub(crate) fn test_interrupt(&mut self, flag: Arc<AtomicBool>) {
        self.interrupted = flag;
    }
    pub(crate) fn test_timeout(&mut self, timeout: Duration) {
        self.test_timeout = Some(timeout);
    }
}

#[cfg(test)]
mod pacing_tests {
    use super::*;

    #[test]
    fn success_with_zero_remaining_preserves_minimum_and_extends_reset() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut client = BybitClient::from_settings(
            Settings {
                base_url: "https://api.bybit.com".into(),
                api_key: b"dummy".to_vec(),
                api_secret: b"dummy".to_vec(),
            },
            Arc::clone(&flag),
        )
        .unwrap();
        let start = Instant::now();
        let mut headers = HeaderMap::new();
        headers.insert("X-Bapi-Limit-Status", "0".parse().unwrap());
        headers.insert("X-Bapi-Limit-Reset-Timestamp", "1".parse().unwrap());
        client.update_pacing(&headers);
        assert!(client.next_card_request.duration_since(start) >= Duration::from_millis(1050));
        let reset = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            + 3000;
        headers.insert(
            "X-Bapi-Limit-Reset-Timestamp",
            reset.to_string().parse().unwrap(),
        );
        client.update_pacing(&headers);
        assert!(
            client
                .next_card_request
                .saturating_duration_since(Instant::now())
                >= Duration::from_millis(3000)
        );
        flag.store(true, Ordering::Relaxed);
        assert!(client.wait_for_card().unwrap_err().contains("interrupted"));
        flag.store(false, Ordering::Relaxed);
        client.next_card_request = Instant::now() + Duration::from_secs(61);
        assert!(client
            .wait_for_card()
            .unwrap_err()
            .contains("longer than one minute"));
    }
}
