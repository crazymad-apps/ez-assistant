//! 同屏采集由 Supervisor 单独持有；查看者只持有有界订阅，不互相等待。
use super::{
    MediaError,
    capture::{Capture, Frame},
    service::DesktopViews,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{broadcast, watch},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(super) struct Captures(Mutex<HashMap<String, Entry>>);
struct Entry {
    source: Arc<Source>,
    users: usize,
}
struct Source {
    binding: String,
    frames: broadcast::Sender<Arc<Frame>>,
    keyframe: watch::Sender<u64>,
    cancel: CancellationToken,
    stopped: CancellationToken,
    error: watch::Sender<Option<String>>,
}
pub(super) struct Subscription {
    owner: Arc<DesktopViews>,
    display: String,
    source: Arc<Source>,
    frames: broadcast::Receiver<Arc<Frame>>,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        let cancel = {
            let mut entries = self
                .owner
                .captures
                .0
                .lock()
                .expect("desktop captures poisoned");
            entries.get_mut(&self.display).and_then(|entry| {
                if !Arc::ptr_eq(&entry.source, &self.source) {
                    return None;
                }
                entry.users -= 1;
                (entry.users == 0).then(|| entry.source.cancel.clone())
            })
        };
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
    }
}
impl Subscription {
    pub(super) fn request_keyframe(&self) {
        self.source
            .keyframe
            .send_modify(|value| *value = value.wrapping_add(1));
    }
    pub(super) fn ended(&self) -> impl std::future::Future<Output = MediaError> + use<> {
        let source = self.source.clone();
        async move {
            source.stopped.cancelled().await;
            MediaError::Unavailable(
                source
                    .error
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| "capture_interrupted".into()),
            )
        }
    }
    pub(super) async fn next_frame(&mut self) -> Result<Arc<Frame>, MediaError> {
        self.frames.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Lagged(_) => {
                MediaError::Unavailable("desktop_slow_viewer".into())
            }
            broadcast::error::RecvError::Closed => MediaError::Ended,
        })
    }
}
impl DesktopViews {
    /// 只在注册/释放订阅时持锁；原生启动、读取和回收都在受监督任务中执行。
    pub(super) async fn capture(
        self: &Arc<Self>,
        display: &str,
        binding: &str,
    ) -> Result<Subscription, MediaError> {
        loop {
            let stopping = {
                let mut entries = self.captures.0.lock().expect("desktop captures poisoned");
                if let Some(entry) = entries.get_mut(display) {
                    if entry.users == 0 || entry.source.cancel.is_cancelled() {
                        Some(entry.source.stopped.clone())
                    } else {
                        if entry.source.binding != binding {
                            return Err(MediaError::Unavailable("display_changed".into()));
                        }
                        entry.users += 1;
                        let source = entry.source.clone();
                        let frames = source.frames.subscribe();
                        let subscription = Subscription {
                            owner: self.clone(),
                            display: display.into(),
                            source,
                            frames,
                        };
                        drop(entries);
                        subscription.request_keyframe();
                        return Ok(subscription);
                    }
                } else {
                    if entries.len() >= 2 {
                        return Err(MediaError::Unavailable("desktop_busy".into()));
                    }
                    let (frames, receiver) = broadcast::channel(4);
                    let (keyframe, requests) = watch::channel(0);
                    let (error, _) = watch::channel(None);
                    let source = Arc::new(Source {
                        binding: binding.into(),
                        frames,
                        keyframe,
                        cancel: self.shutdown.child_token(),
                        stopped: CancellationToken::new(),
                        error,
                    });
                    // Guard 随任务移动；任务异常退出同样撤销订阅，移除占用，唤醒等待者。
                    let guard = CaptureTask {
                        owner: self.clone(),
                        display: display.into(),
                        source: source.clone(),
                    };
                    entries.insert(
                        display.into(),
                        Entry {
                            source: source.clone(),
                            users: 1,
                        },
                    );
                    drop(entries);
                    self.submit(Box::pin(async move { guard.run(requests).await }))?;
                    return Ok(Subscription {
                        owner: self.clone(),
                        display: display.into(),
                        source,
                        frames: receiver,
                    });
                }
            };
            if let Some(stopped) = stopping {
                timeout(Duration::from_secs(5), stopped.cancelled())
                    .await
                    .map_err(|_| MediaError::StopTimeout)?;
            }
        }
    }
}
struct CaptureTask {
    owner: Arc<DesktopViews>,
    display: String,
    source: Arc<Source>,
}
impl Drop for CaptureTask {
    fn drop(&mut self) {
        self.source.cancel.cancel();
        self.owner
            .captures
            .0
            .lock()
            .expect("desktop captures poisoned")
            .remove(&self.display);
        self.source.stopped.cancel();
    }
}
impl CaptureTask {
    async fn run(&self, requests: watch::Receiver<u64>) -> Result<(), MediaError> {
        let result = self.run_helper(&super::helper_path()?, requests).await;
        if let Err(error) = &result {
            let code = match error {
                MediaError::Unavailable(code) => code.clone(),
                _ => "capture_interrupted".into(),
            };
            self.source.error.send_replace(Some(code));
        }
        result
    }
    async fn run_helper(
        &self,
        helper: &std::path::Path,
        requests: watch::Receiver<u64>,
    ) -> Result<(), MediaError> {
        if self.source.cancel.is_cancelled() {
            return Ok(());
        }
        // 启动本身有界；即使最后一人此时离开，也等待启动结果再显式回收子进程。
        let mut capture = Capture::start_bound(
            helper,
            &self.display,
            [1920, 1440, 30],
            Some(&self.source.binding),
        )
        .await?;
        let input = capture.take_control()?;
        let result = tokio::select! { biased;
            () = self.source.cancel.cancelled() => Ok(()),
            result = self.frames(&mut capture) => result,
            result = keyframes(input, requests) => result,
        };
        result.and(capture.stop().await)
    }
    async fn frames(&self, capture: &mut Capture) -> Result<(), MediaError> {
        loop {
            let frame = capture.next_frame().await?;
            // Arc 只复制句柄；每个查看者独立读取，不等待慢用户，也不复制整帧缓存。
            if self.source.frames.send(Arc::new(frame)).is_err() {
                return Ok(());
            }
        }
    }
}
async fn keyframes(
    mut input: tokio::process::ChildStdin,
    mut requests: watch::Receiver<u64>,
) -> Result<(), MediaError> {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if requests.has_changed().map_err(|_| MediaError::Ended)? {
            requests.borrow_and_update();
            timeout(Duration::from_millis(500), input.write_all(b"keyframe\n"))
                .await
                .map_err(|_| MediaError::Ended)??;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::service::DesktopService;
    use super::*;

    #[tokio::test]
    async fn same_screen_shares_frames_and_slow_viewer_does_not_cancel_others() {
        let service = DesktopService::new().unwrap();
        let owner = service.handle.clone();
        let mut fast = owner.capture("main", "geometry").await.unwrap();
        let mut slow = owner.capture("main", "geometry").await.unwrap();
        assert!(Arc::ptr_eq(&fast.source, &slow.source));
        assert_eq!(owner.captures.0.lock().unwrap().len(), 1);
        for timestamp_us in 0..8 {
            let frame = Arc::new(Frame {
                data: vec![0, 0, 0, 1, 0x65],
                keyframe: true,
                timestamp_us,
            });
            assert!(fast.source.frames.send(frame.clone()).is_ok());
            assert!(Arc::ptr_eq(&frame, &fast.next_frame().await.unwrap()));
        }
        assert!(
            matches!(slow.next_frame().await, Err(MediaError::Unavailable(code)) if code == "desktop_slow_viewer")
        );
        drop(slow);
        assert!(!fast.source.cancel.is_cancelled());
        let source = fast.source.clone();
        drop(fast);
        assert!(source.cancel.is_cancelled());
        drop(service); // 未启动的监督任务也有 owner，丢弃后清除占用并唤醒订阅。
        assert!(source.stopped.is_cancelled());
        assert!(owner.captures.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn active_screen_limit_and_changed_geometry_do_not_start_extra_capture() {
        let service = DesktopService::new().unwrap();
        let owner = service.handle.clone();
        let a = owner.capture("a", "old").await.unwrap();
        let b = owner.capture("b", "other").await.unwrap();
        assert!(!Arc::ptr_eq(&a.source, &b.source));
        assert!(
            matches!(owner.capture("c", "third").await, Err(MediaError::Unavailable(code)) if code == "desktop_busy")
        );
        assert!(
            matches!(owner.capture("a", "new").await, Err(MediaError::Unavailable(code)) if code == "display_changed")
        );
        assert_eq!(owner.captures.0.lock().unwrap().len(), 2);
        drop((a, b));
        drop(service);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn last_viewer_cancels_and_waits_for_native_process() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.runtime-test");
        std::fs::create_dir_all(&root).unwrap();
        let directory = tempfile::Builder::new()
            .prefix("desktop-shared-")
            .tempdir_in(&root)
            .unwrap();
        let helper = directory.path().join("capture");
        let stopped = directory.path().join("stopped");
        let started = directory.path().join("started");
        std::fs::write(&helper, format!("#!/bin/sh\nprintf '%s\\n' '{{\"type\":\"configured\",\"codec\":\"h264\",\"width\":1920,\"height\":1080,\"fps\":30,\"hardware\":true}}' >&2\ntouch '{}'\nwhile IFS= read -r line; do :; done\ntouch '{}'\n", started.display(), stopped.display())).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = DesktopService::new().unwrap();
        let owner = service.handle.clone();
        let (frames, receiver) = broadcast::channel(4);
        let (keyframe, requests) = watch::channel(0);
        let (error, _) = watch::channel(None);
        let source = Arc::new(Source {
            binding: "geometry".into(),
            frames,
            keyframe,
            cancel: owner.shutdown.child_token(),
            stopped: CancellationToken::new(),
            error,
        });
        owner.captures.0.lock().unwrap().insert(
            "main".into(),
            Entry {
                source: source.clone(),
                users: 1,
            },
        );
        let first = Subscription {
            owner: owner.clone(),
            display: "main".into(),
            source: source.clone(),
            frames: receiver,
        };
        let second = owner.capture("main", "geometry").await.unwrap();
        let guard = CaptureTask {
            owner: owner.clone(),
            display: "main".into(),
            source,
        };
        let running = tokio::spawn(async move { guard.run_helper(&helper, requests).await });
        timeout(Duration::from_secs(2), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(first);
        assert!(!running.is_finished());
        assert!(!stopped.exists());
        drop(second);
        timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(stopped.exists());
        assert!(owner.captures.0.lock().unwrap().is_empty());
    }
    #[tokio::test]
    #[ignore = "requires an explicitly supplied signed native helper and an interactive desktop"]
    async fn native_shared_capture_late_viewer_and_last_close() {
        let helper = std::path::PathBuf::from(
            std::env::var("EZ_DESKTOP_TEST_HELPER").expect("explicit helper path"),
        );
        let catalog = super::super::capture::displays(&helper).await.unwrap();
        let display = catalog
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["primary"] == true)
            .unwrap();
        let id = display["id"].as_str().unwrap();
        let binding = display["binding"].as_str().unwrap();
        let service = DesktopService::new().unwrap();
        let owner = service.handle.clone();
        let (frames, receiver) = broadcast::channel(4);
        let (keyframe, requests) = watch::channel(0);
        let (error, _) = watch::channel(None);
        let source = Arc::new(Source {
            binding: binding.into(),
            frames,
            keyframe,
            cancel: owner.shutdown.child_token(),
            stopped: CancellationToken::new(),
            error,
        });
        owner.captures.0.lock().unwrap().insert(
            id.into(),
            Entry {
                source: source.clone(),
                users: 1,
            },
        );
        let mut first = Subscription {
            owner: owner.clone(),
            display: id.into(),
            source: source.clone(),
            frames: receiver,
        };
        let guard = CaptureTask {
            owner: owner.clone(),
            display: id.into(),
            source,
        };
        let running = tokio::spawn(async move { guard.run_helper(&helper, requests).await });
        let initial = timeout(Duration::from_secs(10), first.next_frame())
            .await
            .unwrap()
            .unwrap();
        assert!(initial.keyframe);
        let mut late = owner.capture(id, binding).await.unwrap();
        let mut late_keyframe = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        let mut count = 0;
        while !late_keyframe {
            let (a, b) = timeout(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                async { tokio::join!(first.next_frame(), late.next_frame()) },
            )
            .await
            .unwrap();
            let mut a = a.unwrap();
            let b = b.unwrap();
            while a.timestamp_us < b.timestamp_us {
                a = first.next_frame().await.unwrap();
            }
            assert!(Arc::ptr_eq(&a, &b));
            late_keyframe = b.keyframe;
            count += 1;
        }
        drop(first);
        assert!(!running.is_finished());
        late.request_keyframe();
        timeout(Duration::from_secs(4), late.next_frame())
            .await
            .unwrap()
            .unwrap();
        drop(late);
        timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(owner.captures.0.lock().unwrap().is_empty());
        eprintln!(
            "shared native capture: late viewer received keyframe after {count} frames; last viewer released source"
        );
    }
}
