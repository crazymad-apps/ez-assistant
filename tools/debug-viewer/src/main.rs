fn main() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(error) = runtime.block_on(debug_viewer::run(debug_viewer::port_from_env())) {
        eprintln!("debug viewer 退出：{error}");
        std::process::exit(1);
    }
}
