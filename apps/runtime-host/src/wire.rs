//! Runtime Host 私有的 length-prefixed JSON wire。

use std::io::ErrorKind;

use agent_types::ConversationSnapshot;
use assistant_protocol::{
    RuntimeCommand, RuntimeCommandResult, RuntimeErrorInfo, RuntimeEvent, SessionId,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ClientFrame {
    Hello {
        protocol_version: u32,
        client_name: String,
    },
    Request {
        request_id: String,
        command: HostCommand,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
pub(crate) enum HostCommand {
    Runtime(RuntimeCommand),
    ConversationSnapshot { session_id: SessionId },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServerFrame {
    HelloAck {
        protocol_version: u32,
        runtime_version: String,
    },
    Response {
        request_id: String,
        result: HostCommandResult,
    },
    Error {
        request_id: String,
        error: RuntimeErrorInfo,
    },
    Event {
        event: RuntimeEvent,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "scope", content = "payload", rename_all = "snake_case")]
pub(crate) enum HostCommandResult {
    Runtime(RuntimeCommandResult),
    ConversationSnapshot { conversation: ConversationSnapshot },
}

#[derive(Debug, Error)]
pub(crate) enum WireError {
    #[error("frame I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame length must be between 1 and {MAX_FRAME_BYTES} bytes, got {actual}")]
    InvalidLength { actual: usize },
    #[error("frame JSON is invalid: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

pub(crate) async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, WireError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0_u8; 4];
    let read = reader.read(&mut header[..1]).await?;
    if read == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..]).await.map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            std::io::Error::new(ErrorKind::UnexpectedEof, "truncated frame header")
        } else {
            error
        }
    })?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(WireError::InvalidLength { actual: length });
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await.map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            std::io::Error::new(ErrorKind::UnexpectedEof, "truncated frame payload")
        } else {
            error
        }
    })?;
    Ok(Some(serde_json::from_slice(&payload)?))
}

pub(crate) async fn write_frame<W, T>(writer: &mut W, frame: &T) -> Result<(), WireError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(frame)?;
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(WireError::InvalidLength {
            actual: payload.len(),
        });
    }
    let length = u32::try_from(payload.len()).map_err(|_| WireError::InvalidLength {
        actual: payload.len(),
    })?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use assistant_protocol::{ListSessionsRequest, RuntimeCommand};
    use tokio::io::{AsyncWriteExt, duplex};

    use super::*;

    #[tokio::test]
    async fn frame_round_trips_json_with_newlines() {
        let (mut writer, mut reader) = duplex(4096);
        let frame = ClientFrame::Request {
            request_id: "request-1".to_owned(),
            command: HostCommand::Runtime(RuntimeCommand::ListSessions(
                ListSessionsRequest::default(),
            )),
        };
        write_frame(&mut writer, &frame).await.expect("write frame");
        assert_eq!(
            read_frame::<_, ClientFrame>(&mut reader)
                .await
                .expect("read frame"),
            Some(frame)
        );
    }

    #[tokio::test]
    async fn clean_eof_is_distinct_from_truncated_header_and_payload() {
        let (writer, mut reader) = duplex(16);
        drop(writer);
        assert!(
            read_frame::<_, ClientFrame>(&mut reader)
                .await
                .expect("clean eof")
                .is_none()
        );

        let (mut writer, mut reader) = duplex(16);
        writer.write_all(&[0, 0]).await.expect("partial header");
        drop(writer);
        assert!(matches!(
            read_frame::<_, ClientFrame>(&mut reader).await,
            Err(WireError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
        ));

        let (mut writer, mut reader) = duplex(16);
        writer
            .write_all(&4_u32.to_be_bytes())
            .await
            .expect("header");
        writer.write_all(b"{}").await.expect("partial payload");
        drop(writer);
        assert!(matches!(
            read_frame::<_, ClientFrame>(&mut reader).await,
            Err(WireError::Io(error)) if error.kind() == ErrorKind::UnexpectedEof
        ));
    }

    #[tokio::test]
    async fn zero_and_oversized_lengths_are_rejected_before_allocation() {
        for length in [0, MAX_FRAME_BYTES + 1] {
            let (mut writer, mut reader) = duplex(16);
            writer
                .write_all(&(length as u32).to_be_bytes())
                .await
                .expect("header");
            assert!(matches!(
                read_frame::<_, ClientFrame>(&mut reader).await,
                Err(WireError::InvalidLength { actual }) if actual == length
            ));
        }
    }

    #[tokio::test]
    async fn invalid_json_is_rejected_without_panicking() {
        let (mut writer, mut reader) = duplex(64);
        writer
            .write_all(&5_u32.to_be_bytes())
            .await
            .expect("header");
        writer.write_all(b"nope!").await.expect("payload");
        assert!(matches!(
            read_frame::<_, ClientFrame>(&mut reader).await,
            Err(WireError::InvalidJson(_))
        ));
    }
}
