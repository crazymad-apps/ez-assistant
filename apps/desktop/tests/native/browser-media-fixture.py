"""原生媒体关闭回归夹具；仅 loopback、无用户数据、无 Runtime。"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse
import io
import json
import math
import struct
import time
import wave

# 低音量测试音。HTML audio 与 Web Audio 同时运行，关闭后两者都必须终止。
buffer = io.BytesIO()
with wave.open(buffer, "wb") as output:
    output.setnchannels(1)
    output.setsampwidth(2)
    output.setframerate(16000)
    output.writeframes(b"".join(
        struct.pack("<h", int(1500 * math.sin(2 * math.pi * 330 * i / 16000)))
        for i in range(16000)
    ))
TONE = buffer.getvalue()
PAGE = b'''<!doctype html><meta charset="utf-8"><title>Audio close regression</title>
<button>Start audio + Web Audio</button><audio src="/tone.wav" loop></audio>
<script>
let context;
const id = new URL(location).searchParams.get('id');
const audio = document.querySelector('audio');
const report = event => fetch('/event', {method: 'POST', body: JSON.stringify({
  id, event, audio: !audio.paused, time: audio.currentTime,
  ctx: context?.state, clock: context?.currentTime
})}).catch(() => {});
document.querySelector('button').onclick = async () => {
  await audio.play();
  context = new AudioContext();
  const oscillator = context.createOscillator(), gain = context.createGain();
  oscillator.frequency.value = 440;
  gain.gain.value = .025;
  oscillator.connect(gain).connect(context.destination);
  oscillator.start();
  await context.resume();
  report('started');
};
setInterval(() => report('tick'), 500);
addEventListener('pagehide', () => navigator.sendBeacon('/event', JSON.stringify({id, event: 'pagehide'})));
</script>'''
LATEST = {}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        url = urlparse(self.path)
        kind = "text/html"
        data = PAGE
        if url.path == "/tone.wav":
            kind, data = "audio/wav", TONE
        elif url.path == "/shell":
            data = b"<title>Native media close probe</title>"
        elif url.path == "/status":
            page = parse_qs(url.query).get("id", [""])[0]
            kind, data = "application/json", json.dumps(LATEST.get(page, {})).encode()
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        entry = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        entry["at"] = time.time()
        LATEST[entry["id"]] = entry
        self.send_response(204)
        self.end_headers()


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    print(f"Fixture: http://127.0.0.1:{server.server_port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
