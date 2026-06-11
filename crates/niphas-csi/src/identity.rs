use crate::csi;
use tonic::{Request, Response, Status};

/// CSI Identity service implementation.
pub struct IdentityService;

#[tonic::async_trait]
impl csi::identity_server::Identity for IdentityService {
    async fn get_plugin_info(
        &self,
        _request: Request<csi::GetPluginInfoRequest>,
    ) -> Result<Response<csi::GetPluginInfoResponse>, Status> {
        Ok(Response::new(csi::GetPluginInfoResponse {
            name: "niphas.io.csi".into(),
            vendor_version: env!("CARGO_PKG_VERSION").into(),
            manifest: Default::default(),
        }))
    }

    async fn get_plugin_capabilities(
        &self,
        _request: Request<csi::GetPluginCapabilitiesRequest>,
    ) -> Result<Response<csi::GetPluginCapabilitiesResponse>, Status> {
        // No controller service, no volume expansion -- ephemeral inline only.
        Ok(Response::new(csi::GetPluginCapabilitiesResponse {
            capabilities: vec![],
        }))
    }

    async fn probe(
        &self,
        _request: Request<csi::ProbeRequest>,
    ) -> Result<Response<csi::ProbeResponse>, Status> {
        Ok(Response::new(csi::ProbeResponse {
            ready: Some(true),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csi::identity_server::Identity;

    #[tokio::test]
    async fn test_get_plugin_info() {
        let svc = IdentityService;
        let resp = svc
            .get_plugin_info(Request::new(csi::GetPluginInfoRequest {}))
            .await
            .unwrap();
        let info = resp.into_inner();
        assert_eq!(info.name, "niphas.io.csi");
        assert!(!info.vendor_version.is_empty());
    }

    #[tokio::test]
    async fn test_get_plugin_capabilities_empty() {
        let svc = IdentityService;
        let resp = svc
            .get_plugin_capabilities(Request::new(csi::GetPluginCapabilitiesRequest {}))
            .await
            .unwrap();
        assert!(resp.into_inner().capabilities.is_empty());
    }

    #[tokio::test]
    async fn test_probe_ready() {
        let svc = IdentityService;
        let resp = svc
            .probe(Request::new(csi::ProbeRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().ready, Some(true));
    }
}
