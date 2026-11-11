//! `agendar update --check` against a loopback stand-in for the GitHub API, and
//! `agendar status` reading only the cache. No test reaches the network.

mod support;

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use agendar_core::update::{Channel, URL_ENV};

use support::{CliEnv, expect_error, expect_success};

const FIXTURE: &str = include_str!("../../agendar-core/tests/fixtures/github-release-latest.json");
const ETAG: &str = "W/\"fixture-1\"";

/// The header lines of every request a stand-in server answered.
type Seen = Arc<Mutex<Vec<Vec<String>>>>;

/// Serve `connections` requests: the fixture with an ETag, or 304 when the
/// request sends that ETag back. Every request's header lines are recorded.
fn serve(connections: usize) -> (String, Seen, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!(
        "http://127.0.0.1:{}/repos/Maolon/agendar/releases/latest",
        listener.local_addr().expect("addr").port()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let handle = std::thread::spawn(move || {
        for _ in 0..connections {
            let (stream, _) = listener.accept().expect("accept");
            answer(stream, &record);
        }
    });
    (url, seen, handle)
}

fn answer(mut stream: TcpStream, seen: &Mutex<Vec<Vec<String>>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).expect("read request") == 0 {
            break;
        }
        let line = line.trim_end().to_owned();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let revalidated = lines.iter().any(|line| {
        line.to_ascii_lowercase() == format!("if-none-match: {}", ETAG.to_ascii_lowercase())
    });
    seen.lock().expect("record").push(lines);
    let response = if revalidated {
        format!("HTTP/1.1 304 Not Modified\r\nETag: {ETAG}\r\nConnection: close\r\n\r\n")
    } else {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: {ETAG}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{FIXTURE}",
            FIXTURE.len()
        )
    };
    stream.write_all(response.as_bytes()).expect("respond");
}

/// Run with the endpoint override and no proxy, so loopback stays loopback.
fn run(env: &CliEnv, args: &[&str], vars: &[(&str, &str)]) -> std::process::Output {
    let mut command = env.command();
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env_remove(key);
    }
    command
        .arg("--vault")
        .arg(&env.vault)
        .arg("--data-dir")
        .arg(&env.data)
        .arg("--project")
        .arg(&env.project);
    if !env.session.is_empty() {
        command.arg("--session").arg(&env.session);
    }
    command.args(args).envs(vars.iter().copied());
    command.output().expect("run agendar")
}

fn header<'a>(lines: &'a [String], name: &str) -> Option<&'a str> {
    let prefix = format!("{}:", name.to_ascii_lowercase());
    lines.iter().find_map(|line| {
        line.to_ascii_lowercase()
            .starts_with(&prefix)
            .then(|| line[prefix.len()..].trim())
    })
}

#[test]
fn update_check_reports_the_latest_release_and_revalidates_with_its_etag() {
    let env = CliEnv::new("demo");
    let (url, seen, server) = serve(2);

    let first = expect_success(
        &run(&env, &["update", "--check"], &[(URL_ENV, &url)]),
        "update",
    );
    let data = &first["data"];
    assert_eq!(first["command"], "update");
    assert_eq!(data["current"], env!("CARGO_PKG_VERSION"));
    assert_eq!(data["latest"], "0.2.0");
    assert_eq!(
        data["url"],
        "https://github.com/Maolon/agendar/releases/tag/v0.2.0"
    );
    assert_eq!(data["published_at"], "2026-11-01T10:00:00Z");
    assert_eq!(data["notes"], "## Features\n\n* update detection");
    let release = Channel::of_build() == Channel::Release;
    assert_eq!(data["channel"], if release { "release" } else { "dev" });
    // A test build is a dev build: never told to update, whatever the version.
    assert_eq!(
        data["available"],
        release
            && agendar_core::update::is_available(
                Channel::Release,
                env!("CARGO_PKG_VERSION"),
                "0.2.0"
            )
    );
    let checked = data["checked_at"].as_str().expect("checked_at").to_owned();
    assert!(env.data.join("update-check.json").is_file());

    let second = expect_success(&run(&env, &["update"], &[(URL_ENV, &url)]), "update");
    assert_eq!(second["data"]["latest"], "0.2.0", "a 304 keeps the release");
    assert!(second["data"]["checked_at"].as_str().expect("checked_at") >= checked.as_str());
    server.join().expect("server");

    let requests = seen.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    let user_agent = header(&requests[0], "user-agent").expect("user agent");
    assert_eq!(user_agent, format!("agendar/{}", env!("CARGO_PKG_VERSION")));
    assert_eq!(
        header(&requests[0], "accept"),
        Some("application/vnd.github+json")
    );
    assert_eq!(header(&requests[0], "if-none-match"), None);
    assert_eq!(header(&requests[1], "if-none-match"), Some(ETAG));
}

#[test]
fn status_reads_the_cached_check_and_never_the_network() {
    let mut env = CliEnv::new("demo");
    env.init();
    let (url, _, server) = serve(1);
    expect_success(
        &run(&env, &["update", "--check"], &[(URL_ENV, &url)]),
        "update",
    );
    server.join().expect("server");

    // A listener that fails the test if anything connects to it.
    let trap = TcpListener::bind("127.0.0.1:0").expect("bind trap");
    let trap_url = format!(
        "http://127.0.0.1:{}/latest",
        trap.local_addr().expect("addr").port()
    );
    let status = expect_success(&run(&env, &["status"], &[(URL_ENV, &trap_url)]), "status");
    trap.set_nonblocking(true).expect("nonblocking");
    assert!(
        matches!(trap.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "status must not reach the network"
    );
    let update = &status["data"]["update"];
    assert_eq!(update["latest"], "0.2.0");
    assert_eq!(update["current"], env!("CARGO_PKG_VERSION"));
    assert!(update.get("notes").is_none(), "status leaves the notes out");
}

#[test]
fn an_unreachable_server_is_a_retryable_io_error_and_keeps_no_cache() {
    let env = CliEnv::new("demo");
    // Bind then drop: nothing listens on this port any more.
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let url = format!("http://127.0.0.1:{port}/latest");
    let error = expect_error(
        &run(&env, &["update", "--check"], &[(URL_ENV, &url)]),
        "E_IO",
        5,
    );
    assert_eq!(error["error"]["retryable"], true);
    assert!(!env.data.join("update-check.json").exists());

    let refused = expect_error(
        &run(
            &env,
            &["update", "--check"],
            &[(URL_ENV, "http://example.com/latest")],
        ),
        "E_USAGE",
        2,
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(URL_ENV)
    );
}
