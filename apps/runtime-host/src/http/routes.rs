//! HTTP 路由与准入规则的唯一装配入口。
//!
//! 阅读顺序：资源分组 → 已登录入口 → 公开入口 → 全局网络边界。
//! 元组中间件按从左到右处理请求；`route_layer` 只影响已经注册的路由。
//! 因此先合并组、最后挂外层鉴权，不在鉴权函数中再次枚举 URL。
//! 新增普通业务接口应加入对应已登录分组；公开入口仅在文件末尾显式注册。

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware::{from_fn, from_fn_with_state},
    routing::{get, post},
};

use super::{
    HttpState, MAX_COMMAND_BYTES, attachments, auth, capabilities, commands, compatibility,
    ensure_ready, events, health, login, materializations, resources, terminals, web,
};

pub(crate) fn router(state: HttpState) -> Router {
    let version = from_fn(compatibility::require_declaration);
    let request_body = from_fn(auth::guard_request_body);
    let response_body = from_fn(auth::guard_response_body);
    let services = from_fn_with_state(state.clone(), auth::bind_user_services);
    let login_context = from_fn(auth::require_login_context);
    let user_identity = from_fn_with_state(state.clone(), auth::reject_enterprise_bootstrap);
    let enterprise = from_fn_with_state(state.clone(), auth::verify_enterprise);
    // 所有用户业务共用这一顺序：拒绝旧页面 → 排除企业管理凭据 → 复核用户登录。
    // require_login 由下方 authenticated 组统一挂载，保证这些层拿到有效身份。
    let user_access = (
        login_context.clone(),
        user_identity.clone(),
        enterprise.clone(),
    );
    // 固定用户域后再包装正文，使上传和下载同时受登录与用户域生命周期控制。
    let resource_body = (services, request_body.clone(), response_body.clone());

    // 浏览器可直接加载的 GET 媒体，才允许省略版本头并沿用登录时的版本声明。
    let media = Router::new()
        .route("/sessions/{session_id}/attachments/{attachment_id}/download", get(resources::download_attachment))
        .route("/sessions/{session_id}/messages/{message_id}/resources/{resource_ref_id}/download", get(resources::download_tool_file))
        .route("/sessions/{session_id}/child-tasks/{child_task_id}/messages/{message_id}/resources/{resource_ref_id}/download", get(resources::download_child_tool_file))
        .route("/sessions/{session_id}/attachments/{attachment_id}/preview", get(resources::preview_attachment))
        .route("/sessions/{session_id}/attachments/{attachment_id}/thumbnail", get(resources::thumbnail_attachment))
        .route("/sessions/{session_id}/messages/{message_id}/resources/{resource_ref_id}/preview", get(resources::preview_tool_file))
        .route("/sessions/{session_id}/child-tasks/{child_task_id}/messages/{message_id}/resources/{resource_ref_id}/preview", get(resources::preview_child_tool_file))
        .route("/sessions/{session_id}/export.md", get(resources::export_session_markdown))
        .route_layer((from_fn(compatibility::allow_session_declaration), resource_body.clone()));

    // 返回本机绝对路径还要有真实本机证明；证明只增加本机能力，不切换前面绑定的用户。
    let native_resources = Router::new()
        .route("/sessions/{session_id}/messages/{message_id}/resources/{resource_ref_id}/native-path", get(resources::resolve_tool_file_native_path))
        .route("/sessions/{session_id}/child-tasks/{child_task_id}/messages/{message_id}/resources/{resource_ref_id}/native-path", get(resources::resolve_child_tool_file_native_path))
        .route("/sessions/{session_id}/resource-files/native-path", post(resources::resolve_session_resource_native_path))
        .route_layer((
            from_fn_with_state(state.clone(), auth::require_local_proof),
            version.clone(),
            resource_body.clone(),
        ));

    // 上传由 handler 按流入字节执行 MAX_ATTACHMENT_BYTES 限制；不能先整体读入内存。
    let files = Router::new()
        .route("/host-files/list", post(resources::list_host_files))
        .route(
            "/host-files/select-directory",
            post(resources::select_host_directory),
        )
        .route("/host-files/preview", post(resources::preview_host_file))
        .route("/host-files/download", post(resources::download_host_file))
        .route(
            "/sessions/{session_id}/resource-files/download",
            post(resources::download_session_file),
        )
        .route(
            "/sessions/{session_id}/resource-files/list",
            post(resources::list_session_resource_files),
        )
        .route(
            "/sessions/{session_id}/resource-files/preview",
            post(resources::preview_session_resource_file),
        )
        .route(
            "/session-materializations",
            post(materializations::materialize_session).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/sessions/{session_id}/attachments",
            post(attachments::upload_attachment).layer(DefaultBodyLimit::disable()),
        )
        .route_layer((version.clone(), resource_body));
    let resources = files
        .merge(media)
        .merge(native_resources)
        .route_layer(user_access.clone());

    // 账号操作不隐式打开 Runtime；ensure-ready 自己阻塞到就绪，返回统一的 204。
    let user_control = Router::new()
        .route(
            "/auth/password",
            post(login::password).layer(DefaultBodyLimit::max(4096)),
        )
        .route("/runtime/ensure-ready", post(ensure_ready))
        .route_layer((user_access.clone(), version.clone(), request_body.clone()));

    // commands 同时承载 Host 管理和用户命令：handler 先分流，再按需取得用户 Runtime。
    // 不能在这里要求企业用户身份或提前打开用户数据库，也不能取消命令的最终响应。
    let commands = Router::new()
        .route(
            "/commands",
            post(commands::handle_command).layer(DefaultBodyLimit::max(MAX_COMMAND_BYTES)),
        )
        .route_layer((
            login_context.clone(),
            enterprise.clone(),
            version.clone(),
            request_body.clone(),
        ));

    // SSE 在 handler 内订阅本人 Runtime；连接结束只停止观察，不取消已接纳的任务。
    let events = Router::new()
        .route("/events", get(events::stream_events))
        .route_layer((
            user_access,
            version.clone(),
            request_body.clone(),
            response_body.clone(),
        ));

    let authenticated = Router::new()
        .merge(resources)
        .merge(user_control)
        .merge(commands)
        .merge(events)
        // 查询身份时尚未拿到 login_context；允许本机管理端查看自己的 bootstrap 身份。
        .route(
            "/auth/session",
            get(login::session).layer((enterprise, version.clone(), request_body.clone())),
        )
        // 退出必须在中心离线、客户端版本不兼容时也能执行；先校验页面仍属于当前账号。
        .route(
            "/auth/logout",
            post(login::logout).layer((login_context, user_identity.clone(), request_body.clone())),
        )
        // 健康查询保留登录要求，但不依赖客户端版本或用户 Runtime 就绪。
        .route(
            "/health",
            get(health).layer((request_body.clone(), response_body.clone())),
        )
        .route_layer(from_fn(auth::require_login));

    let api = authenticated
        // 登录允许匿名或过期 Cookie；具体登录方式的同源检查与凭据交换由 handler 负责。
        .route(
            "/auth/login",
            post(login::login).layer((version, request_body.clone(), DefaultBodyLimit::max(4096))),
        )
        .route(
            "/capabilities",
            get(capabilities).layer((request_body, response_body)),
        )
        // WS 只放行到有界首帧认证；版本、账号上下文和 Runtime 都在首帧验证后处理。
        .route(
            "/user-terminals/socket",
            get(terminals::upgrade).layer((
                from_fn_with_state(state.clone(), auth::require_socket_origin),
                user_identity,
            )),
        )
        .layer(from_fn_with_state(state.clone(), auth::authorize));

    let pages = Router::new()
        .fallback(web::serve)
        .layer(from_fn_with_state(state.clone(), auth::authorize_page));
    api.merge(pages).with_state(state)
}
