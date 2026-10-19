// 独立输入模式；不切换桌面、不提权。锁只保护本控制者的按下集合和短输入调用。
#include <map>
#include <set>
#include <mutex>
#include <sstream>
#include <cmath>

namespace desktop_input {
static const std::map<std::string,int> keys={
 {"KeyA",0x1e},{"KeyB",0x30},{"KeyC",0x2e},{"KeyD",0x20},{"KeyE",0x12},{"KeyF",0x21},{"KeyG",0x22},{"KeyH",0x23},{"KeyI",0x17},{"KeyJ",0x24},{"KeyK",0x25},{"KeyL",0x26},{"KeyM",0x32},{"KeyN",0x31},{"KeyO",0x18},{"KeyP",0x19},{"KeyQ",0x10},{"KeyR",0x13},{"KeyS",0x1f},{"KeyT",0x14},{"KeyU",0x16},{"KeyV",0x2f},{"KeyW",0x11},{"KeyX",0x2d},{"KeyY",0x15},{"KeyZ",0x2c},
 {"Digit1",2},{"Digit2",3},{"Digit3",4},{"Digit4",5},{"Digit5",6},{"Digit6",7},{"Digit7",8},{"Digit8",9},{"Digit9",10},{"Digit0",11},
 {"Escape",1},{"Minus",12},{"Equal",13},{"Backspace",14},{"Tab",15},{"BracketLeft",26},{"BracketRight",27},{"Enter",28},{"ControlLeft",29},{"Semicolon",39},{"Quote",40},{"Backquote",41},{"ShiftLeft",42},{"Backslash",43},{"Comma",51},{"Period",52},{"Slash",53},{"ShiftRight",54},{"AltLeft",56},{"Space",57},{"CapsLock",58},
 {"F1",59},{"F2",60},{"F3",61},{"F4",62},{"F5",63},{"F6",64},{"F7",65},{"F8",66},{"F9",67},{"F10",68},{"F11",87},{"F12",88},
 {"ControlRight",0x11d},{"AltRight",0x138},{"Home",0x147},{"ArrowUp",0x148},{"PageUp",0x149},{"ArrowLeft",0x14b},{"ArrowRight",0x14d},{"End",0x14f},{"ArrowDown",0x150},{"PageDown",0x151},{"Insert",0x152},{"Delete",0x153},{"MetaLeft",0x15b},{"MetaRight",0x15c}};
static void post(INPUT event) { if(SendInput(1,&event,sizeof(event))!=1) throw std::runtime_error("input_failed"); }
static bool desktop_available() {
    HDESK desktop=OpenInputDesktop(0,FALSE,DESKTOP_READOBJECTS);
    if(!desktop) return false;
    wchar_t name[128]{};DWORD needed=0;
    bool valid=GetUserObjectInformationW(desktop,UOI_NAME,name,sizeof(name),&needed) && std::wstring(name)==L"Default";
    CloseDesktop(desktop);return valid;
}
struct Executor {
    std::set<std::string> pressed_keys;std::set<int> buttons;
    std::wstring display;std::string binding;RECT bounds{};Clock::time_point heartbeat=Clock::now();
    explicit Executor(const Display& d):display(d.desc.DeviceName),binding(display_binding(d.desc.DeviceName)),bounds(d.desc.DesktopCoordinates){}
    bool valid() {
        DEVMODEW mode{};mode.dmSize=sizeof(mode);
        return Clock::now()-heartbeat<=std::chrono::seconds(1) && desktop_available() && EnumDisplaySettingsW(display.c_str(),ENUM_CURRENT_SETTINGS,&mode) && mode.dmDisplayOrientation==DMDO_DEFAULT &&
            std::to_string(mode.dmPosition.x)+","+std::to_string(mode.dmPosition.y)+","+std::to_string(mode.dmPelsWidth)+","+std::to_string(mode.dmPelsHeight)+","+std::to_string(mode.dmDisplayOrientation)==binding && mode.dmPosition.x==bounds.left && mode.dmPosition.y==bounds.top && mode.dmPelsWidth==static_cast<DWORD>(bounds.right-bounds.left) && mode.dmPelsHeight==static_cast<DWORD>(bounds.bottom-bounds.top);
    }
    void key(const std::string& name,bool down) {
        auto found=keys.find(name);if(found==keys.end())throw std::runtime_error("invalid_input");
        INPUT event{};event.type=INPUT_KEYBOARD;event.ki.wScan=static_cast<WORD>(found->second&0xff);
        event.ki.dwFlags=KEYEVENTF_SCANCODE|(found->second&0x100?KEYEVENTF_EXTENDEDKEY:0)|(down?0:KEYEVENTF_KEYUP);
        // 先记可能已执行集合，SendInput 返回失败也允许在收尾补发释放。
        if(down)pressed_keys.insert(name);post(event);if(!down)pressed_keys.erase(name);
    }
    void button(int b,bool down) {
        if(b<0||b>2)throw std::runtime_error("invalid_input");
        INPUT event{};event.type=INPUT_MOUSE;
        event.mi.dwFlags=b==0?(down?MOUSEEVENTF_LEFTDOWN:MOUSEEVENTF_LEFTUP):b==1?(down?MOUSEEVENTF_MIDDLEDOWN:MOUSEEVENTF_MIDDLEUP):(down?MOUSEEVENTF_RIGHTDOWN:MOUSEEVENTF_RIGHTUP);
        if(down)buttons.insert(b);post(event);if(!down)buttons.erase(b);
    }
    bool release() {
        bool ok=true;auto old_keys=pressed_keys;auto old_buttons=buttons;
        for(const auto& k:old_keys){try{key(k,false);}catch(...){ok=false;}}
        for(int b:old_buttons){try{button(b,false);}catch(...){ok=false;}}
        return ok;
    }
    bool command(const std::vector<std::string>& w) {
        if(w.size()<2)throw std::runtime_error("invalid_input");
        if(w[0]=="stop"){if(!release())throw std::runtime_error("release_failed");return false;}
        if(w[0]=="release"){if(!release())throw std::runtime_error("release_failed");return true;}
        if(!valid())throw std::runtime_error("input_unavailable");
        if(w[0]=="heartbeat"){heartbeat=Clock::now();return true;}
        if(w[0]=="move"&&w.size()==4){
            double x=std::stod(w[2]),y=std::stod(w[3]);if(!std::isfinite(x)||!std::isfinite(y)||x<0||x>1||y<0||y>1)throw std::runtime_error("invalid_input");
            const int left=GetSystemMetrics(SM_XVIRTUALSCREEN),top=GetSystemMetrics(SM_YVIRTUALSCREEN),width=GetSystemMetrics(SM_CXVIRTUALSCREEN),height=GetSystemMetrics(SM_CYVIRTUALSCREEN);
            if(width<2||height<2)throw std::runtime_error("input_unavailable");
            INPUT event{};event.type=INPUT_MOUSE;event.mi.dx=static_cast<LONG>((bounds.left+x*(bounds.right-bounds.left-1)-left)*65535/(width-1));event.mi.dy=static_cast<LONG>((bounds.top+y*(bounds.bottom-bounds.top-1)-top)*65535/(height-1));event.mi.dwFlags=MOUSEEVENTF_MOVE|MOUSEEVENTF_ABSOLUTE|MOUSEEVENTF_VIRTUALDESK;post(event);
        }else if(w[0]=="button"&&w.size()==5){button(std::stoi(w[2]),w[3]=="1");}
        else if(w[0]=="key"&&w.size()==5){key(w[2],w[3]=="1");}
        else if(w[0]=="scroll"&&w.size()==4){
            int x=std::stoi(w[2]),y=std::stoi(w[3]);if(x < -1200||x>1200||y < -1200||y>1200)throw std::runtime_error("invalid_input");
            if(y){INPUT e{};e.type=INPUT_MOUSE;e.mi.dwFlags=MOUSEEVENTF_WHEEL;e.mi.mouseData=static_cast<DWORD>(-y);post(e);}
            if(x){INPUT e{};e.type=INPUT_MOUSE;e.mi.dwFlags=MOUSEEVENTF_HWHEEL;e.mi.mouseData=static_cast<DWORD>(x);post(e);}
        }else if(w[0]=="text"&&w.size()==3){
            const auto& hex=w[2];if(hex.size()>32768||hex.size()%2||!pressed_keys.empty()||!buttons.empty())throw std::runtime_error("invalid_input");
            std::string text;for(size_t i=0;i<hex.size();i+=2){size_t used=0;auto byte=std::stoul(hex.substr(i,2),&used,16);if(used!=2||!byte||byte>255)throw std::runtime_error("invalid_input");text.push_back(static_cast<char>(byte));}
            int count=MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,text.data(),static_cast<int>(text.size()),nullptr,0);if(count<=0)throw std::runtime_error("invalid_input");
            std::wstring unicode(static_cast<size_t>(count),0);MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,text.data(),static_cast<int>(text.size()),unicode.data(),count);
            for(wchar_t unit:unicode){if(!valid())throw std::runtime_error("input_unavailable");for(bool down:{true,false}){INPUT e{};e.type=INPUT_KEYBOARD;e.ki.wScan=unit;e.ki.dwFlags=KEYEVENTF_UNICODE|(down?0:KEYEVENTF_KEYUP);post(e);}}
        }else throw std::runtime_error("invalid_input");
        return true;
    }
};
static int run(int argc,char**argv){
    try {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        auto list=displays();auto chosen=list.begin();
        if((argc==3||argc==4)&&std::string(argv[1])=="--input") chosen=std::find_if(list.begin(),list.end(),[&](const Display& d){return d.id==argv[2];});
        else if(!(argc==4&&std::string(argv[1])=="--release"))throw std::runtime_error("invalid_input");
        if(chosen==list.end()||!desktop_available())throw std::runtime_error("input_unavailable");
        Executor executor(*chosen);
        if(std::string(argv[1])=="--input" && argc==4 && argv[3]!=executor.binding)throw std::runtime_error("display_changed");
        if(std::string(argv[1])=="--release"){
            std::istringstream ks(argv[2]),bs(argv[3]);std::string s;
            while(std::getline(ks,s,','))executor.key(s,false);while(std::getline(bs,s,','))executor.button(std::stoi(s),false);return 0;
        }
        std::mutex mutex;std::atomic<bool> done{false};
        std::thread watchdog([&]{while(!done){std::this_thread::sleep_for(std::chrono::milliseconds(50));std::lock_guard<std::mutex> lock(mutex);if(!executor.valid()){bool ok=executor.release();ExitProcess(ok?0:1);}}});
        std::puts("ready");std::fflush(stdout);std::string line;char byte;bool keep=true;int result=0;
        while(keep&&_read(0,&byte,1)==1){
            if(byte=='\n'){
                std::istringstream stream(line);std::vector<std::string>w;std::string word;while(stream>>word)w.push_back(word);line.clear();
                std::lock_guard<std::mutex> lock(mutex);
                try {keep=executor.command(w);std::printf("ok %s\n",w[1].c_str());std::fflush(stdout);}
                catch(...){std::puts("error input_failed");std::fflush(stdout);result=1;keep=false;}
            }else{line.push_back(byte);if(line.size()>33000){result=1;break;}}
        }
        {std::lock_guard<std::mutex> lock(mutex);if(!executor.release())result=1;done=true;}
        watchdog.join();return result;
    }catch(...){std::puts("error input_unavailable");std::fflush(stdout);return 1;}
}
}
