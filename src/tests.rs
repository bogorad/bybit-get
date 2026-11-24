use super::*;
use crate::client::{BybitClient, CARD_PATH, CATEGORIES, TIME_PATH};
use crate::config::{load_config, validate_base_url, Settings};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

static ENV_LOCK: Mutex<()> = Mutex::new(());
static TEMP_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("bybit-get-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct SavedEnv(Vec<(&'static str, Option<OsString>)>);
impl SavedEnv {
    fn clean() -> Self {
        let values = [
            "HOME",
            "XDG_CONFIG_HOME",
            "BYBIT_API_KEY_FILE",
            "BYBIT_API_SECRET_FILE",
            "BYBIT_BASE_URL",
        ]
        .into_iter()
        .map(|name| {
            let saved = std::env::var_os(name);
            std::env::remove_var(name);
            (name, saved)
        })
        .collect();
        Self(values)
    }
}
impl Drop for SavedEnv {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

#[derive(Clone)]
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}
impl Response {
    fn json(body: Value) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }
}
struct Server {
    origin: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(Request) -> Response + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let request = read_request(&mut stream);
                let response = handler(request);
                let mut head = format!("HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", response.status, response.body.len());
                for (name, value) in response.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(response.body.as_bytes());
            }
        });
        Self {
            origin,
            stop,
            thread: Some(thread),
        }
    }
    fn client(&self) -> BybitClient {
        let mut client = BybitClient::from_settings(
            Settings {
                base_url: self.origin.clone(),
                api_key: b"synthetic-key".to_vec(),
                api_secret: b"synthetic-secret".to_vec(),
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        client.test_fast();
        client
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn read_request(stream: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    let mut byte = [0; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 65536);
    }
    let text = String::from_utf8(bytes).unwrap();
    let mut lines = text.split("\r\n");
    let first: Vec<_> = lines.next().unwrap().split(' ').collect();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let count: usize = headers
        .get("content-length")
        .map(|v| v.parse().unwrap())
        .unwrap_or(0);
    let mut body = vec![0; count];
    stream.read_exact(&mut body).unwrap();
    Request {
        method: first[0].into(),
        path: first[1].into(),
        headers,
        body,
    }
}

fn fixture(id: &str, last4: &str, created: i64) -> Value {
    serde_json::from_str(&format!(r#"{{"txnId":"{id}","pan4":"{last4}","txnCreate":{created},"transactionAmount":"12.340000000000000001","referenceNumber":9007199254740993123,"amount":123456789.000000000000000001}}"#)).unwrap()
}
fn page(total: Value, data: Vec<Value>) -> Value {
    json!({"totalCount": total, "data": data})
}

struct Calls {
    card: usize,
    times: i64,
    categories: Vec<String>,
    filters: Vec<Option<String>>,
}
fn fake(
    page_fn: impl Fn(&str, Option<&str>, usize) -> Value + Send + 'static,
) -> (Server, Arc<Mutex<Calls>>) {
    let calls = Arc::new(Mutex::new(Calls {
        card: 0,
        times: 0,
        categories: Vec::new(),
        filters: Vec::new(),
    }));
    let state = Arc::clone(&calls);
    let server = Server::new(move |request| {
        let mut state = state.lock().unwrap();
        if request.path == TIME_PATH {
            assert_eq!(request.method, "GET");
            assert!(!request.headers.contains_key("x-bapi-api-key"));
            let stamp = 1700000000000_i64 + state.times;
            state.times += 1;
            return Response::json(json!({"retCode": 0, "time": stamp, "result": {}}));
        }
        assert_eq!(request.path, CARD_PATH);
        assert_eq!(request.method, "POST");
        assert_eq!(request.headers["x-bapi-api-key"], "synthetic-key");
        assert_eq!(request.headers["x-bapi-recv-window"], "5000");
        assert_eq!(request.headers["content-type"], "application/json");
        assert_eq!(
            request.headers["x-bapi-timestamp"],
            (1700000000000_i64 + state.times - 1).to_string()
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(b"synthetic-secret").unwrap();
        mac.update(format!("{}synthetic-key5000", request.headers["x-bapi-timestamp"]).as_bytes());
        mac.update(&request.body);
        let signature = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(
            request.headers["x-bapi-sign"], signature,
            "signature must cover exact wire body"
        );
        let payload: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(payload["limit"], 50);
        assert_eq!(payload["createEndTime"], 1700000000000_i64);
        for field in ["createBeginTime", "side", "beginTime", "endTime"] {
            assert!(payload.get(field).is_none());
        }
        let number = payload["page"].as_u64().unwrap() as usize;
        let category = payload["type"].as_str().unwrap();
        let filter = payload.get("pan4").map(|value| value.as_str().unwrap());
        state.card += 1;
        state.categories.push(category.into());
        state.filters.push(filter.map(str::to_owned));
        Response::json(json!({"retCode": 0, "result": page_fn(category, filter, number)}))
    });
    (server, calls)
}

#[test]
fn local_cli_paths_never_load_config() {
    for args in [
        vec![],
        vec!["--help"],
        vec!["-h"],
        vec!["--version"],
        vec!["--config", "/missing/sensitive-config"],
    ] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute(
            args.into_iter().map(OsString::from),
            &mut out,
            &mut err,
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(code, 0);
        assert!(err.is_empty());
        assert!(!out.is_empty());
    }
    for args in [
        vec!["123"],
        vec!["12345"],
        vec!["１２３４"],
        vec!["--all", "--cards"],
        vec!["--all", "1234"],
        vec!["1234", "5678"],
        vec!["--unknown"],
        vec!["--config"],
    ] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute(
            args.into_iter().map(OsString::from),
            &mut out,
            &mut err,
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(!err.is_empty());
    }
    assert_eq!(
        cli::parse([OsString::from("0042")]).unwrap().action,
        Action::Last4("0042".into())
    );
}

#[test]
fn config_inline_file_relative_home_and_environment_precedence() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _env = SavedEnv::clean();
    let temp = Temp::new();
    std::env::set_var("HOME", &temp.0);
    std::env::set_var("XDG_CONFIG_HOME", "/must-not-be-used");
    temp.write(".config/bybit-get/key", "file-key\n");
    temp.write("secret", "file-secret\n");
    temp.write(
        ".config/bybit-get/config.toml",
        "api_key_file = 'key'\napi_secret_file = '~/secret'\nbase_url = 'https://api.bybit.eu/'\n",
    );
    let settings = load_config(None).unwrap();
    assert_eq!(settings.api_key, b"file-key");
    assert_eq!(settings.api_secret, b"file-secret");
    assert_eq!(settings.base_url, "https://api.bybit.eu");
    let config = temp.write(
        "inline.toml",
        "api_key = 'inline-key'\napi_secret = 'inline-secret'\n",
    );
    let settings = load_config(Some(config.to_str().unwrap())).unwrap();
    assert_eq!(settings.api_key, b"inline-key");
    assert_eq!(settings.api_secret, b"inline-secret");
    std::env::set_var("BYBIT_API_KEY_FILE", "");
    std::env::set_var("BYBIT_API_SECRET_FILE", "");
    std::env::set_var("BYBIT_BASE_URL", "");
    let settings = load_config(Some(config.to_str().unwrap())).unwrap();
    assert_eq!(settings.api_key, b"inline-key");
    assert_eq!(settings.api_secret, b"inline-secret");
    assert_eq!(settings.base_url, "https://api.bybit.com");
    let key = temp.write("override-key", "env-key\n");
    let secret = temp.write("override-secret", "env-secret\n");
    std::env::set_var("BYBIT_API_KEY_FILE", key);
    std::env::set_var("BYBIT_API_SECRET_FILE", secret);
    std::env::set_var("BYBIT_BASE_URL", "https://api.bytick.com");
    let settings = load_config(Some(config.to_str().unwrap())).unwrap();
    assert_eq!(settings.api_key, b"env-key");
    assert_eq!(settings.api_secret, b"env-secret");
    assert_eq!(settings.base_url, "https://api.bytick.com");
    fs::remove_file(temp.0.join(".config/bybit-get/config.toml")).unwrap();
    assert!(
        load_config(None).is_ok(),
        "missing default config must be optional"
    );
}

#[test]
fn config_failures_are_bounded_and_redacted() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _env = SavedEnv::clean();
    let temp = Temp::new();
    std::env::set_var("HOME", &temp.0);
    std::env::set_var(
        "BYBIT_API_KEY_FILE",
        temp.write("dummy-key", "synthetic-key"),
    );
    std::env::set_var(
        "BYBIT_API_SECRET_FILE",
        temp.write("dummy-secret", "synthetic-secret"),
    );
    for content in [
        "api_key = 'PRIVATE_SENTINEL'\napi_key_file = 'key'",
        "api_secret = 'PRIVATE_SENTINEL'\napi_secret_file = 'secret'",
        "api_key = 'PRIVATE_SENTINEL'\nbroken = [",
        "api_key = 'PRIVATE_SENTINEL'\nunknown = 'x'",
    ] {
        let path = temp.write("bad.toml", content);
        let error = load_config(Some(path.to_str().unwrap())).err().unwrap();
        assert!(!error.contains("PRIVATE_SENTINEL"));
    }
    let path = temp.write("huge.toml", &"x".repeat(65537));
    assert!(load_config(Some(path.to_str().unwrap())).is_err());
    assert!(load_config(Some(temp.0.join("missing").to_str().unwrap())).is_err());
    for content in ["", "first\nsecond", "first\rsecond", "first\0second"] {
        let key = temp.write("key", content);
        std::env::set_var("BYBIT_API_KEY_FILE", key);
        let error = load_config(None).err().unwrap();
        assert!(!error.contains(content) || content.is_empty());
    }
    let key = temp.write("key", &"x".repeat(8193));
    std::env::set_var("BYBIT_API_KEY_FILE", key);
    assert!(load_config(None).is_err());
}

#[test]
fn origins_reject_credential_exfiltration() {
    for origin in [
        "http://api.bybit.com",
        "https://api.bybit.com.evil.invalid",
        "https://user:password@api.bybit.com",
        "https://api.bybit.com/path",
        "https://api.bybit.com/path/..",
        "https://api.bybit.com?secret=value",
        "https://api.bybit.com?",
        "https://api.bybit.com/#fragment",
        "https://api.bybit.com:8443",
    ] {
        assert!(validate_base_url(origin).is_err(), "{origin}");
    }
    for origin in [
        "https://api.bybit.com",
        "https://api.bybit.eu/",
        "https://api.bybit.com:443",
    ] {
        assert!(validate_base_url(origin).is_ok(), "{origin}");
    }
}

#[test]
fn signed_short_pages_deduplication_variants_and_lossless_sorting() {
    let a = fixture("a", "1234", 10);
    let b = fixture("same-id", "1234", 20);
    let mut variant = fixture("same-id", "1234", 30);
    variant["transactionAmount"] = json!("20.00");
    let mut newest = fixture("c", "5678", 40);
    newest["txnCreate"] = json!("40");
    let (server, calls) = fake(move |category, _, number| match category {
        "SIDE_QUERY_AUTH_ALL" if number == 1 => page(json!(3), vec![a.clone()]),
        "SIDE_QUERY_AUTH_ALL" => page(json!(3), vec![b.clone(), variant.clone()]),
        "SIDE_QUERY_AUTH_REVERSAL" => page(json!(1), vec![a.clone()]),
        "SIDE_QUERY_FINANCIAL_ALL" => page(json!(1), vec![newest.clone()]),
        _ => page(json!(0), vec![]),
    });
    let records = server.client().fetch_all(None).unwrap();
    assert_eq!(records.len(), 4);
    let output =
        serde_json::to_string(&records.iter().map(|record| &record.raw).collect::<Vec<_>>())
            .unwrap();
    assert!(output.contains("9007199254740993123"));
    assert!(output.contains("123456789.000000000000000001"));
    assert!(output.contains("12.340000000000000001"));
    let decoded: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(decoded[0]["txnCreate"], "40");
    assert_eq!(decoded[1]["txnCreate"], 30);
    assert_eq!(decoded[2]["txnCreate"], 20);
    assert_eq!(decoded[3]["txnCreate"], 10);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.card, 5);
    for category in CATEGORIES {
        assert!(calls.categories.iter().any(|got| got == category));
    }
}

#[test]
fn raw_transaction_json_survives_the_complete_output_path() {
    let raw = r#"{ "pan4" : "0042", "txnCreate" : "9", "unknown" : { "escaped" : "\\u0041", "fraction" : 1.0000000000000000001 }, "id":184467440737095516161 }"#;
    let server = Server::new(move |request| {
        if request.path == TIME_PATH {
            return Response::json(json!({"retCode":0,"time":1700000000000_i64}));
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        if body["type"] == CATEGORIES[0] {
            Response {
                status: 200,
                headers: vec![],
                body: format!(r#"{{"retCode":0,"result":{{"totalCount":1,"data":[{raw}]}}}}"#),
            }
        } else {
            Response::json(json!({"retCode":0,"result":{"totalCount":0,"data":[]}}))
        }
    });
    let mut out = Vec::new();
    let mut err = Vec::new();
    assert_eq!(
        execute_with_client([OsString::from("--all")], &mut out, &mut err, |_| Ok(
            server.client()
        )),
        0
    );
    assert!(err.is_empty());
    assert_eq!(String::from_utf8(out).unwrap(), format!("[{raw}]\n"));
}

#[test]
fn filtered_205_records_are_not_truncated() {
    let (server, calls) = fake(|category, filter, number| {
        assert_eq!(filter, Some("0042"));
        if category != CATEGORIES[0] {
            return page(json!(0), vec![]);
        }
        let start = (number - 1) * 50;
        let end = (start + 50).min(205);
        page(
            json!(205),
            (start..end)
                .map(|n| fixture(&format!("event-{n}"), "0042", n as i64))
                .collect(),
        )
    });
    let records = server.client().fetch_all(Some("0042")).unwrap();
    assert_eq!(records.len(), 205);
    assert!(records[0].raw.get().contains("event-204"));
    assert!(records[204].raw.get().contains("event-0"));
    assert_eq!(calls.lock().unwrap().card, 8);
}

#[test]
fn card_lists_are_sorted_and_all_empty_modes_emit_arrays() {
    let (server, _) = fake(|category, _, _| {
        if category == CATEGORIES[0] {
            page(
                json!(4),
                vec![
                    fixture("a", "9999", 1),
                    fixture("b", "0042", 2),
                    fixture("c", "1234", 3),
                    fixture("d", "0042", 4),
                ],
            )
        } else {
            page(json!(0), vec![])
        }
    });
    let mut out = Vec::new();
    let mut err = Vec::new();
    assert_eq!(
        execute_with_client([OsString::from("--cards")], &mut out, &mut err, |_| Ok(
            server.client()
        )),
        0
    );
    assert_eq!(out, b"[\"0042\",\"1234\",\"9999\"]\n");
    assert!(err.is_empty());
    for action in ["--all", "--cards", "0042"] {
        let (server, _) = fake(|_, _, _| page(json!(0), vec![]));
        out.clear();
        err.clear();
        assert_eq!(
            execute_with_client([OsString::from(action)], &mut out, &mut err, |_| Ok(
                server.client()
            )),
            0
        );
        assert_eq!(out, b"[]\n");
        assert!(err.is_empty());
    }
}

#[test]
fn malformed_or_incomplete_pagination_fails() {
    for case in [
        "boolean-total",
        "string-total",
        "negative-total",
        "missing-data",
        "null-result",
        "empty-early",
        "overshoot",
        "changing-total",
        "wrong-card",
        "bad-pan4",
        "bad-time",
    ] {
        let name = case.to_string();
        let (server, _) = fake(move |_, _, number| {
            let record = fixture("a", "1234", 10);
            match name.as_str() {
                "boolean-total" => page(json!(true), vec![]),
                "string-total" => page(json!("1"), vec![record]),
                "negative-total" => page(json!(-1), vec![]),
                "missing-data" => json!({"totalCount": 0}),
                "null-result" => Value::Null,
                "empty-early" => page(json!(1), vec![]),
                "overshoot" => page(json!(0), vec![record]),
                "changing-total" => page(
                    json!(number + 1),
                    vec![fixture(&format!("{number}"), "1234", 10)],
                ),
                "wrong-card" => page(json!(1), vec![fixture("a", "9999", 10)]),
                "bad-pan4" => page(json!(1), vec![fixture("a", "１２３４", 10)]),
                _ => page(
                    json!(1),
                    vec![json!({"pan4":"1234", "txnCreate":"invalid"})],
                ),
            }
        });
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_with_client([OsString::from("1234")], &mut out, &mut err, |_| {
            Ok(server.client())
        });
        assert_eq!(code, 1, "{case}");
        assert!(out.is_empty(), "partial stdout for {case}");
        assert!(!err.is_empty());
    }
}

#[test]
fn exact_duplicate_within_category_is_kept_once_and_succeeds() {
    // Bybit can repeat an identical record in one category; keep one copy.
    // Across pages: totalCount counts the raw repeated row, so 2 pages end it.
    let repeated = fixture("a", "1234", 10);
    let (server, calls) = fake(move |category, _, _| {
        if category == CATEGORIES[0] {
            page(json!(2), vec![repeated.clone()])
        } else {
            page(json!(0), vec![])
        }
    });
    let records = server.client().fetch_all(None).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(calls.lock().unwrap().card, 5);

    // Same page: [a, a, b] with totalCount 3 succeeds with two unique records.
    let a = fixture("a", "1234", 10);
    let b = fixture("b", "1234", 20);
    let (server, calls) = fake(move |category, _, _| {
        if category == CATEGORIES[0] {
            page(json!(3), vec![a.clone(), a.clone(), b.clone()])
        } else {
            page(json!(0), vec![])
        }
    });
    let records = server.client().fetch_all(None).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(calls.lock().unwrap().card, 4);
}

#[test]
fn failures_redact_response_and_redirects_stay_local() {
    for status in [200, 403, 302] {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = Arc::clone(&count);
        let server = Server::new(move |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            Response { status, headers: vec![("Location".into(), "http://127.0.0.1:1/private".into())], body: r#"{"retCode":10005,"retMsg":"PRIVATE_RESPONSE_SENTINEL synthetic-key synthetic-secret","result":{}}"#.into() }
        });
        let error = server.client().fetch_all(None).err().unwrap();
        assert!(error.contains("10005"));
        for private in [
            "PRIVATE_RESPONSE_SENTINEL",
            "synthetic-key",
            "synthetic-secret",
        ] {
            assert!(!error.contains(private));
        }
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn public_time_fallbacks_and_bounded_responses() {
    for result in [
        json!({"timeNano":"1700000000000000000"}),
        json!({"timeSecond":1700000000}),
    ] {
        let server = Server::new(move |_| Response::json(json!({"retCode":0,"result":result})));
        assert_eq!(server.client().test_server_time().unwrap(), 1700000000000);
    }
    let server = Server::new(|_| Response {
        status: 200,
        headers: vec![],
        body: "x".repeat((8 << 20) + 1),
    });
    assert!(server
        .client()
        .fetch_all(None)
        .err()
        .unwrap()
        .contains("bounded"));
}

#[test]
fn interrupted_network_request_returns_without_waiting_for_server() {
    let server = Server::new(|_| {
        std::thread::sleep(Duration::from_millis(350));
        Response::json(json!({"retCode":0,"time":1700000000000_i64}))
    });
    let flag = Arc::new(AtomicBool::new(false));
    let mut client = server.client();
    client.test_interrupt(Arc::clone(&flag));
    let signal = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        flag.store(true, Ordering::Relaxed);
    });
    let start = Instant::now();
    assert!(client
        .fetch_all(None)
        .err()
        .unwrap()
        .contains("interrupted"));
    assert!(start.elapsed() < Duration::from_millis(250));
    signal.join().unwrap();
}

#[test]
fn stalled_network_requests_have_a_deadline() {
    let server = Server::new(|_| {
        std::thread::sleep(Duration::from_millis(250));
        Response::json(json!({"retCode":0,"time":1700000000000_i64}))
    });
    let mut client = server.client();
    client.test_timeout(Duration::from_millis(30));
    let start = Instant::now();
    assert!(client.fetch_all(None).err().unwrap().contains("timed out"));
    assert!(start.elapsed() < Duration::from_millis(200));
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn pages_option_parsing_accepts_only_all_or_card_with_a_valid_count() {
    let parsed = cli::parse(args(&["--pages", "3", "--all"])).unwrap();
    assert_eq!((parsed.action, parsed.pages), (Action::All, Some(3)));
    let parsed = cli::parse(args(&["0042", "--pages", "1"])).unwrap();
    assert_eq!(
        (parsed.action, parsed.pages),
        (Action::Last4("0042".into()), Some(1))
    );
    assert_eq!(cli::parse(args(&["--all"])).unwrap().pages, None);
    // Help and version win and do not need a data action.
    assert_eq!(
        cli::parse(args(&["--pages", "2", "--help"]))
            .unwrap()
            .action,
        Action::Help
    );
    assert_eq!(
        cli::parse(args(&["--version", "--pages", "2"]))
            .unwrap()
            .action,
        Action::Version
    );
    for bad in [
        vec!["--all", "--pages"],
        vec!["--all", "--pages", "0"],
        vec!["--all", "--pages", "-1"],
        vec!["--all", "--pages", "+1"],
        vec!["--all", "--pages", "1x"],
        vec!["--all", "--pages", ""],
        vec!["--all", "--pages", "10001"],
        vec!["--all", "--pages", "99999999999999999999999"],
        vec!["--all", "--pages", "--config", "x"],
        vec!["--all", "--pages", "1", "--pages", "2"],
        vec!["--cards", "--pages", "1"],
        vec!["--pages", "1"],
    ] {
        assert!(cli::parse(args(&bad)).is_err(), "{bad:?}");
    }
    assert_eq!(
        cli::parse(args(&["--all", "--pages", "10000"]))
            .unwrap()
            .pages,
        Some(10000)
    );
}

#[test]
fn poll_mode_reads_at_most_n_pages_per_category_and_stops_early() {
    // AUTH_ALL has 3 pages of data; FINANCIAL_ALL has one short page; the others
    // are empty. With --pages 2 the third AUTH_ALL page must not be requested.
    let (server, calls) = fake(|category, _, number| match category {
        "SIDE_QUERY_AUTH_ALL" => page(
            json!(150),
            (0..50)
                .map(|n| {
                    fixture(
                        &format!("a{number}-{n}"),
                        "1234",
                        1000 - number as i64 * 100 + n,
                    )
                })
                .collect(),
        ),
        "SIDE_QUERY_FINANCIAL_ALL" => page(json!(1), vec![fixture("f", "1234", 5000)]),
        _ => page(json!(0), vec![]),
    });
    let records = server.client().fetch(None, Some(2), None).unwrap();
    assert_eq!(records.len(), 101);
    let calls = calls.lock().unwrap();
    // 2 AUTH_ALL pages + 1 per other category.
    assert_eq!(calls.card, 5);
    assert_eq!(
        calls
            .categories
            .iter()
            .filter(|c| *c == "SIDE_QUERY_AUTH_ALL")
            .count(),
        2
    );
}

#[test]
fn poll_mode_one_page_sends_page_one_and_keeps_sort_and_output_format() {
    let newer = fixture("n", "1234", 300);
    let older = fixture("o", "1234", 100);
    let (server, calls) = fake(move |category, _, number| {
        assert_eq!(number, 1, "only page 1 may be requested");
        match category {
            "SIDE_QUERY_AUTH_ALL" => page(json!(500), vec![older.clone(), newer.clone()]),
            _ => page(json!(0), vec![]),
        }
    });
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = execute_with_client(args(&["--all", "--pages", "1"]), &mut out, &mut err, |_| {
        Ok(server.client())
    });
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    assert!(err.is_empty());
    let decoded: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(decoded.as_array().unwrap().len(), 2);
    assert_eq!(decoded[0]["txnId"], "n");
    assert_eq!(decoded[1]["txnId"], "o");
    assert!(out.ends_with(b"]\n"));
    assert_eq!(calls.lock().unwrap().card, 4);
}

#[test]
fn poll_mode_skips_only_full_history_completeness_errors() {
    // Changed totalCount between pages, overshoot, and an empty page before
    // totalCount are fatal in a full fetch. They must not fail a partial one.
    // Each fetch uses a fresh fixture server (the fixture clock is stateful).
    let changed = || {
        fake(|category, _, number| {
            if category != CATEGORIES[0] {
                return page(json!(0), vec![]);
            }
            match number {
                1 => page(json!(100), vec![fixture("a", "1234", 10)]),
                2 => page(json!(101), vec![fixture("b", "1234", 20)]),
                _ => page(json!(3), vec![fixture("c", "1234", 30)]),
            }
        })
    };
    let (server, _) = changed();
    assert!(server.client().fetch(None, None, None).is_err());
    let (server, _) = changed();
    assert_eq!(server.client().fetch(None, Some(3), None).unwrap().len(), 3);

    let overshoot = || {
        fake(|category, _, _| {
            if category == CATEGORIES[0] {
                page(
                    json!(1),
                    vec![fixture("a", "1234", 10), fixture("b", "1234", 20)],
                )
            } else {
                page(json!(0), vec![])
            }
        })
    };
    let (server, _) = overshoot();
    assert!(server.client().fetch(None, None, None).is_err());
    let (server, _) = overshoot();
    assert_eq!(server.client().fetch(None, Some(1), None).unwrap().len(), 2);

    // Empty page although totalCount promises more.
    let empty_early = || {
        fake(|category, _, number| {
            if category == CATEGORIES[0] && number == 1 {
                page(json!(100), vec![fixture("a", "1234", 10)])
            } else if category == CATEGORIES[0] {
                page(json!(100), vec![])
            } else {
                page(json!(0), vec![])
            }
        })
    };
    let (server, _) = empty_early();
    assert!(server.client().fetch(None, None, None).is_err());
    let (server, calls) = empty_early();
    assert_eq!(server.client().fetch(None, Some(5), None).unwrap().len(), 1);
    // AUTH_ALL stops at its empty page 2; three other categories take one call.
    assert_eq!(calls.lock().unwrap().card, 5);
}

#[test]
fn poll_mode_keeps_record_validation_and_exact_duplicate_rule() {
    // Exact duplicates are kept once, in a page and across pages.
    let a = fixture("a", "1234", 10);
    let (server, _) = fake(move |category, _, _| {
        if category == CATEGORIES[0] {
            page(json!(100), vec![a.clone(), a.clone()])
        } else {
            page(json!(0), vec![])
        }
    });
    assert_eq!(server.client().fetch(None, Some(2), None).unwrap().len(), 1);

    // Malformed totalCount, missing card ending, bad timestamp, bad data list.
    for bad_page in [
        json!({"totalCount": "x", "data": []}),
        json!({"totalCount": -1, "data": []}),
        json!({"totalCount": 1, "data": {}}),
        json!({"totalCount": 1, "data": [{"txnCreate": 1}]}),
        json!({"totalCount": 1, "data": [{"pan4": "1234"}]}),
        json!({"totalCount": 1, "data": [fixture("a", "12x4", 1)]}),
        json!({"totalCount": 1, "data": [7]}),
    ] {
        let (server, _) = fake(move |_, _, _| bad_page.clone());
        assert!(server.client().fetch(None, Some(1), None).is_err());
    }

    // The card filter is still sent and re-checked.
    let (server, calls) = fake(|category, _, _| {
        if category == CATEGORIES[0] {
            page(json!(1), vec![fixture("a", "9999", 10)])
        } else {
            page(json!(0), vec![])
        }
    });
    let error = server
        .client()
        .fetch(Some("1234"), Some(1), None)
        .err()
        .unwrap();
    assert!(error.contains("different card"));
    assert_eq!(calls.lock().unwrap().filters[0].as_deref(), Some("1234"));
}

#[test]
fn since_option_parsing() {
    let parsed = cli::parse(args(&["--all", "--since", "1700000000000", "--pages", "3"])).unwrap();
    assert_eq!((parsed.since, parsed.pages), (Some(1700000000000), Some(3)));
    assert_eq!(
        cli::parse(args(&["0042", "--since", "0"])).unwrap().since,
        Some(0)
    );
    assert_eq!(cli::parse(args(&["--all"])).unwrap().since, None);
    assert_eq!(
        cli::parse(args(&["--since", "5", "--help"]))
            .unwrap()
            .action,
        Action::Help
    );
    for bad in [
        vec!["--all", "--since"],
        vec!["--all", "--since", "-1"],
        vec!["--all", "--since", "+1"],
        vec!["--all", "--since", "1.5"],
        vec!["--all", "--since", "x"],
        vec!["--all", "--since", ""],
        vec!["--all", "--since", "99999999999999999999999"],
        vec!["--all", "--since", "--config", "x"],
        vec!["--all", "--since", "1", "--since", "2"],
        vec!["--cards", "--since", "1"],
        vec!["--since", "1"],
    ] {
        assert!(cli::parse(args(&bad)).is_err(), "{bad:?}");
    }
}

/// AUTH_ALL: 3 pages of 50 events, newest first, stamps 1000 down to 851.
/// Other categories are empty. Returns the fixture server and call log.
fn descending_auth() -> (Server, Arc<Mutex<Calls>>) {
    fake(|category, _, number| {
        if category != CATEGORIES[0] {
            return page(json!(0), vec![]);
        }
        let first = 1000 - (number as i64 - 1) * 50;
        page(
            json!(150),
            (0..50)
                .map(|n| fixture(&format!("t{}", first - n), "1234", first - n))
                .collect(),
        )
    })
}

fn auth_pages(calls: &Arc<Mutex<Calls>>) -> usize {
    calls
        .lock()
        .unwrap()
        .categories
        .iter()
        .filter(|c| *c == CATEGORIES[0])
        .count()
}

#[test]
fn since_stops_after_the_first_page_with_an_older_event() {
    // Bound 960 lies inside page 1 (stamps 1000..951): keep 960..1000, 41 events,
    // and do not request page 2.
    let (server, calls) = descending_auth();
    let records = server.client().fetch(None, None, Some(960)).unwrap();
    assert_eq!(records.len(), 41);
    assert!(records.iter().all(|r| r.raw.get().contains("txnCreate")));
    assert_eq!(auth_pages(&calls), 1);
    assert_eq!(calls.lock().unwrap().card, 4);

    // Bound 940 lies inside page 2 (stamps 950..901): page 2 is needed, page 3 is not.
    let (server, calls) = descending_auth();
    let records = server.client().fetch(None, None, Some(940)).unwrap();
    assert_eq!(records.len(), 61);
    assert_eq!(auth_pages(&calls), 2);
}

#[test]
fn since_bound_is_inclusive_and_a_tie_at_the_bound_continues_to_the_next_page() {
    // Bound 951 is the oldest stamp of page 1. Nothing on page 1 is older, so
    // page 2 is read (a tie could continue there); its first event is older.
    let (server, calls) = descending_auth();
    let records = server.client().fetch(None, None, Some(951)).unwrap();
    assert_eq!(records.len(), 50);
    assert_eq!(auth_pages(&calls), 2);
    let stamps: Vec<i64> = records
        .iter()
        .map(|r| {
            serde_json::from_str::<Value>(r.raw.get()).unwrap()["txnCreate"]
                .as_i64()
                .unwrap()
        })
        .collect();
    assert_eq!(stamps.iter().min(), Some(&951));

    // A tie that continues over the page boundary is kept on both pages.
    let (server, calls) = fake(|category, _, number| {
        if category != CATEGORIES[0] {
            return page(json!(0), vec![]);
        }
        match number {
            1 => page(
                json!(120),
                (0..50)
                    .map(|n| fixture(&format!("p1-{n}"), "1234", 500))
                    .collect(),
            ),
            2 => page(
                json!(120),
                (0..50)
                    .map(|n| fixture(&format!("p2-{n}"), "1234", if n < 10 { 500 } else { 499 }))
                    .collect(),
            ),
            _ => page(json!(120), vec![fixture("p3", "1234", 1)]),
        }
    });
    let records = server.client().fetch(None, None, Some(500)).unwrap();
    assert_eq!(records.len(), 60);
    assert_eq!(auth_pages(&calls), 2);
}

#[test]
fn since_newer_than_everything_returns_nothing_and_old_bound_reads_all_pages() {
    let (server, calls) = descending_auth();
    assert!(server
        .client()
        .fetch(None, None, Some(5000))
        .unwrap()
        .is_empty());
    assert_eq!(auth_pages(&calls), 1);

    // A bound older than every event reads the whole category and stops at totalCount.
    let (server, calls) = descending_auth();
    assert_eq!(
        server.client().fetch(None, None, Some(0)).unwrap().len(),
        150
    );
    assert_eq!(auth_pages(&calls), 3);
}

#[test]
fn pages_cap_and_since_stop_at_whichever_comes_first() {
    // Cap first: bound 0 would read 3 pages, the cap stops at 2.
    let (server, calls) = descending_auth();
    assert_eq!(
        server.client().fetch(None, Some(2), Some(0)).unwrap().len(),
        100
    );
    assert_eq!(auth_pages(&calls), 2);

    // Bound first: page 1 holds an older event, so the cap of 3 is not reached.
    let (server, calls) = descending_auth();
    assert_eq!(
        server
            .client()
            .fetch(None, Some(3), Some(990))
            .unwrap()
            .len(),
        11
    );
    assert_eq!(auth_pages(&calls), 1);
}

#[test]
fn since_mode_keeps_validation_duplicates_and_skips_completeness_errors() {
    // Changed totalCount and overshoot do not fail; exact repeats are kept once.
    let a = fixture("a", "1234", 100);
    let (server, _) = fake(move |category, _, number| {
        if category != CATEGORIES[0] {
            return page(json!(0), vec![]);
        }
        match number {
            1 => page(json!(500), vec![a.clone(), a.clone()]),
            _ => page(
                json!(1),
                vec![fixture("b", "1234", 90), fixture("c", "1234", 80)],
            ),
        }
    });
    // Bound 85 keeps a (once) and b; c is older and ends the read on page 2.
    assert_eq!(
        server.client().fetch(None, None, Some(85)).unwrap().len(),
        2
    );

    // Validation still applies to events older than the bound.
    let (server, _) = fake(|category, _, _| {
        if category == CATEGORIES[0] {
            page(json!(1), vec![fixture("z", "12x4", 1)])
        } else {
            page(json!(0), vec![])
        }
    });
    assert!(server.client().fetch(None, None, Some(100)).is_err());
}

#[test]
fn since_end_to_end_output_keeps_format_and_sort() {
    let (server, _) = fake(|category, _, _| match category {
        "SIDE_QUERY_AUTH_ALL" => page(
            json!(3),
            vec![
                fixture("n", "1234", 300),
                fixture("m", "1234", 200),
                fixture("o", "1234", 100),
            ],
        ),
        "SIDE_QUERY_FINANCIAL_ALL" => page(json!(1), vec![fixture("f", "1234", 250)]),
        _ => page(json!(0), vec![]),
    });
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = execute_with_client(
        args(&["--all", "--since", "200", "--pages", "2"]),
        &mut out,
        &mut err,
        |_| Ok(server.client()),
    );
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let decoded: Value = serde_json::from_slice(&out).unwrap();
    let ids: Vec<&str> = decoded
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["txnId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["n", "f", "m"]);
    assert!(out.ends_with(b"]\n"));
}
