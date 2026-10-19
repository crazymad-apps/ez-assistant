//! 只把可合并的编码反馈投影给媒体 owner；NACK 留在各段拦截器内处理。
use rtc::{
    interceptor::{Attribute, Interceptor, Packet, StreamInfo, TaggedPacket},
    sansio::Protocol,
};
use std::{collections::VecDeque, time::Instant};
use tokio::sync::watch;

pub(super) const MAX_BITRATE: u32 = 8_000_000;

#[derive(Clone, Copy, Default)]
pub(super) struct Feedback {
    pub(super) keyframe: u64,
    pub(super) bitrate: u32,
}
pub(super) struct EncoderFeedback {
    state: watch::Sender<Feedback>,
    network_bitrate: u32,
    viewer_bitrate: u32,
    read: VecDeque<TaggedPacket>,
    write: VecDeque<TaggedPacket>,
}
impl EncoderFeedback {
    pub(super) fn new(state: watch::Sender<Feedback>) -> Self {
        Self {
            state,
            network_bitrate: MAX_BITRATE,
            viewer_bitrate: MAX_BITRATE,
            read: VecDeque::new(),
            write: VecDeque::new(),
        }
    }
}
impl Protocol<TaggedPacket, TaggedPacket, ()> for EncoderFeedback {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = rtc::shared::error::Error;
    type Time = Instant;
    fn handle_read(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        use rtc::rtcp::payload_feedbacks::{
            full_intra_request::FullIntraRequest, picture_loss_indication::PictureLossIndication,
            receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate,
        };
        let mut next = *self.state.borrow();
        if let Packet::Rtcp(packets) = &msg.message.packet {
            for packet in packets {
                if packet.as_any().is::<PictureLossIndication>()
                    || packet.as_any().is::<FullIntraRequest>()
                {
                    next.keyframe = next.keyframe.wrapping_add(1);
                }
                if let Some(remb) = packet
                    .as_any()
                    .downcast_ref::<ReceiverEstimatedMaximumBitrate>()
                {
                    self.viewer_bitrate = remb.bitrate.clamp(150_000.0, MAX_BITRATE as f32) as u32;
                }
            }
        }
        for attr in &msg.message.attributes {
            if let Attribute::TargetBitrateChanged { bits_per_second } = attr {
                self.network_bitrate =
                    bits_per_second.clamp(150_000.0, f64::from(MAX_BITRATE)) as u32;
            }
        }
        next.bitrate = self.network_bitrate.min(self.viewer_bitrate);
        self.state.send_if_modified(|state| {
            if state.keyframe == next.keyframe && state.bitrate == next.bitrate {
                false
            } else {
                *state = next;
                true
            }
        });
        if self.read.len() >= 64 {
            return Err(Self::Error::Other("desktop feedback queue full".into()));
        }
        self.read.push_back(msg);
        Ok(())
    }
    fn poll_read(&mut self) -> Option<TaggedPacket> {
        self.read.pop_front()
    }
    fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        if self.write.len() >= 64 {
            return Err(Self::Error::Other("desktop write queue full".into()));
        }
        self.write.push_back(msg);
        Ok(())
    }
    fn poll_write(&mut self) -> Option<TaggedPacket> {
        self.write.pop_front()
    }
}
impl Interceptor for EncoderFeedback {
    fn bind_local_stream(&mut self, _: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _: &StreamInfo) {}
}
