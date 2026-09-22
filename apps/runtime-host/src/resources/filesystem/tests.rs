use super::*;
use agent_tools::{SearchKind, SearchMatch};
use agent_tools_local::LocalFileSystemConfig;

#[tokio::test]
async fn structured_search_excludes_private_trees_before_reading_and_keeps_public_results() {
    let dir = tempfile::tempdir().unwrap();
    let host = dir.path().join("host[private]");
    let user = host.join("users/alice");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::create_dir_all(host.join("users/bob")).unwrap();
    std::fs::write(host.join("users/bob/secret.txt"), "needle private").unwrap();
    std::fs::write(user.join("own.txt"), "needle own").unwrap();
    std::fs::write(dir.path().join("public.txt"), "needle public").unwrap();
    std::fs::write(dir.path().join("tls[secret].key"), "needle key").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(host.join("users/bob"), dir.path().join("alias")).unwrap();
    let fs = UserFileSystem {
        paths: Arc::new(UserPaths::new(
            &user,
            vec![dir.path().join("tls[secret].key")],
        )),
        inner: LocalFileSystem::new(LocalFileSystemConfig {
            max_text_file_bytes: 4096.try_into().unwrap(),
            ripgrep_program: "rg".into(),
            max_search_stderr_bytes: 4096.try_into().unwrap(),
        }),
    };
    for kind in [SearchKind::ByName, SearchKind::ByContent] {
        let request = SearchFilesRequest {
            query: if kind == SearchKind::ByName {
                "."
            } else {
                "needle"
            }
            .into(),
            path: AbsolutePath::new(dir.path().to_owned()).unwrap(),
            kind,
            max_results: 20.try_into().unwrap(),
            max_output_bytes: 4096.try_into().unwrap(),
            max_record_bytes: 4096.try_into().unwrap(),
        };
        let result = fs
            .search(request.clone(), FileToolContext::default())
            .await
            .unwrap();
        assert_eq!(result.matches.len(), 1, "{result:?}");
        let path = match &result.matches[0] {
            SearchMatch::Name { path } | SearchMatch::Content { path, .. } => path,
        };
        assert_eq!(path.as_path().file_name().unwrap(), "public.txt");
        let mut own = request.clone();
        own.path = AbsolutePath::new(user.clone()).unwrap();
        assert_eq!(
            fs.search(own, FileToolContext::default())
                .await
                .unwrap()
                .matches
                .len(),
            1
        );
        let mut other = request;
        other.path = AbsolutePath::new(host.join("users/bob")).unwrap();
        assert!(fs.search(other, FileToolContext::default()).await.is_err());
    }
}
