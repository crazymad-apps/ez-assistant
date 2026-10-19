// Host 私有采集能力子进程：stdin 为控制，stdout 为有界 H.264 帧，stderr 为状态。
// 不持有企业身份、不监听端口、不注入输入；关闭 stdin 即撤销本次采集。
import AppKit
import ScreenCaptureKit
import VideoToolbox

struct CaptureFailure: Error {
    let code: String
    let status: Int
    init(_ code: String, _ status: Int = 0) { self.code = code; self.status = status }
}

func check(_ status: OSStatus, _ code: String) throws {
    if status != noErr { throw CaptureFailure(code, Int(status)) }
}

func report(_ value: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) else { return }
    // 消息只有有限的设备元数据／错误码，绝不记录像素、凭据或窗口标题。
    try? FileHandle.standardError.write(contentsOf: data + Data([10]))
}

// 写管道不允许无限等待；停止与父进程 EOF 在独立队列上观察。
final class Control {
    private let lock = NSLock()
    private var ended = false
    private var idr = true
    private var bitrate: Int?
    var stopped: Bool { lock.lock(); defer { lock.unlock() }; return ended }
    func stop() {
        lock.lock(); let first = !ended; ended = true; lock.unlock()
        if first {
            // 原生驱动即使卡在关闭中也不能遗留采集子进程；OS 回收其资源。
            DispatchQueue.global().asyncAfter(deadline: .now() + 2) { _exit(2) }
        }
    }
    var needsKeyframe: Bool { lock.lock(); defer { lock.unlock() }; return idr }
    func keyframe() { lock.lock(); idr = true; lock.unlock() }
    func takeKeyframe() -> Bool { lock.lock(); defer { lock.unlock() }; let value = idr; idr = false; return value }
    func setBitrate(_ value: Int) { lock.lock(); bitrate = value; lock.unlock() }
    func takeBitrate() -> Int? { lock.lock(); defer { lock.unlock() }; let value = bitrate; bitrate = nil; return value }
    func read() {
        DispatchQueue.global().async {
            var line = [UInt8]()
            var byte: UInt8 = 0
            while Darwin.read(STDIN_FILENO, &byte, 1) == 1 {
                if byte == 10 {
                    let command = String(bytes: line, encoding: .utf8)
                    if command == "keyframe" { self.keyframe() }
                    else if let command, command.hasPrefix("bitrate "), let value = Int(command.dropFirst(8)), (150_000...8_000_000).contains(value) { self.setBitrate(value) }
                    else { self.stop(); return }
                    line.removeAll(keepingCapacity: true)
                } else {
                    line.append(byte)
                    if line.count > 32 { self.stop(); return }
                }
            }
            self.stop()
        }
    }
}

func writePacket(_ data: Data, control: Control) throws {
    let deadline = DispatchTime.now().uptimeNanoseconds + 1_000_000_000
    try data.withUnsafeBytes { raw in
        guard let base = raw.baseAddress else { return }
        var offset = 0
        while offset < raw.count {
            if control.stopped { throw CaptureFailure("cancelled") }
            let count = Darwin.write(STDOUT_FILENO, base.advanced(by: offset), raw.count - offset)
            if count > 0 { offset += count; continue }
            if count < 0 && errno == EINTR { continue }
            if count < 0 && (errno == EAGAIN || errno == EWOULDBLOCK) {
                if DispatchTime.now().uptimeNanoseconds >= deadline { throw CaptureFailure("consumer_stalled") }
                var fd = pollfd(fd: STDOUT_FILENO, events: Int16(POLLOUT), revents: 0)
                _ = poll(&fd, 1, 20)
                continue
            }
            throw CaptureFailure("output_closed", Int(errno))
        }
    }
}

// 同一签名用于枚举、采集和输入；屏幕布局／缩放／旋转变化使旧绑定失效。
func displayBinding(_ id: CGDirectDisplayID) -> String {
    let r = CGDisplayBounds(id), mode = CGDisplayCopyDisplayMode(id)
    return "\(r.origin.x),\(r.origin.y),\(r.width),\(r.height),\(mode?.pixelWidth ?? 0),\(mode?.pixelHeight ?? 0),\(CGDisplayRotation(id))"
}

@available(macOS 13.0, *)
final class Capture: NSObject, SCStreamOutput, SCStreamDelegate {
    let control: Control
    // 单采集串行队列，最多两个在途帧。满时在编码前丢原始帧，不破坏 H.264 参考链。
    let queue = DispatchQueue(label: "ez.desktop.capture")
    let pending = DispatchSemaphore(value: 2)
    var stream: SCStream?
    var encoder: VTCompressionSession?
    var fps = 30
    var lastBuffer: CVPixelBuffer?
    var lastPTS = CMTime.invalid
    var refresh: DispatchSourceTimer?
    init(_ control: Control) { self.control = control }

    func start(displayID: UInt32?, windowID: UInt32?, width: Int, height: Int, fps: Int, binding: String?) async throws {
        guard CGPreflightScreenCaptureAccess() else { throw CaptureFailure("screen_permission_required") }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let filter: SCContentFilter
        let size: CGSize
        if let windowID {
            guard let window = content.windows.first(where: { $0.windowID == windowID }) else { throw CaptureFailure("source_unavailable") }
            filter = SCContentFilter(desktopIndependentWindow: window); size = window.frame.size
        } else {
            guard let displayID, let display = content.displays.first(where: { $0.displayID == displayID }) else { throw CaptureFailure("source_unavailable") }
            filter = SCContentFilter(display: display, excludingWindows: [])
            // Retina 按物理像素采样，避免先缩到逻辑点尺寸再编码。
            let mode = CGDisplayCopyDisplayMode(display.displayID)
            size = CGSize(width: mode?.pixelWidth ?? CGDisplayPixelsWide(display.displayID),
                height: mode?.pixelHeight ?? CGDisplayPixelsHigh(display.displayID))
        }
        if let displayID, let binding, displayBinding(displayID) != binding { throw CaptureFailure("display_changed") }
        guard size.width > 0, size.height > 0 else { throw CaptureFailure("invalid_dimensions") }
        // 启动参数是长边/短边预算，按源屏方向放置，保持比例且不放大。
        let longEdge = Double(max(width, height)), shortEdge = Double(min(width, height))
        let scale = min(1, min(longEdge / max(size.width, size.height), shortEdge / min(size.width, size.height)))
        let w = max(2, Int(size.width * scale) / 2 * 2)
        let h = max(2, Int(size.height * scale) / 2 * 2)
        self.fps = fps
        let config = SCStreamConfiguration()
        config.width = w; config.height = h
        config.pixelFormat = kCVPixelFormatType_32BGRA
        config.minimumFrameInterval = CMTime(value: 1, timescale: Int32(fps))
        config.queueDepth = 3; config.showsCursor = true; config.capturesAudio = false
        let specification = [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: true] as CFDictionary
        // refcon 借用 self；stop 完成所有回调并 invalidate 后，才释放 Capture owner。
        try check(VTCompressionSessionCreate(allocator: nil, width: Int32(w), height: Int32(h), codecType: kCMVideoCodecType_H264,
            encoderSpecification: specification, imageBufferAttributes: nil, compressedDataAllocator: nil,
            outputCallback: { ref, _, status, _, sample in
                guard let ref else { return }
                let owner = Unmanaged<Capture>.fromOpaque(ref).takeUnretainedValue()
                defer { owner.pending.signal() }
                do {
                    try check(status, "encoder_failed")
                    if let sample { try owner.output(sample) }
                } catch {
                    if !owner.control.stopped { emitError(error) }
                    owner.control.stop()
                }
            }, refcon: Unmanaged.passUnretained(self).toOpaque(), compressionSessionOut: &encoder), "encoder_unavailable")
        guard let encoder else { throw CaptureFailure("encoder_unavailable") }
        // 1920×1440 需要 10800 宏块，超出 Level 4.2；两段 SDP 统一协商 Level 5.0。
        let properties: [(CFString, CFTypeRef)] = [
            (kVTCompressionPropertyKey_RealTime, kCFBooleanTrue!),
            (kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse!),
            (kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_Baseline_5_0),
            (kVTCompressionPropertyKey_AverageBitRate, NSNumber(value: 8_000_000)),
            (kVTCompressionPropertyKey_ExpectedFrameRate, NSNumber(value: fps)),
            (kVTCompressionPropertyKey_MaxKeyFrameInterval, NSNumber(value: fps))]
        for (key, value) in properties { try check(VTSessionSetProperty(encoder, key: key, value: value), "encoder_configuration_failed") }
        try check(VTCompressionSessionPrepareToEncodeFrames(encoder), "encoder_unavailable")
        if control.stopped { throw CaptureFailure("cancelled") }
        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        self.stream = stream
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        try await stream.startCapture()
        // 静止屏幕没有新采集回调时，晚加入者或 PLI 仍能得到新关键帧。
        let refresh = DispatchSource.makeTimerSource(queue: queue)
        refresh.schedule(deadline: .now() + 0.1, repeating: 0.1)
        refresh.setEventHandler { [weak self] in
            guard let self, !self.control.stopped, self.control.needsKeyframe, let buffer = self.lastBuffer else { return }
            self.encode(buffer, pts: CMClockGetTime(CMClockGetHostTimeClock()))
        }
        self.refresh = refresh; refresh.resume()
        report(["type": "configured", "codec": "h264", "hardware": true, "width": w, "height": h, "fps": fps])
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        if !control.stopped { report(["type": "error", "code": "capture_interrupted", "status": (error as NSError).code]) }
        control.stop()
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
        guard !control.stopped, type == .screen, sample.isValid,
              let info = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let status = info.first?[.status] as? Int, SCFrameStatus(rawValue: status) == .complete,
              let buffer = CMSampleBufferGetImageBuffer(sample) else { return }
        lastBuffer = buffer
        encode(buffer, pts: sample.presentationTimeStamp)
    }

    func encode(_ buffer: CVPixelBuffer, pts: CMTime) {
        guard !control.stopped, let encoder else { return }
        guard pending.wait(timeout: .now()) == .success else { return }
        if let bitrate = control.takeBitrate() {
            let result = VTSessionSetProperty(encoder, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: bitrate))
            if result != noErr { pending.signal(); report(["type":"error", "code":"encoder_configuration_failed"]); control.stop(); return }
        }
        let timestamp = lastPTS.isValid && CMTimeCompare(pts, lastPTS) <= 0 ? CMTimeAdd(lastPTS, CMTime(value: 1, timescale: 1_000_000)) : pts
        lastPTS = timestamp
        let properties = control.takeKeyframe() ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
        let result = VTCompressionSessionEncodeFrame(encoder, imageBuffer: buffer, presentationTimeStamp: timestamp,
            duration: CMTime(value: 1, timescale: Int32(fps)), frameProperties: properties, sourceFrameRefcon: nil, infoFlagsOut: nil)
        if result != noErr {
            pending.signal(); report(["type": "error", "code": "encoder_failed", "status": result]); control.stop()
        }
    }

    func output(_ sample: CMSampleBuffer) throws {
        guard let format = sample.formatDescription, let block = sample.dataBuffer else { throw CaptureFailure("invalid_encoded_frame") }
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]]
        let keyframe = attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool != true
        var data = Data()
        let prefix: [UInt8] = [0, 0, 0, 1]
        var header: Int32 = 0
        var count = 0
        try check(CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: 0, parameterSetPointerOut: nil,
            parameterSetSizeOut: nil, parameterSetCountOut: &count, nalUnitHeaderLengthOut: &header), "invalid_h264_parameters")
        guard header == 4, count <= 16 else { throw CaptureFailure("invalid_h264_parameters") }
        if keyframe {
            for index in 0..<count {
                var pointer: UnsafePointer<UInt8>?; var size = 0
                try check(CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, parameterSetIndex: index, parameterSetPointerOut: &pointer,
                    parameterSetSizeOut: &size, parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil), "invalid_h264_parameters")
                guard let pointer, size <= 65_536 else { throw CaptureFailure("invalid_h264_parameters") }
                data.append(contentsOf: prefix); data.append(pointer, count: size)
            }
        }
        let length = CMBlockBufferGetDataLength(block)
        guard length > 0, length <= 8_000_000 else { throw CaptureFailure("encoded_frame_too_large") }
        var avcc = [UInt8](repeating: 0, count: length)
        try check(CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: length, destination: &avcc), "invalid_encoded_frame")
        var offset = 0
        while offset + 4 <= avcc.count {
            let size = avcc[offset..<offset + 4].reduce(0) { ($0 << 8) | Int($1) }; offset += 4
            guard size > 0, size <= avcc.count - offset else { throw CaptureFailure("invalid_encoded_frame") }
            data.append(contentsOf: prefix); data.append(contentsOf: avcc[offset..<offset + size]); offset += size
        }
        guard offset == avcc.count, data.count <= 4_194_304 else { throw CaptureFailure("invalid_encoded_frame") }
        let micros = CMTimeConvertScale(sample.presentationTimeStamp, timescale: 1_000_000, method: .default).value
        guard micros >= 0 else { throw CaptureFailure("invalid_timestamp") }
        var size = UInt32(data.count).bigEndian; var timestamp = UInt64(micros).bigEndian
        var packet = Data()
        withUnsafeBytes(of: &size) { packet.append(contentsOf: $0) }
        withUnsafeBytes(of: &timestamp) { packet.append(contentsOf: $0) }
        packet.append(keyframe ? 1 : 0); packet.append(data)
        try writePacket(packet, control: control)
    }

    func stop() async {
        refresh?.cancel(); refresh = nil
        try? await stream?.stopCapture()
        // 排空采集回调后再销毁编码器，避免提交与 invalidate 并发。
        queue.sync {
            lastBuffer = nil
            if let encoder {
                VTCompressionSessionCompleteFrames(encoder, untilPresentationTimeStamp: .invalid)
                VTCompressionSessionInvalidate(encoder)
                self.encoder = nil
            }
        }
    }
}

func emitError(_ error: Error) {
    if let error = error as? CaptureFailure { report(["type": "error", "code": error.code, "status": error.status]) }
    else { report(["type": "error", "code": "capture_unavailable", "status": (error as NSError).code]) }
}

let inputArgs = Array(CommandLine.arguments.dropFirst())
if inputArgs.first == "--input" || inputArgs.first == "--release" { desktopInputMain(inputArgs) }
let control = Control()
signal(SIGPIPE, SIG_IGN)
_ = fcntl(STDOUT_FILENO, F_SETFL, fcntl(STDOUT_FILENO, F_GETFL) | O_NONBLOCK)
control.read()
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
Task {
    guard #available(macOS 13.0, *) else {
        report(["type": "error", "code": "unsupported_os", "status": 0]); exit(1)
    }
    let capture = Capture(control)
    do {
        let args = Array(CommandLine.arguments.dropFirst())
        if args == ["--list"] {
            guard CGPreflightScreenCaptureAccess() else { throw CaptureFailure("screen_permission_required") }
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            let displays: [[String: Any]] = content.displays.map { display in
                let bounds = CGDisplayBounds(display.displayID)
                let mode = CGDisplayCopyDisplayMode(display.displayID)
                return ["id": String(display.displayID), "width": mode?.pixelWidth ?? CGDisplayPixelsWide(display.displayID), "height": mode?.pixelHeight ?? CGDisplayPixelsHigh(display.displayID), "binding": displayBinding(display.displayID),
                    "x": bounds.origin.x, "y": bounds.origin.y, "primary": CGDisplayIsMain(display.displayID) != 0]
            }
            let data = try JSONSerialization.data(withJSONObject: displays, options: [.sortedKeys])
            try writePacket(data + Data([10]), control: control)
        } else {
            guard (5...6).contains(args.count), ["--display", "--window"].contains(args[0]), let id = UInt32(args[1]),
                  let w = Int(args[2]), let h = Int(args[3]), let fps = Int(args[4]),
                  (2...1920).contains(max(w, h)), (2...1440).contains(min(w, h)), (1...30).contains(fps) else { throw CaptureFailure("invalid_arguments") }
            let display = args[0] == "--display" ? id : nil
            let binding = display.map(displayBinding)
            if args.count == 6 && args[5] != binding { throw CaptureFailure("display_changed") }
            try await capture.start(displayID: args[0] == "--display" ? id : nil,
                windowID: args[0] == "--window" ? id : nil, width: w, height: h, fps: fps, binding: binding)
            while !control.stopped {
                if let display, CGDisplayIsActive(display) == 0 || displayBinding(display) != binding { throw CaptureFailure("display_changed") }
                try await Task.sleep(nanoseconds: 20_000_000)
            }
        }
        await capture.stop()
        exit(0)
    } catch {
        emitError(error); control.stop(); await capture.stop(); exit(1)
    }
}
RunLoop.main.run()
