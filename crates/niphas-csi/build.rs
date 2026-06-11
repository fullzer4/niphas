fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::compile_protos("../../proto/csi/v1/csi.proto")?;
    Ok(())
}
