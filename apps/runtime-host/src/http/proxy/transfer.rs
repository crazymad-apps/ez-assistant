//! 单个 HTTP 流的双向收发；发送 future 由 HTTP 响应持有，取消时一起释放。
use super::*;
use std::{future::Future, pin::Pin, sync::Arc};

pub(super) type Upload = Option<Pin<Box<dyn Future<Output = Result<(), StreamError>> + Send>>>;

pub(super) fn upload(flow: Arc<Flow>, body: Body, length: Option<u64>) -> Upload {
    Some(Box::pin(async move {
        let mut body = body.into_data_stream();
        let mut sent = 0u64;
        while let Some(chunk) = timeout(Duration::from_secs(30), body.next())
            .await
            .map_err(|_| StreamError::Timeout)?
        {
            let chunk = chunk.map_err(|_| StreamError::Ended)?;
            sent = sent
                .checked_add(chunk.len() as u64)
                .ok_or(StreamError::Invalid)?;
            if length.is_some_and(|n| sent > n) {
                return Err(StreamError::Invalid);
            }
            flow.bytes(&chunk).await?;
        }
        if length.is_some_and(|n| sent != n) {
            return Err(StreamError::Invalid);
        }
        flow.send(Frame {
            kind: 4,
            payload: vec![],
        })
        .await
    }))
}

/// 接收响应的同时推进上传，允许目标在完整请求到达前拒绝；没有游离发送任务。
pub(super) async fn receive(flow: &Flow, upload: &mut Upload) -> Result<Frame, StreamError> {
    if let Some(send) = upload {
        tokio::select! {
            biased;
            frame = flow.recv() => return frame,
            result = send => { result?; *upload = None; }
        }
    }
    flow.recv().await
}

pub(super) async fn response_head(
    flow: &Flow,
    upload: &mut Upload,
) -> Result<ResponseHead, StreamError> {
    // 大文件不受整个请求 60 秒限制；上传各次读取和信用等待各自有界。
    let frame = if let Some(send) = upload {
        tokio::select! {
            frame = flow.recv() => frame?,
            result = send => {
                result?;
                *upload = None;
                timeout(Duration::from_secs(60), flow.recv()).await.map_err(|_| StreamError::Timeout)??
            }
        }
    } else {
        timeout(Duration::from_secs(60), flow.recv())
            .await
            .map_err(|_| StreamError::Timeout)??
    };
    if frame.kind != 2 {
        return Err(StreamError::Invalid);
    }
    serde_json::from_slice(&frame.payload).map_err(|_| StreamError::Invalid)
}

pub(super) fn incoming(
    flow: Arc<Flow>,
    length: Option<u64>,
    mut upload: Upload,
    idle_timeout: bool,
) -> Body {
    let stream = async_stream::try_stream! {
        let mut received = 0u64;
        // HTTP 消费者读足 Content-Length 后可能不再轮询；最后一字节须等 FIN 后交付。
        let mut final_byte = None;
        loop {
            let receive = receive(&flow, &mut upload);
            let mut frame = if idle_timeout {
                timeout(Duration::from_secs(30), receive).await.map_err(|_| StreamError::Timeout)??
            } else { receive.await? };
            if frame.kind == 4 {
                if length.is_some_and(|n| n != received) { Err(StreamError::Invalid)?; }
                if let Some(byte) = final_byte { yield Bytes::copy_from_slice(&[byte]); }
                // 完整响应到达即可结束；提前拒绝时丢弃尚未完成的上传，由 Flow Drop reset 收尾。
                break;
            }
            if frame.kind != 3 { Err(StreamError::Invalid)?; }
            received = received.checked_add(frame.payload.len() as u64).ok_or(StreamError::Invalid)?;
            if length.is_some_and(|n| received > n) { Err(StreamError::Invalid)?; }
            if length == Some(received) && !frame.payload.is_empty() { final_byte = frame.payload.pop(); }
            if !frame.payload.is_empty() { yield Bytes::from(frame.payload); }
        }
    };
    let stream: Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, StreamError>> + Send>> =
        Box::pin(stream);
    Body::from_stream(stream)
}
