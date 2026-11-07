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
        "repeated-record",
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
                "repeated-record" => page(json!(2), vec![record]),
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
