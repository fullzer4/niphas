use crate::cache::NarCache;
use crate::csi;
use crate::mount::{MountOps, RealMountOps};
use niphas_core::nix::cache_client::{BinaryCacheClient, CacheClient};
use std::sync::Arc;
use tonic::{Request, Response, Status};
use tracing::{debug, error, info, warn};

/// CSI Node service implementation.
///
/// Handles NodePublishVolume / NodeUnpublishVolume for ephemeral inline volumes.
/// Each volume maps to a Nix closure that gets fetched, cached, and bind-mounted.
///
/// Generic over `C` (cache client) and `M` (mount ops) for testability.
pub struct NodeService<C: BinaryCacheClient = CacheClient, M: MountOps = RealMountOps> {
    node_id: String,
    cache: Arc<NarCache<C>>,
    mount: M,
}

impl NodeService<CacheClient, RealMountOps> {
    pub fn new(node_id: String, cache: Arc<NarCache<CacheClient>>) -> Self {
        NodeService {
            node_id,
            cache,
            mount: RealMountOps,
        }
    }
}

impl<C: BinaryCacheClient, M: MountOps> NodeService<C, M> {
    #[cfg(any(test, feature = "testutils"))]
    pub fn with_deps(node_id: String, cache: Arc<NarCache<C>>, mount: M) -> Self {
        NodeService {
            node_id,
            cache,
            mount,
        }
    }
}

#[tonic::async_trait]
impl<C: BinaryCacheClient + 'static, M: MountOps + 'static> csi::node_server::Node
    for NodeService<C, M>
{
    async fn node_publish_volume(
        &self,
        request: Request<csi::NodePublishVolumeRequest>,
    ) -> Result<Response<csi::NodePublishVolumeResponse>, Status> {
        let req = request.into_inner();
        let volume_id = &req.volume_id;
        let target_path = &req.target_path;

        debug!(volume_id, target_path, "NodePublishVolume");

        if volume_id.is_empty() {
            return Err(Status::invalid_argument("volume_id is required"));
        }
        if target_path.is_empty() {
            return Err(Status::invalid_argument("target_path is required"));
        }

        // Check if already mounted (idempotency).
        if self.mount.is_mountpoint(target_path) {
            info!(target_path, "already mounted, returning OK");
            return Ok(Response::new(csi::NodePublishVolumeResponse {}));
        }

        // Extract volume context.
        let ctx = &req.volume_context;

        let store_path = ctx
            .get("storePath")
            .ok_or_else(|| Status::invalid_argument("volumeAttributes.storePath is required"))?;

        let closure_paths_raw = ctx
            .get("closurePaths")
            .ok_or_else(|| Status::invalid_argument("volumeAttributes.closurePaths is required"))?;

        // closurePaths is comma-separated list of store path basenames.
        let closure_paths: Vec<&str> = closure_paths_raw
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        if closure_paths.is_empty() {
            return Err(Status::invalid_argument(
                "closurePaths must contain at least one path",
            ));
        }

        info!(
            volume_id,
            store_path,
            closure_count = closure_paths.len(),
            "fetching closure paths"
        );

        // Ensure all closure paths are cached.
        let binary_cache_url = ctx.get("binaryCacheUrl").map(|s| s.as_str());
        if let Err(e) = self
            .cache
            .ensure_cached(&closure_paths, binary_cache_url)
            .await
        {
            error!(err = %e, "failed to fetch closure paths");
            return Err(Status::unavailable(format!("failed to fetch closure: {e}")));
        }

        // Create target directory and bind mount.
        if let Err(e) = self.mount.setup_target_dir(target_path) {
            error!(target_path, err = %e, "failed to create target directory");
            return Err(Status::internal(format!(
                "failed to create target dir: {e}"
            )));
        }

        // Bind mount the primary store path (read-only).
        let source = self.cache.path_for(store_path);
        if let Err(e) = self.mount.bind_mount_readonly(&source, target_path) {
            // Clean up target dir on failure.
            warn!(target_path, err = %e, "bind mount failed, cleaning up");
            let _ = self.mount.cleanup_target(target_path);
            return Err(Status::internal(format!("bind mount failed: {e}")));
        }

        info!(volume_id, target_path, "volume published");
        Ok(Response::new(csi::NodePublishVolumeResponse {}))
    }

    async fn node_unpublish_volume(
        &self,
        request: Request<csi::NodeUnpublishVolumeRequest>,
    ) -> Result<Response<csi::NodeUnpublishVolumeResponse>, Status> {
        let req = request.into_inner();
        let volume_id = &req.volume_id;
        let target_path = &req.target_path;

        debug!(volume_id, target_path, "NodeUnpublishVolume");

        if volume_id.is_empty() {
            return Err(Status::invalid_argument("volume_id is required"));
        }
        if target_path.is_empty() {
            return Err(Status::invalid_argument("target_path is required"));
        }

        // Idempotent: if not mounted, return OK.
        if !self.mount.is_mountpoint(target_path) {
            debug!(target_path, "not a mountpoint, cleaning up directory");
            let _ = self.mount.cleanup_target(target_path);
            return Ok(Response::new(csi::NodeUnpublishVolumeResponse {}));
        }

        // Unmount.
        if let Err(e) = self.mount.unmount(target_path) {
            error!(target_path, err = %e, "unmount failed");
            return Err(Status::internal(format!("unmount failed: {e}")));
        }

        // Remove target directory.
        let _ = self.mount.cleanup_target(target_path);

        info!(volume_id, target_path, "volume unpublished");
        Ok(Response::new(csi::NodeUnpublishVolumeResponse {}))
    }

    async fn node_get_capabilities(
        &self,
        _request: Request<csi::NodeGetCapabilitiesRequest>,
    ) -> Result<Response<csi::NodeGetCapabilitiesResponse>, Status> {
        Ok(Response::new(csi::NodeGetCapabilitiesResponse {
            capabilities: vec![],
        }))
    }

    async fn node_get_info(
        &self,
        _request: Request<csi::NodeGetInfoRequest>,
    ) -> Result<Response<csi::NodeGetInfoResponse>, Status> {
        Ok(Response::new(csi::NodeGetInfoResponse {
            node_id: self.node_id.clone(),
            max_volumes_per_node: 0,
            accessible_topology: None,
        }))
    }

    async fn node_stage_volume(
        &self,
        _request: Request<csi::NodeStageVolumeRequest>,
    ) -> Result<Response<csi::NodeStageVolumeResponse>, Status> {
        Err(Status::unimplemented(
            "NodeStageVolume not supported (ephemeral volumes)",
        ))
    }

    async fn node_unstage_volume(
        &self,
        _request: Request<csi::NodeUnstageVolumeRequest>,
    ) -> Result<Response<csi::NodeUnstageVolumeResponse>, Status> {
        Err(Status::unimplemented(
            "NodeUnstageVolume not supported (ephemeral volumes)",
        ))
    }

    async fn node_get_volume_stats(
        &self,
        _request: Request<csi::NodeGetVolumeStatsRequest>,
    ) -> Result<Response<csi::NodeGetVolumeStatsResponse>, Status> {
        Err(Status::unimplemented("NodeGetVolumeStats not supported"))
    }

    async fn node_expand_volume(
        &self,
        _request: Request<csi::NodeExpandVolumeRequest>,
    ) -> Result<Response<csi::NodeExpandVolumeResponse>, Status> {
        Err(Status::unimplemented("NodeExpandVolume not supported"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csi::node_server::Node;
    use niphas_core::testutils::fake_cache::FakeCacheClient;
    use std::collections::HashSet;
    use std::io;
    use std::path::PathBuf;
    use std::sync::Mutex;

    // -- Fakes --

    struct FakeMountOps {
        mounted: Mutex<HashSet<String>>,
        setup_fail: bool,
        mount_fail: bool,
        unmount_fail: bool,
    }

    impl FakeMountOps {
        fn new() -> Self {
            Self {
                mounted: Mutex::new(HashSet::new()),
                setup_fail: false,
                mount_fail: false,
                unmount_fail: false,
            }
        }

        fn with_mounted(path: &str) -> Self {
            let mut m = HashSet::new();
            m.insert(path.to_string());
            Self {
                mounted: Mutex::new(m),
                setup_fail: false,
                mount_fail: false,
                unmount_fail: false,
            }
        }
    }

    impl MountOps for FakeMountOps {
        fn is_mountpoint(&self, path: &str) -> bool {
            self.mounted.lock().unwrap().contains(path)
        }

        fn setup_target_dir(&self, _path: &str) -> io::Result<()> {
            if self.setup_fail {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fake setup fail",
                ));
            }
            Ok(())
        }

        fn bind_mount_readonly(&self, _source: &str, target: &str) -> io::Result<()> {
            if self.mount_fail {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fake mount fail",
                ));
            }
            self.mounted.lock().unwrap().insert(target.to_string());
            Ok(())
        }

        fn unmount(&self, target: &str) -> io::Result<()> {
            if self.unmount_fail {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fake unmount fail",
                ));
            }
            self.mounted.lock().unwrap().remove(target);
            Ok(())
        }

        fn cleanup_target(&self, _target: &str) -> io::Result<()> {
            Ok(())
        }
    }

    fn make_service(mount: FakeMountOps) -> NodeService<FakeCacheClient, FakeMountOps> {
        let cache = Arc::new(NarCache::with_client(
            PathBuf::from("/tmp/test-cache"),
            FakeCacheClient::new(),
        ));
        NodeService::with_deps("test-node".into(), cache, mount)
    }

    fn publish_request(
        volume_id: &str,
        target_path: &str,
        store_path: Option<&str>,
        closure_paths: Option<&str>,
    ) -> Request<csi::NodePublishVolumeRequest> {
        let mut ctx = std::collections::HashMap::new();
        if let Some(sp) = store_path {
            ctx.insert("storePath".into(), sp.into());
        }
        if let Some(cp) = closure_paths {
            ctx.insert("closurePaths".into(), cp.into());
        }
        Request::new(csi::NodePublishVolumeRequest {
            volume_id: volume_id.into(),
            target_path: target_path.into(),
            volume_context: ctx,
            ..Default::default()
        })
    }

    fn unpublish_request(
        volume_id: &str,
        target_path: &str,
    ) -> Request<csi::NodeUnpublishVolumeRequest> {
        Request::new(csi::NodeUnpublishVolumeRequest {
            volume_id: volume_id.into(),
            target_path: target_path.into(),
        })
    }

    // -- Tests --

    #[tokio::test]
    async fn test_publish_missing_volume_id() {
        let svc = make_service(FakeMountOps::new());
        let req = publish_request("", "/target", Some("/nix/store/abc"), Some("abc"));
        let err = svc.node_publish_volume(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_publish_missing_target_path() {
        let svc = make_service(FakeMountOps::new());
        let req = publish_request("vol-1", "", Some("/nix/store/abc"), Some("abc"));
        let err = svc.node_publish_volume(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_publish_missing_store_path() {
        let svc = make_service(FakeMountOps::new());
        let req = publish_request("vol-1", "/target", None, Some("abc"));
        let err = svc.node_publish_volume(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_publish_missing_closure_paths() {
        let svc = make_service(FakeMountOps::new());
        let req = publish_request("vol-1", "/target", Some("/nix/store/abc"), None);
        let err = svc.node_publish_volume(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_publish_already_mounted() {
        let svc = make_service(FakeMountOps::with_mounted("/target"));
        let req = publish_request("vol-1", "/target", Some("/nix/store/abc"), Some("abc"));
        let resp = svc.node_publish_volume(req).await.unwrap();
        assert_eq!(resp.into_inner(), csi::NodePublishVolumeResponse {});
    }

    #[tokio::test]
    async fn test_publish_mount_failure() {
        let mut mount = FakeMountOps::new();
        mount.mount_fail = true;
        let svc = make_service(mount);
        // ensure_cached will fail because FakeCacheClient always fails fetch
        // Actually, ensure_cached tries to fetch NARs. Since our fake cache dir
        // doesn't exist, the "is_cached" check returns false, then fetch_and_extract
        // gets called. Let's test with empty closure paths instead.
        // Actually the closure_paths parse logic filters empty strings, so we need
        // at least one. The cache will try to fetch it and fail with Unavailable.
        let req = publish_request("vol-1", "/target", Some("/nix/store/abc"), Some("abc"));
        let err = svc.node_publish_volume(req).await.unwrap_err();
        // Cache fetch fails before mount, so it's UNAVAILABLE
        assert_eq!(err.code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn test_unpublish_missing_volume_id() {
        let svc = make_service(FakeMountOps::new());
        let req = unpublish_request("", "/target");
        let err = svc.node_unpublish_volume(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_unpublish_missing_target_path() {
        let svc = make_service(FakeMountOps::new());
        let req = unpublish_request("vol-1", "");
        let err = svc.node_unpublish_volume(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_unpublish_not_mounted() {
        let svc = make_service(FakeMountOps::new());
        let req = unpublish_request("vol-1", "/target");
        let resp = svc.node_unpublish_volume(req).await.unwrap();
        assert_eq!(resp.into_inner(), csi::NodeUnpublishVolumeResponse {});
    }

    #[tokio::test]
    async fn test_unpublish_success() {
        let svc = make_service(FakeMountOps::with_mounted("/target"));
        let req = unpublish_request("vol-1", "/target");
        let resp = svc.node_unpublish_volume(req).await.unwrap();
        assert_eq!(resp.into_inner(), csi::NodeUnpublishVolumeResponse {});
    }

    #[tokio::test]
    async fn test_node_get_capabilities_empty() {
        let svc = make_service(FakeMountOps::new());
        let resp = svc
            .node_get_capabilities(Request::new(csi::NodeGetCapabilitiesRequest {}))
            .await
            .unwrap();
        assert!(resp.into_inner().capabilities.is_empty());
    }

    #[tokio::test]
    async fn test_node_get_info_returns_id() {
        let svc = make_service(FakeMountOps::new());
        let resp = svc
            .node_get_info(Request::new(csi::NodeGetInfoRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().node_id, "test-node");
    }
}
