//! 正式 SSE 的有界采集；建立完成后才允许测试发输入，线程由本次测试拥有。

use std::{
    io::{self, Read},
    thread::{self, JoinHandle},
    time::Duration,
};

use serde_json::Value;

use crate::support::HostProcess;

const LIMIT: u64 = 2 * 1024 * 1024;

pub(super) struct EventCapture(Option<JoinHandle<io::Result<Vec<u8>>>>);

impl EventCapture {
    pub fn start(host: &HostProcess) -> Self {
        let response = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("SSE client")
            .get(format!("{}/events", host.base_url()))
            .bearer_auth(host.access_token())
            .send()
            .expect("SSE subscription");
        assert!(response.status().is_success());
        Self(Some(thread::spawn(move || {
            let mut bytes = Vec::new();
            response.take(LIMIT + 1).read_to_end(&mut bytes)?;
            assert!(
                (bytes.len() as u64) <= LIMIT,
                "SSE fixture capture exceeded limit"
            );
            Ok(bytes)
        })))
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.0
            .take()
            .expect("SSE owner")
            .join()
            .expect("SSE worker")
            .expect("read SSE before timeout")
    }
}

impl Drop for EventCapture {
    fn drop(&mut self) {
        // 失败展开时也不遗留 reader；请求自身的总超时保证兜底 join 有界。
        if let Some(owner) = self.0.take() {
            let _ = owner.join();
        }
    }
}

pub(super) fn assert_invocation_order(bytes: &[u8], allowed: bool) {
    let events = std::str::from_utf8(bytes)
        .expect("SSE UTF-8")
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str::<Value>(data).expect("SSE JSON")["event"].clone())
        .collect::<Vec<_>>();
    let resolved = events
        .iter()
        .position(|event| event["type"] == "approval_resolved")
        .expect("approval resolution SSE");
    let started = events
        .iter()
        .position(|event| event["type"] == "tool_started" && event["call_id"] == "m8-invoke");
    if allowed {
        assert!(resolved < started.expect("MCP started SSE"));
    } else {
        assert!(started.is_none(), "denied invocation cannot start");
    }
    let completed = events
        .iter()
        .position(|event| event["type"] == "tool_completed" && event["call_id"] == "m8-invoke")
        .expect("MCP completed SSE");
    assert!(resolved < completed);
    assert!(
        completed
            < events
                .iter()
                .position(|event| event["type"] == "run_finished")
                .expect("Run finished SSE")
    );
}
