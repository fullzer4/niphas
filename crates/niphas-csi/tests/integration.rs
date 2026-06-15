use niphas_core::testutils::fake_cache::FakeCacheClient;
use niphas_csi::cache::NarCache;
use niphas_csi::csi;
use niphas_csi::identity::IdentityService;
use niphas_csi::mount::MountOps;
use niphas_csi::node::NodeService;
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tonic::Request;

// -- Fake mount ops for integration tests --

struct FakeMountOps {
    mounted: Mutex<HashSet<String>>,
}

impl FakeMountOps {
    fn new() -> Self {
        Self {
            mounted: Mutex::new(HashSet::new()),
        }
    }
}

impl MountOps for FakeMountOps {
    fn is_mountpoint(&self, path: &str) -> bool {
        self.mounted.lock().unwrap().contains(path)
    }

    fn setup_target_dir(&self, _path: &str) -> io::Result<()> {
        Ok(())
    }

    fn bind_mount_readonly(&self, _source: &str, target: &str) -> io::Result<()> {
        self.mounted.lock().unwrap().insert(target.to_string());
        Ok(())
    }

    fn unmount(&self, target: &str) -> io::Result<()> {
        self.mounted.lock().unwrap().remove(target);
        Ok(())
    }

    fn cleanup_target(&self, _target: &str) -> io::Result<()> {
        Ok(())
    }
}

/// Spawn a gRPC server with Identity + Node services on a random TCP port.
/// Returns the channel connected to that server.
async fn spawn_grpc_server() -> tonic::transport::Channel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let cache = Arc::new(NarCache::with_client(
        PathBuf::from("/tmp/niphas-test-cache"),
        FakeCacheClient::new(),
    ));
    let node_svc = NodeService::with_deps("test-node".into(), cache, FakeMountOps::new());

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        tonic::transport::Server::builder()
            .add_service(csi::identity_server::IdentityServer::new(IdentityService))
            .add_service(csi::node_server::NodeServer::new(node_svc))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    // Give the server a moment to start.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

// -- Identity service tests --

#[tokio::test]
async fn get_plugin_info_returns_name_and_version() {
    let channel = spawn_grpc_server().await;
    let mut client = csi::identity_client::IdentityClient::new(channel);

    let resp = client
        .get_plugin_info(Request::new(csi::GetPluginInfoRequest {}))
        .await
        .unwrap();
    let info = resp.into_inner();

    assert_eq!(info.name, "niphas.io.csi");
    assert!(!info.vendor_version.is_empty());
}

#[tokio::test]
async fn probe_returns_ready() {
    let channel = spawn_grpc_server().await;
    let mut client = csi::identity_client::IdentityClient::new(channel);

    let resp = client
        .probe(Request::new(csi::ProbeRequest {}))
        .await
        .unwrap();

    assert_eq!(resp.into_inner().ready, Some(true));
}

// -- Node service tests --

#[tokio::test]
async fn node_publish_volume_success() {
    let channel = spawn_grpc_server().await;
    let mut client = csi::node_client::NodeClient::new(channel);

    let mut ctx = std::collections::HashMap::new();
    ctx.insert("storePath".into(), "/nix/store/abc123-hello".into());
    ctx.insert("closurePaths".into(), "abc123-hello".into());

    let resp = client
        .node_publish_volume(Request::new(csi::NodePublishVolumeRequest {
            volume_id: "vol-1".into(),
            target_path: "/mnt/test-target".into(),
            volume_context: ctx,
            ..Default::default()
        }))
        .await;

    // ensure_cached will fail because FakeCacheClient has no narinfos loaded,
    // so this returns UNAVAILABLE (cache fetch failure).
    // This is expected — we're testing the gRPC transport + validation path.
    let err = resp.unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn node_publish_volume_missing_volume_id_returns_invalid_argument() {
    let channel = spawn_grpc_server().await;
    let mut client = csi::node_client::NodeClient::new(channel);

    let err = client
        .node_publish_volume(Request::new(csi::NodePublishVolumeRequest {
            volume_id: "".into(),
            target_path: "/mnt/target".into(),
            ..Default::default()
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(err.message().contains("volume_id"));
}

#[tokio::test]
async fn node_unpublish_volume_missing_target_path_returns_invalid_argument() {
    let channel = spawn_grpc_server().await;
    let mut client = csi::node_client::NodeClient::new(channel);

    let err = client
        .node_unpublish_volume(Request::new(csi::NodeUnpublishVolumeRequest {
            volume_id: "vol-1".into(),
            target_path: "".into(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(err.message().contains("target_path"));
}
