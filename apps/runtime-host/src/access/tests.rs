//! 地址准入与本机权限分离；不依赖数据库或真实监听。

use super::*;

#[test]
fn remote_ip_access_includes_forwarded_loopback_but_requires_enabled_policy() {
    let (commands, _receive) = mpsc::channel(1);
    let handle = HostAccessHandle {
        credentials: Arc::new(Credentials::new()),
        commands,
        remote: Arc::new(RwLock::new(RemoteAccess::default())),
    };
    let addresses = [
        "127.0.0.1",
        "127.0.0.2",
        "[::1]",
        "192.0.2.10",
        "[2001:db8::1]",
        "0.0.0.0",
        "[::]",
    ];
    for address in addresses {
        assert!(matches!(
            handle.remote_connection(address),
            Err(AccessError::Unauthorized)
        ));
    }
    {
        let mut remote = handle.remote.write().unwrap();
        remote.enabled = true;
        remote
            .server_names
            .extend(["RUNTIME.TEST".into(), "例子.测试".into()]);
    }
    for address in addresses {
        assert!(handle.remote_connection(address).is_ok(), "{address}");
    }
    assert!(handle.remote_connection("runtime.test").is_ok());
    assert!(handle.remote_connection("xn--fsqu00a.xn--0zwm56d").is_ok());
    for address in ["unknown.test", "localhost"] {
        assert!(matches!(
            handle.remote_connection(address),
            Err(AccessError::Unauthorized)
        ));
    }
    let granted = handle.remote_connection("127.0.0.1").unwrap();
    handle.remote.write().unwrap().disable();
    assert!(granted.is_cancelled());
    assert!(handle.remote_connection("127.0.0.1").is_err());
}
