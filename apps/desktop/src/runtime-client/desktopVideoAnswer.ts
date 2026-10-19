/** 浏览器的默认 H.264 SDP 常只声明 Level 3.1；仅在实际解码能力查询通过后提高接收档位。 */
export async function desktopVideoAnswer(answer: RTCSessionDescriptionInit): Promise<RTCSessionDescriptionInit | null> {
  if (!answer.sdp || !navigator.mediaCapabilities?.decodingInfo) return null;
  const lines = answer.sdp.split("\r\n");
  let video = false;
  let receiveOnly = false;
  let codecLine = -1;
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index]!;
    if (line.startsWith("m=")) video = line.startsWith("m=video ") && !line.startsWith("m=video 0 ");
    if (!video) continue;
    if (line === "a=recvonly") receiveOnly = true;
    if (/^a=fmtp:\d+ /.test(line) && /(?:[ ;])profile-level-id=42e0[0-9a-f]{2}(?:;|$)/i.test(line)
      && /(?:[ ;])packetization-mode=1(?:;|$)/.test(line)) codecLine = index;
  }
  if (!receiveOnly || codecLine < 0) return null;
  try {
    // 同一能力覆盖横屏与竖屏；不以文件解码能力冒充 WebRTC 解码能力。
    for (const [width, height] of [[1920, 1440], [1440, 1920]] as const) {
      const capability = await navigator.mediaCapabilities.decodingInfo({
        type: "webrtc",
        video: { contentType: "video/H264;level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e032", width, height, bitrate: 8_000_000, framerate: 30 },
      });
      if (!capability.supported) return null;
    }
  } catch { return null; }
  lines[codecLine] = lines[codecLine]!.replace(/profile-level-id=42e0[0-9a-f]{2}/i, "profile-level-id=42e032");
  return { ...answer, sdp: lines.join("\r\n") };
}
