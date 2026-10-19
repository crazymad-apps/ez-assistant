// 输入模式与采集模式为独立子进程。串行执行器拥有按下集合，IPC EOF/短存活到期只释放自己的输入。
import Foundation
import ApplicationServices
import CoreGraphics
import Darwin

private let physicalKeys: [String: CGKeyCode] = [
 "KeyA":0,"KeyS":1,"KeyD":2,"KeyF":3,"KeyH":4,"KeyG":5,"KeyZ":6,"KeyX":7,"KeyC":8,"KeyV":9,"KeyB":11,
 "KeyQ":12,"KeyW":13,"KeyE":14,"KeyR":15,"KeyY":16,"KeyT":17,"Digit1":18,"Digit2":19,"Digit3":20,"Digit4":21,"Digit6":22,"Digit5":23,
 "Equal":24,"Digit9":25,"Digit7":26,"Minus":27,"Digit8":28,"Digit0":29,"BracketRight":30,"KeyO":31,"KeyU":32,"BracketLeft":33,"KeyI":34,"KeyP":35,
 "Enter":36,"KeyL":37,"KeyJ":38,"Quote":39,"KeyK":40,"Semicolon":41,"Backslash":42,"Comma":43,"Slash":44,"KeyN":45,"KeyM":46,"Period":47,
 "Tab":48,"Space":49,"Backquote":50,"Backspace":51,"Escape":53,"MetaLeft":55,"MetaRight":54,"ShiftLeft":56,"ShiftRight":60,"CapsLock":57,
 "AltLeft":58,"AltRight":61,"ControlLeft":59,"ControlRight":62,"F1":122,"F2":120,"F3":99,"F4":118,"F5":96,"F6":97,"F7":98,"F8":100,
 "F9":101,"F10":109,"F11":103,"F12":111,"Insert":114,"Home":115,"PageUp":116,"Delete":117,"End":119,"PageDown":121,"ArrowLeft":123,"ArrowRight":124,"ArrowDown":125,"ArrowUp":126]

private final class DesktopInputExecutor {
    let display: CGDirectDisplayID
    let binding: String
    let bounds: CGRect
    var keys = Set<String>(), buttons = Set<Int>()
    var point: CGPoint
    var heartbeat = ProcessInfo.processInfo.systemUptime
    init(_ display: CGDirectDisplayID) { self.display=display; binding=displayBinding(display); bounds=CGDisplayBounds(display); point=CGPoint(x:bounds.midX,y:bounds.midY) }
    var flags: CGEventFlags {
        var result: CGEventFlags=[]
        for (names, flag): ([String],CGEventFlags) in [(["ShiftLeft","ShiftRight"],.maskShift),(["ControlLeft","ControlRight"],.maskControl),(["AltLeft","AltRight"],.maskAlternate),(["MetaLeft","MetaRight"],.maskCommand)] {
            if names.contains(where: { keys.contains($0) }) { result.insert(flag) }
        }
        return result
    }
    func key(_ name: String,_ down: Bool,_ repeating: Bool=false) throws {
        guard let code=physicalKeys[name],let event=CGEvent(keyboardEventSource:nil,virtualKey:code,keyDown:down) else { throw CaptureFailure("invalid_input") }
        if down {keys.insert(name)} else {keys.remove(name)}
        event.flags=flags; event.setIntegerValueField(.keyboardEventAutorepeat,value:repeating ? 1:0);event.setIntegerValueField(.eventSourceUserData,value:0x455A3200);event.post(tap:.cghidEventTap)
    }
    func button(_ number: Int,_ down: Bool,_ clicks: Int=1) throws {
        guard (0...2).contains(number) else {throw CaptureFailure("invalid_input")}
        let b:CGMouseButton=number==0 ? .left : number==1 ? .center : .right
        let type:CGEventType=number==0 ? (down ? .leftMouseDown:.leftMouseUp) : number==2 ? (down ? .rightMouseDown:.rightMouseUp) : (down ? .otherMouseDown:.otherMouseUp)
        guard let event=CGEvent(mouseEventSource:nil,mouseType:type,mouseCursorPosition:point,mouseButton:b) else {throw CaptureFailure("input_failed")}
        if down {buttons.insert(number)} else {buttons.remove(number)}
        event.flags=flags;event.setIntegerValueField(.mouseEventClickState,value:Int64(clicks));event.setIntegerValueField(.eventSourceUserData,value:0x455A3200);event.post(tap:.cghidEventTap)
    }
    func release() {for key in Array(keys){try? self.key(key,false)};for b in Array(buttons){try? button(b,false)}}
    func valid() -> Bool {
        let session=CGSessionCopyCurrentDictionary() as? [String:Any]
        return ProcessInfo.processInfo.systemUptime-heartbeat <= 1 && CGDisplayIsActive(display) != 0 && CGDisplayBounds(display)==bounds && displayBinding(display)==binding && AXIsProcessTrusted() && CGPreflightPostEventAccess() && session?["CGSSessionScreenIsLocked"] as? Bool != true
    }
    func handle(_ words:[Substring]) throws -> Bool {
        guard words.count>=2 else {throw CaptureFailure("invalid_input")}
        let kind=words[0]
        if kind=="stop" {release();return false}
        if kind=="release" {release();return true}
        guard valid() else {throw CaptureFailure("input_unavailable")}
        if kind=="heartbeat" {heartbeat=ProcessInfo.processInfo.systemUptime;return true}
        switch kind {
        case "move":
            guard words.count==4,let x=Double(words[2]),let y=Double(words[3]),x.isFinite,y.isFinite,(0...1).contains(x),(0...1).contains(y) else {throw CaptureFailure("invalid_input")}
            point=CGPoint(x:bounds.minX+x*max(0,bounds.width-1),y:bounds.minY+y*max(0,bounds.height-1))
            let type:CGEventType=buttons.contains(0) ? .leftMouseDragged:buttons.contains(2) ? .rightMouseDragged:buttons.contains(1) ? .otherMouseDragged:.mouseMoved
            guard let event=CGEvent(mouseEventSource:nil,mouseType:type,mouseCursorPosition:point,mouseButton:buttons.contains(2) ? .right:buttons.contains(1) ? .center:.left) else {throw CaptureFailure("input_failed")}
            event.flags=flags;event.setIntegerValueField(.eventSourceUserData,value:0x455A3200);event.post(tap:.cghidEventTap)
        case "button":
            guard words.count==5,let b=Int(words[2]),let down=Int(words[3]),let clicks=Int(words[4]),(0...1).contains(down),(1...2).contains(clicks) else {throw CaptureFailure("invalid_input")}
            try button(b,down==1,clicks)
        case "key":
            guard words.count==5,(words[3]=="0" || words[3]=="1"),(words[4]=="0" || words[4]=="1") else {throw CaptureFailure("invalid_input")}
            try key(String(words[2]),words[3]=="1",words[4]=="1")
        case "scroll":
            guard words.count==4,let x=Int32(words[2]),let y=Int32(words[3]),(-1200...1200).contains(x),(-1200...1200).contains(y),let event=CGEvent(scrollWheelEvent2Source:nil,units:.pixel,wheelCount:2,wheel1:-y,wheel2:-x,wheel3:0) else {throw CaptureFailure("invalid_input")}
            event.flags=flags;event.setIntegerValueField(.eventSourceUserData,value:0x455A3200);event.post(tap:.cghidEventTap)
        case "text":
            guard words.count==3,words[2].count<=32768,words[2].count%2==0,keys.isEmpty,buttons.isEmpty else {throw CaptureFailure("invalid_input")}
            let chars=Array(words[2]);var bytes=[UInt8]()
            for i in stride(from:0,to:chars.count,by:2){guard let b=UInt8(String(chars[i...i+1]),radix:16) else {throw CaptureFailure("invalid_input")};bytes.append(b)}
            guard let text=String(bytes:bytes,encoding:.utf8) else {throw CaptureFailure("invalid_input")}
            // Unicode 文本使用独立键码，避免目标输入法按 KeyA 的物理位置重新组合。
            // 按完整码点分成最多 20 个 UTF-16 单元，保留代理对。
            var chunks=[[UniChar]](), units=[UniChar]()
            for scalar in text.unicodeScalars {
                let next=Array(String(scalar).utf16)
                if units.count+next.count>20 {chunks.append(units);units=[]}
                units.append(contentsOf:next)
            }
            if !units.isEmpty {chunks.append(units)}
            for chunk in chunks {
                guard valid() else {throw CaptureFailure("input_unavailable")}
                for down in [true,false] {
                    guard let event=CGEvent(keyboardEventSource:nil,virtualKey:0xFFFF,keyDown:down) else {throw CaptureFailure("input_failed")}
                    event.flags=[];event.keyboardSetUnicodeString(stringLength:chunk.count,unicodeString:chunk)
                    event.setIntegerValueField(.eventSourceUserData,value:0x455A3200);event.post(tap:.cghidEventTap)
                }
            }
        default: throw CaptureFailure("invalid_input")
        }
        return true
    }
}

func desktopInputMain(_ args:[String]) -> Never {
    signal(SIGPIPE,SIG_IGN)
    guard AXIsProcessTrusted(),CGPreflightPostEventAccess() else {print("error input_permission_required");fflush(stdout);exit(1)}
    if args.first=="--release",args.count==3 {
        let executor=DesktopInputExecutor(CGMainDisplayID());executor.point=CGEvent(source:nil)?.location ?? .zero
        do {for name in args[1].split(separator:","){try executor.key(String(name),false)};for b in args[2].split(separator:","){guard let n=Int(b) else {throw CaptureFailure("invalid_input")};try executor.button(n,false)}} catch {exit(1)}
        exit(0)
    }
    guard (2...3).contains(args.count),args[0]=="--input",let display=UInt32(args[1]),CGDisplayIsActive(display) != 0 else {print("error input_unavailable");fflush(stdout);exit(1)}
    let executor=DesktopInputExecutor(display)
    if args.count==3 && args[2] != executor.binding {print("error input_unavailable");fflush(stdout);exit(1)}
    let queue=DispatchQueue(label:"ez.desktop.input")
    let timer=DispatchSource.makeTimerSource(queue:queue)
    timer.schedule(deadline:.now()+0.05,repeating:0.05)
    timer.setEventHandler{if !executor.valid(){executor.release();exit(0)}};timer.resume()
    print("ready");fflush(stdout)
    var line=Data(),byte:UInt8=0
    while Darwin.read(STDIN_FILENO,&byte,1)==1 {
        if byte==10 {
            guard let text=String(data:line,encoding:.utf8) else {break}
            line.removeAll(keepingCapacity:true)
            queue.sync {
                do {let words=text.split(separator:" ");let keep=try executor.handle(words);print("ok \(words[1])");fflush(stdout);if !keep {exit(0)}}
                catch {executor.release();print("error input_failed");fflush(stdout);exit(1)}
            }
        } else {line.append(byte);if line.count>33000 {break}}
    }
    queue.sync{executor.release()};exit(0)
}
