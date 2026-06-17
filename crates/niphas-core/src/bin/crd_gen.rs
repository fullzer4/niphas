use kube::CustomResourceExt;
use niphas_core::crd::NiphasWorkload;

fn main() {
    print!("{}", serde_yaml::to_string(&NiphasWorkload::crd()).unwrap());
}
