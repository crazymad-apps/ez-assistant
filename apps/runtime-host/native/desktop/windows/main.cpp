// Host 私有桌面能力子进程。COM/D3D 资源归主线程；控制管道独立于视频输出。
// 采集与输入分别按进程模式运行，不切换用户、不提权、不监听网络。
#define NOMINMAX
#include <windows.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mftransform.h>
#include <mferror.h>
#include <codecapi.h>
#include <wmcodecdsp.h>
#include <wrl/client.h>
#include <io.h>
#include <fcntl.h>
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

using Microsoft::WRL::ComPtr;
using Clock = std::chrono::steady_clock;
static std::atomic<bool> stopped{false}, force_idr{true};
static std::atomic<unsigned long> requested_bitrate{0};
static void check(HRESULT status, const char* code) {
    if (FAILED(status)) { std::fprintf(stderr, "{\"type\":\"error\",\"code\":\"%s\",\"status\":%ld}\n", code, status); throw std::runtime_error(code); }
}
static void stop() {
    if (!stopped.exchange(true)) {
        // 驱动或 stdout 写入卡住时的最后兜底；进程结束后系统回收 COM、GPU 和管道资源。
        std::thread([] { std::this_thread::sleep_for(std::chrono::seconds(2)); ExitProcess(2); }).detach();
    }
}
static void control() {
    std::thread([] {
        std::string line; char byte;
        while (_read(0, &byte, 1) == 1) {
            if (byte == '\n') {
                if (line == "keyframe") { force_idr = true; line.clear(); }
                else if (line.rfind("bitrate ", 0) == 0) {
                    char* end = nullptr; const auto value = std::strtoul(line.c_str() + 8, &end, 10);
                    if (!end || *end != '\0' || value < 150000 || value > 8000000) { stop(); return; }
                    requested_bitrate = value; line.clear();
                }
                else { stop(); return; }
            } else {
                line.push_back(byte);
                if (line.size() > 32) { stop(); return; }
            }
        }
        stop();
    }).detach(); // 此线程仅拥有进程级 stdin，主循环返回即随进程退出。
}

struct Display {
    ComPtr<IDXGIAdapter1> adapter;
    ComPtr<IDXGIOutput> output;
    DXGI_OUTPUT_DESC desc{};
    std::string id;
};
static std::string display_binding(const wchar_t* name) {
    DEVMODEW mode{}; mode.dmSize = sizeof(mode);
    if (!EnumDisplaySettingsExW(name, ENUM_CURRENT_SETTINGS, &mode, 0)) throw std::runtime_error("display_changed");
    return std::to_string(mode.dmPosition.x)+","+std::to_string(mode.dmPosition.y)+","+
        std::to_string(mode.dmPelsWidth)+","+std::to_string(mode.dmPelsHeight)+","+std::to_string(mode.dmDisplayOrientation);
}
static std::vector<Display> displays() {
    DWORD session = 0;
    if (!ProcessIdToSessionId(GetCurrentProcessId(), &session) || session == 0) throw std::runtime_error("interactive_session_required");
    ComPtr<IDXGIFactory1> factory;
    check(CreateDXGIFactory1(IID_PPV_ARGS(&factory)), "capture_unavailable");
    std::vector<Display> list;
    for (UINT a = 0; a < 16; ++a) {
        ComPtr<IDXGIAdapter1> adapter;
        HRESULT hr = factory->EnumAdapters1(a, &adapter);
        if (hr == DXGI_ERROR_NOT_FOUND) break;
        check(hr, "display_enumeration_failed");
        for (UINT o = 0; o < 16; ++o) {
            Display display; display.adapter = adapter;
            hr = adapter->EnumOutputs(o, &display.output);
            if (hr == DXGI_ERROR_NOT_FOUND) break;
            check(hr, "display_enumeration_failed");
            check(display.output->GetDesc(&display.desc), "display_enumeration_failed");
            if (!display.desc.AttachedToDesktop) continue;
            // DXGI 设备名称（DISPLAYn），不用枚举列表下标作为跨调用标识。
            std::wstring name(display.desc.DeviceName);
            auto pos = name.find(L"DISPLAY");
            if (pos == std::wstring::npos) continue;
            auto suffix = name.substr(pos);
            for (wchar_t c : suffix) {
                if (c > 127) throw std::runtime_error("invalid_display_id");
                display.id.push_back(static_cast<char>(c));
            }
            list.push_back(std::move(display));
        }
    }
    if (list.empty()) throw std::runtime_error("no_displays");
    return list;
}

struct Encoder {
    ComPtr<IMFTransform> transform;
    ComPtr<ICodecAPI> settings;
    UINT width, height, fps;
    Encoder(UINT w, UINT h, UINT rate): width(w), height(h), fps(rate) {
        // 明确的软件降档路径，所有 Windows 版本均报告 hardware=false；不伪称硬编。
        check(CoCreateInstance(CLSID_CMSH264EncoderMFT, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&transform)), "encoder_unavailable");
        check(transform.As(&settings), "encoder_unavailable");
        // 默认的无约束 VBR 会缓存输入，低更新率桌面可能数秒没有首帧。
        // CBR、中等复杂度和两个切片线程提升文字画质；低延迟模式禁止多帧缓存。
        VARIANT value; VariantInit(&value); value.vt = VT_UI4;
        value.ulVal = eAVEncCommonRateControlMode_CBR;
        check(settings->SetValue(&CODECAPI_AVEncCommonRateControlMode, &value), "encoder_configuration_failed");
        value.ulVal = 50;
        check(settings->SetValue(&CODECAPI_AVEncCommonQualityVsSpeed, &value), "encoder_configuration_failed");
        value.ulVal = 2;
        check(settings->SetValue(&CODECAPI_AVEncNumWorkerThreads, &value), "encoder_configuration_failed");
        value.vt = VT_BOOL; value.boolVal = VARIANT_TRUE;
        check(settings->SetValue(&CODECAPI_AVLowLatencyMode, &value), "encoder_configuration_failed");
        ComPtr<IMFMediaType> out;
        check(MFCreateMediaType(&out), "encoder_configuration_failed");
        configure(out.Get(), MFVideoFormat_H264);
        check(out->SetUINT32(MF_MT_AVG_BITRATE, 8000000), "encoder_configuration_failed");
        check(out->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base), "encoder_configuration_failed");
        check(out->SetUINT32(MF_MT_MPEG2_LEVEL, 50), "encoder_configuration_failed");
        check(transform->SetOutputType(0, out.Get(), 0), "encoder_configuration_failed");
        ComPtr<IMFMediaType> in;
        check(MFCreateMediaType(&in), "encoder_configuration_failed"); configure(in.Get(), MFVideoFormat_NV12);
        check(transform->SetInputType(0, in.Get(), 0), "encoder_configuration_failed");
        value.vt = VT_UI4; value.ulVal = fps;
        check(settings->SetValue(&CODECAPI_AVEncMPVGOPSize, &value), "encoder_configuration_failed");
        check(transform->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0), "encoder_configuration_failed");
        check(transform->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "encoder_configuration_failed");
    }
    void configure(IMFMediaType* type, REFGUID subtype) {
        check(type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video), "encoder_configuration_failed");
        check(type->SetGUID(MF_MT_SUBTYPE, subtype), "encoder_configuration_failed");
        check(MFSetAttributeSize(type, MF_MT_FRAME_SIZE, width, height), "encoder_configuration_failed");
        check(MFSetAttributeRatio(type, MF_MT_FRAME_RATE, fps, 1), "encoder_configuration_failed");
        check(MFSetAttributeRatio(type, MF_MT_PIXEL_ASPECT_RATIO, 1, 1), "encoder_configuration_failed");
        check(type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive), "encoder_configuration_failed");
    }
    void submit(const std::vector<uint8_t>& nv12, uint64_t pts) {
        if (const auto bitrate = requested_bitrate.exchange(0)) {
            VARIANT value; VariantInit(&value); value.vt = VT_UI4; value.ulVal = bitrate;
            check(settings->SetValue(&CODECAPI_AVEncCommonMeanBitRate, &value), "encoder_configuration_failed");
        }
        if (force_idr.exchange(false)) {
            VARIANT value; VariantInit(&value); value.vt = VT_UI4; value.ulVal = 1;
            check(settings->SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &value), "encoder_failed");
        }
        ComPtr<IMFMediaBuffer> buffer;
        check(MFCreateMemoryBuffer(static_cast<DWORD>(nv12.size()), &buffer), "encoder_failed");
        BYTE* data = nullptr;
        check(buffer->Lock(&data, nullptr, nullptr), "encoder_failed");
        std::copy(nv12.begin(), nv12.end(), data);
        check(buffer->Unlock(), "encoder_failed");
        check(buffer->SetCurrentLength(static_cast<DWORD>(nv12.size())), "encoder_failed");
        ComPtr<IMFSample> sample;
        check(MFCreateSample(&sample), "encoder_failed");
        check(sample->AddBuffer(buffer.Get()), "encoder_failed");
        check(sample->SetSampleTime(static_cast<LONGLONG>(pts * 10)), "encoder_failed");
        check(sample->SetSampleDuration(10000000 / fps), "encoder_failed");
        check(transform->ProcessInput(0, sample.Get(), 0), "encoder_failed");
    }
    void output() {
        MFT_OUTPUT_STREAM_INFO info{};
        check(transform->GetOutputStreamInfo(0, &info), "encoder_failed");
        // MFT 要求的是最坏情况工作缓冲，不是实际编码帧长度；4:3 高清会超过原来的 4MiB。
        // 工作缓冲按最大原始 BGRA 帧有界，交给 Host 的实际编码帧仍限制为 4MiB。
        if (info.cbSize > 1920u * 1440u * 4u) throw std::runtime_error("encoded_frame_too_large");
        for (;;) {
            ComPtr<IMFSample> sample;
            check(MFCreateSample(&sample), "encoder_failed");
            ComPtr<IMFMediaBuffer> buffer;
            check(MFCreateMemoryBuffer(std::max(info.cbSize, DWORD(1048576)), &buffer), "encoder_failed");
            check(sample->AddBuffer(buffer.Get()), "encoder_failed");
            MFT_OUTPUT_DATA_BUFFER output{}; output.pSample = sample.Get();
            DWORD status = 0;
            HRESULT hr = transform->ProcessOutput(0, 1, &output, &status);
            if (output.pEvents) output.pEvents->Release();
            if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) return;
            check(hr, "encoder_failed");
            check(sample->ConvertToContiguousBuffer(&buffer), "encoder_failed");
            BYTE* data = nullptr; DWORD size = 0;
            check(buffer->Lock(&data, nullptr, &size), "encoder_failed");
            // 样本是 Annex-B；有界复制后先 Unlock，输出管道阻塞不持有 MF buffer 锁。
            if (!size || size > 4194304) { buffer->Unlock(); throw std::runtime_error("encoded_frame_too_large"); }
            std::vector<uint8_t> packet(13 + size);
            for (int i = 0; i < 4; ++i) packet[i] = static_cast<uint8_t>(size >> (24 - 8 * i));
            LONGLONG pts = 0; check(sample->GetSampleTime(&pts), "encoder_failed");
            auto micros = static_cast<uint64_t>(pts / 10);
            for (int i = 0; i < 8; ++i) packet[4 + i] = static_cast<uint8_t>(micros >> (56 - 8 * i));
            UINT32 key = 0; sample->GetUINT32(MFSampleExtension_CleanPoint, &key); packet[12] = key ? 1 : 0;
            std::copy(data, data + size, packet.begin() + 13);
            check(buffer->Unlock(), "encoder_failed");
            size_t offset = 0;
            while (offset < packet.size() && !stopped) {
                int count = _write(1, packet.data() + offset, static_cast<unsigned int>(packet.size() - offset));
                if (count <= 0) throw std::runtime_error("output_closed");
                offset += count;
            }
            if (stopped) return;
        }
    }
    ~Encoder() { if (transform) transform->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0); }
};

// 软件编码保持最多 15fps，优先保留分辨率；转换/编码/输出不建立积压队列。
// DXGI 保留最新桌面，消费者慢时降低采样频率，不丢已经提交的参考帧。
static uint8_t clamp(int n) { return static_cast<uint8_t>(std::clamp(n, 0, 255)); }
static void convert(const D3D11_MAPPED_SUBRESOURCE& source, UINT sw, UINT sh, UINT w, UINT h, DXGI_MODE_ROTATION rotation, std::vector<uint8_t>& out) {
    // 像素中心对齐的双线性降采样，避免最近邻丢失细线；NV12 色度取 2×2 平均值。
    const bool portrait = rotation == DXGI_MODE_ROTATION_ROTATE90 || rotation == DXGI_MODE_ROTATION_ROTATE270;
    const UINT dw = portrait ? sh : sw, dh = portrait ? sw : sh;
    auto pixel = [&](UINT x, UINT y, UINT c) {
        UINT sx = x, sy = y;
        switch (rotation) {
            case DXGI_MODE_ROTATION_ROTATE90: sx = y; sy = sh-1-x; break;
            case DXGI_MODE_ROTATION_ROTATE180: sx = sw-1-x; sy = sh-1-y; break;
            case DXGI_MODE_ROTATION_ROTATE270: sx = sw-1-y; sy = x; break;
            default: break;
        }
        return static_cast<const uint8_t*>(source.pData)[size_t(sy)*source.RowPitch+sx*4+c];
    };
    auto channel = [&](UINT x, UINT y, UINT c) {
        const int64_t sx = std::clamp(int64_t(2*x+1)*dw*128/w-128, int64_t(0), int64_t(dw-1)*256);
        const int64_t sy = std::clamp(int64_t(2*y+1)*dh*128/h-128, int64_t(0), int64_t(dh-1)*256);
        const UINT x0 = UINT(sx/256), y0 = UINT(sy/256);
        const UINT x1 = std::min(x0+1, dw-1), y1 = std::min(y0+1, dh-1);
        const int fx = int(sx%256), fy = int(sy%256);
        const int top = pixel(x0,y0,c)*(256-fx) + pixel(x1,y0,c)*fx;
        const int bottom = pixel(x0,y1,c)*(256-fx) + pixel(x1,y1,c)*fx;
        return (top*(256-fy) + bottom*fy + 32768)/65536;
    };
    for (UINT y = 0; y < h; y += 2) {
        for (UINT x = 0; x < w; x += 2) {
            int red = 0, green = 0, blue = 0;
            for (UINT dy = 0; dy < 2; ++dy) {
                for (UINT dx = 0; dx < 2; ++dx) {
                    int b = channel(x+dx, y+dy, 0), g = channel(x+dx, y+dy, 1), r = channel(x+dx, y+dy, 2);
                    out[(y+dy)*w+x+dx] = clamp(((66*r + 129*g + 25*b + 128) >> 8) + 16);
                    red += r; green += g; blue += b;
                }
            }
            const int r = (red+2)/4, g = (green+2)/4, b = (blue+2)/4;
            auto uv = w*h + y/2*w + x;
            out[uv] = clamp(((-38*r - 74*g + 112*b + 128) >> 8) + 128);
            out[uv+1] = clamp(((112*r - 94*g - 18*b + 128) >> 8) + 128);
        }
    }
}

static std::pair<UINT, UINT> capture_size(UINT sw, UINT sh, UINT long_edge, UINT short_edge) {
    // 预算随源屏方向旋转；只缩小，尺寸向下对齐至 NV12 偶数像素。
    double scale = std::min(1.0, std::min(double(long_edge)/std::max(sw, sh), double(short_edge)/std::min(sw, sh)));
    return {std::max(2u, UINT(sw * scale)/2*2), std::max(2u, UINT(sh * scale)/2*2)};
}

static void capture(Display& display, UINT requested_width, UINT requested_height, UINT requested_fps, const std::string& binding) {
    ComPtr<ID3D11Device> device; ComPtr<ID3D11DeviceContext> context;
    check(D3D11CreateDevice(display.adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        nullptr, 0, D3D11_SDK_VERSION, &device, nullptr, &context), "capture_unavailable");
    ComPtr<IDXGIOutput1> output;
    check(display.output.As(&output), "capture_unavailable");
    ComPtr<IDXGIOutputDuplication> duplication;
    check(output->DuplicateOutput(device.Get(), &duplication), "desktop_access_denied");
    DXGI_OUTDUPL_DESC desc{}; duplication->GetDesc(&desc);
    const bool portrait = desc.Rotation == DXGI_MODE_ROTATION_ROTATE90 || desc.Rotation == DXGI_MODE_ROTATION_ROTATE270;
    UINT sw = desc.ModeDesc.Width, sh = desc.ModeDesc.Height;
    if (!sw || !sh || sw > 16384 || sh > 16384) throw std::runtime_error("invalid_dimensions");
    const auto [w, h] = capture_size(portrait ? sh : sw, portrait ? sw : sh, std::max(requested_width, requested_height), std::min(requested_width, requested_height));
    const UINT fps = std::min(requested_fps, 15u);
    Encoder encoder(w, h, fps);
    D3D11_TEXTURE2D_DESC texture{};
    texture.Width = sw; texture.Height = sh; texture.MipLevels = 1; texture.ArraySize = 1;
    texture.Format = DXGI_FORMAT_B8G8R8A8_UNORM; texture.SampleDesc.Count = 1;
    texture.Usage = D3D11_USAGE_STAGING; texture.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ComPtr<ID3D11Texture2D> staging;
    check(device->CreateTexture2D(&texture, nullptr, &staging), "capture_unavailable");
    std::vector<uint8_t> nv12(size_t(w)*h*3/2);
    std::fprintf(stderr, "{\"type\":\"configured\",\"codec\":\"h264\",\"hardware\":false,\"width\":%u,\"height\":%u,\"fps\":%u}\n", w,h,fps);
    auto start = Clock::now();
    bool has_frame = false;
    unsigned acquired_count = 0, submitted_count = 0;
    while (!stopped) {
        if (display_binding(display.desc.DeviceName) != binding) throw std::runtime_error("display_changed");
        auto tick = Clock::now();
        DXGI_OUTDUPL_FRAME_INFO info{}; ComPtr<IDXGIResource> resource;
        HRESULT hr = duplication->AcquireNextFrame(1000/fps, &info, &resource);
        if (hr == DXGI_ERROR_WAIT_TIMEOUT && !has_frame) continue;
        // 静止桌面仍以限定帧率提交最后一帧，保证编码器输出、首帧和 PLI 恢复。
        // 只保留这一份 NV12，不增加采集队列，也不制造视觉变化。
        if (hr != DXGI_ERROR_WAIT_TIMEOUT) {
        check(hr, "capture_interrupted");
        // 取得帧后所有异常路径均 ReleaseFrame，不把下次采集卡在旧帧所有权上。
        struct Acquired { IDXGIOutputDuplication* p; ~Acquired() { if (p) p->ReleaseFrame(); } } acquired{duplication.Get()};
        ComPtr<ID3D11Texture2D> frame; check(resource.As(&frame), "capture_failed");
        context->CopyResource(staging.Get(), frame.Get());
        D3D11_MAPPED_SUBRESOURCE mapped{};
        check(context->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped), "capture_failed");
        convert(mapped, sw, sh, w, h, desc.Rotation, nv12); context->Unmap(staging.Get(), 0);
        check(duplication->ReleaseFrame(), "capture_failed"); acquired.p = nullptr;
        has_frame = true; ++acquired_count;
        }
        auto pts = std::chrono::duration_cast<std::chrono::microseconds>(Clock::now()-start).count();
        encoder.submit(nv12, static_cast<uint64_t>(pts)); ++submitted_count; encoder.output();
        std::this_thread::sleep_until(tick + std::chrono::microseconds(1000000/fps));
    }
    std::fprintf(stderr, "{\"type\":\"capture_stopped\",\"acquired\":%u,\"submitted\":%u}\n", acquired_count, submitted_count);
}

#include "input.hpp"

int main(int argc, char** argv) {
    _setmode(0, _O_BINARY); _setmode(1, _O_BINARY);
    if (argc > 1 && (std::string(argv[1]) == "--input" || std::string(argv[1]) == "--release")) return desktop_input::run(argc, argv);
    setvbuf(stderr, nullptr, _IONBF, 0); control();
    bool com = false, mf = false;
    int result = 0;
    try {
        // DXGI 枚举与后续画面统一使用物理像素；不能把 DPI 虚拟化尺寸当作源桌面尺寸。
        if (!SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            && GetLastError() != ERROR_ACCESS_DENIED) throw std::runtime_error("display_enumeration_failed");
        check(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "capture_unavailable"); com = true;
        check(MFStartup(MF_VERSION, MFSTARTUP_FULL), "encoder_unavailable"); mf = true;
        auto list = displays();
        if (argc == 2 && std::string(argv[1]) == "--list") {
            std::printf("[");
            for (size_t i = 0; i < list.size(); ++i) {
                auto& display = list[i]; auto r = display.desc.DesktopCoordinates;
                std::printf("%s{\"id\":\"%s\",\"width\":%ld,\"height\":%ld,\"x\":%ld,\"y\":%ld,\"primary\":%s,\"binding\":\"%s\"}",
                    i ? "," : "", display.id.c_str(), r.right-r.left, r.bottom-r.top, r.left, r.top, r.left==0 && r.top==0 ? "true" : "false", display_binding(display.desc.DeviceName).c_str());
            }
            std::printf("]\n"); std::fflush(stdout);
        } else if ((argc == 6 || argc == 7) && std::string(argv[1]) == "--display") {
            int w = std::stoi(argv[3]), h = std::stoi(argv[4]), fps = std::stoi(argv[5]);
            if (w < 2 || h < 2 || std::max(w, h) > 1920 || std::min(w, h) > 1440 || fps < 1 || fps > 30) throw std::runtime_error("invalid_arguments");
            auto display = std::find_if(list.begin(), list.end(), [&](auto& d) { return d.id == argv[2]; });
            if (display == list.end()) throw std::runtime_error("source_unavailable");
            const auto binding = display_binding(display->desc.DeviceName);
            if (argc == 7 && argv[6] != binding) throw std::runtime_error("display_changed");
            capture(*display, w, h, fps, binding);
        } else { throw std::runtime_error("invalid_arguments"); }
    } catch (const std::exception& error) {
        // 自定义 code 为固定 ASCII；标准转换异常不暴露原始参数或库正文。
        std::string code(error.what());
        if (code.empty() || code.size() > 64 || code.find_first_not_of("abcdefghijklmnopqrstuvwxyz_") != std::string::npos) code = "invalid_arguments";
        std::fprintf(stderr, "{\"type\":\"error\",\"code\":\"%s\"}\n", code.c_str()); result = 1;
    }
    if (mf) MFShutdown(); if (com) CoUninitialize();
    return result;
}
