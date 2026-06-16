# Niphas -- Design de Testabilidade

## Problema

4.569 linhas de Rust, 32 testes cobrindo apenas parsing/crypto. Zero testes para operator, CSI, eval pipeline. A causa raiz nao e falta de testes -- e que o codigo mistura logica de negocio com IO, tornando impossivel testar sem infraestrutura real (cluster K8s, root, nix binary, rede).

```
Hoje:
  reconcile() --> constroi JSON + chama K8s API + chama HTTP eval      (tudo junto)
  node_publish() --> valida input + chama cache HTTP + chama libc mount (tudo junto)
  evaluate() --> valida allowlist + spawn nix + resolve closure HTTP    (tudo junto)

Queremos:
  reconcile() --> build_deployment() [puro] + apply() [IO]
  node_publish() --> validate() [puro] + cache.ensure() [injetavel] + mount [injetavel]
  evaluate() --> validate() [puro] + eval [injetavel] + resolve [injetavel]
```

---

## Rust Patterns para Testabilidade

### 1. Sans-IO: Separar logica de efeitos

O pattern mais importante. A ideia: funcoes de negocio recebem dados e retornam dados. Nunca fazem IO diretamente.

**Antes** (resources.rs atual):
```rust
async fn apply_deployment(ctx: &Context, workload: &NiphasWorkload, eval: &EvalResult) -> Result<()> {
    // 200 linhas construindo JSON
    let json = serde_json::json!({ /* ... */ });
    // IO misturado no fim
    let api: Api<Deployment> = Api::namespaced(ctx.client.clone(), ns);
    api.patch(name, &params, &Patch::Apply(&json)).await?;
    Ok(())
}
```

**Depois**:
```rust
// PURO -- recebe dados, retorna dados. Testavel sem K8s.
fn build_deployment(workload: &NiphasWorkload, eval: &EvalResult, name: &str, ns: &str) -> serde_json::Value {
    serde_json::json!({ /* ... */ })
}

// IO fino -- so aplica
async fn apply_resource(ctx: &Context, name: &str, ns: &str, manifest: &serde_json::Value) -> Result<()> {
    let api: Api<DynamicObject> = Api::namespaced(ctx.client.clone(), ns);
    api.patch(name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(manifest)).await?;
    Ok(())
}
```

Teste:
```rust
#[test]
fn deployment_always_has_nix_store_node_selector() {
    let json = build_deployment(&workload, &eval, "app", "ns");
    assert_eq!(json["spec"]["template"]["spec"]["nodeSelector"]["niphas.io/store"], "true");
}
```

**Onde aplicar no Niphas:**

| Arquivo | Funcao atual | Separar em |
|---------|-------------|------------|
| `resources.rs` | `apply_deployment` | `build_deployment` (puro) + `apply_resource` (IO) |
| `resources.rs` | `apply_service` | `build_service` (puro) + `apply_resource` (IO) |
| `resources.rs` | `apply_ingress` | `build_ingress` (puro) + `apply_resource` (IO) |
| `resources.rs` | `apply_pdb` | `build_pdb` (puro) + `apply_resource` (IO) |
| `reconciler.rs` | `set_phase` | `build_status_patch` (puro) + `apply_status` (IO) |
| `reconciler.rs` | `set_failed` | `build_failed_patch` (puro) + `apply_status` (IO) |
| `evaluator.rs` | `nix_eval` (subprocess) | `build_nix_expr` (puro) + `run_nix` (IO) |

---

### 2. Trait Injection: Generics com static dispatch

Para dependencias que precisam ser substituidas em testes. Preferir generics sobre `dyn Trait` quando possivel (zero-cost, sem vtable).

**Pattern: trait + generic no struct**

```rust
// Definir a capability
#[async_trait]
pub trait BinaryCacheClient: Send + Sync {
    async fn fetch_narinfo(&self, store_path: &StorePath) -> Result<NarInfo, NiphasError>;
    async fn fetch_nar(&self, narinfo: &NarInfo) -> Result<Bytes, NiphasError>;
    async fn fetch_narinfo_by_hash(&self, hash: &str) -> Result<NarInfo, NiphasError>;
}

// Struct real implementa
impl BinaryCacheClient for CacheClient { /* reqwest calls */ }

// Consumidores sao genericos com default
pub struct NarCache<C: BinaryCacheClient = CacheClient> {
    cache_dir: PathBuf,
    client: C,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

// Em producao: NarCache<CacheClient> (default, nao precisa anotar)
// Em testes: NarCache<FakeCacheClient>
```

**Tres traits a extrair no Niphas:**

#### Trait 1: `BinaryCacheClient` (maior impacto)

Desbloqueia testes de: `closure.rs`, `evaluator.rs`, CSI `cache.rs`.

```rust
// niphas-core/src/nix/cache_client.rs
#[async_trait]
pub trait BinaryCacheClient: Send + Sync {
    async fn fetch_narinfo(&self, store_path: &StorePath) -> Result<NarInfo, NiphasError>;
    async fn fetch_nar(&self, narinfo: &NarInfo) -> Result<Bytes, NiphasError>;
    async fn fetch_narinfo_by_hash(&self, hash: &str) -> Result<NarInfo, NiphasError>;
}
```

Consumidores mudam:
```rust
// closure.rs: antes
pub async fn resolve_closure(client: &CacheClient, ...) -> ...
// closure.rs: depois
pub async fn resolve_closure<C: BinaryCacheClient>(client: &C, ...) -> ...
```

#### Trait 2: `MountOps` (CSI)

Desbloqueia testes de: `node.rs` (12 branches de publish/unpublish).

```rust
// niphas-csi/src/mount.rs
pub trait MountOps: Send + Sync {
    fn is_mountpoint(&self, path: &str) -> bool;
    fn setup_target_dir(&self, path: &str) -> io::Result<()>;
    fn bind_mount_readonly(&self, source: &str, target: &str) -> io::Result<()>;
    fn unmount(&self, target: &str) -> io::Result<()>;
    fn cleanup_target(&self, target: &str) -> io::Result<()>;
}

pub struct LinuxMountOps;
impl MountOps for LinuxMountOps { /* libc::mount calls */ }

// NodeService fica generico
pub struct NodeService<M: MountOps = LinuxMountOps> {
    node_id: String,
    cache: Arc<NarCache>,
    mount: M,
}
```

#### Trait 3: `NixEval` (eval subprocess)

Desbloqueia testes de: `evaluator.rs` (pipeline completo).

```rust
// niphas-eval/src/evaluator.rs
#[async_trait]
pub trait NixEval: Send + Sync {
    async fn eval(&self, pinned_ref: &str, attribute: &str) -> Result<NixEvalResult, AppError>;
}

// Producao: chama tokio::process::Command("nix")
pub struct SubprocessNixEval { timeout: Duration }
impl NixEval for SubprocessNixEval { /* spawn nix eval */ }

// Evaluator generico
pub struct Evaluator<E: NixEval = SubprocessNixEval, C: BinaryCacheClient = CacheClient> {
    config: NiphasConfig,
    nix: E,
    cache: C,
    warm: AtomicBool,
}
```

---

### 3. Fakes sobre Mocks

Preferir **fakes** (implementacoes simplificadas que funcionam) sobre **mocks** (verificam chamadas). Fakes sao mais robustos a refactoring e capturam bugs reais.

```rust
// FAKE -- implementacao in-memory que funciona
struct FakeCacheClient {
    entries: HashMap<String, NarInfo>,
}

#[async_trait]
impl BinaryCacheClient for FakeCacheClient {
    async fn fetch_narinfo(&self, sp: &StorePath) -> Result<NarInfo, NiphasError> {
        self.entries.get(&sp.hash_str())
            .cloned()
            .ok_or(NiphasError::StorePathNotCached(sp.hash_str()))
    }
    async fn fetch_nar(&self, _ni: &NarInfo) -> Result<Bytes, NiphasError> {
        Ok(Bytes::from_static(b"fake"))
    }
    async fn fetch_narinfo_by_hash(&self, hash: &str) -> Result<NarInfo, NiphasError> {
        self.entries.get(hash)
            .cloned()
            .ok_or(NiphasError::StorePathNotCached(hash.into()))
    }
}

// FAKE mount -- sem syscalls, tracking de chamadas
struct FakeMountOps {
    mounted: Mutex<HashSet<String>>,
}

impl MountOps for FakeMountOps {
    fn is_mountpoint(&self, path: &str) -> bool {
        self.mounted.lock().unwrap().contains(path)
    }
    fn bind_mount_readonly(&self, _source: &str, target: &str) -> io::Result<()> {
        self.mounted.lock().unwrap().insert(target.into());
        Ok(())
    }
    fn unmount(&self, target: &str) -> io::Result<()> {
        self.mounted.lock().unwrap().remove(target);
        Ok(())
    }
    fn setup_target_dir(&self, _: &str) -> io::Result<()> { Ok(()) }
    fn cleanup_target(&self, _: &str) -> io::Result<()> { Ok(()) }
}
```

**Quando usar mock (mockall) em vez de fake:**
- Quando voce precisa verificar que uma funcao foi chamada N vezes
- Quando precisa verificar a ordem das chamadas
- No Niphas: apenas para testes do reconciler verificando que status patches especificos foram enviados

---

### 4. Test Fixtures: Builder Pattern

Construir CRDs e structs de teste e verboso. Builders com defaults sensiveis eliminam boilerplate.

```rust
// niphas-core/src/testutils.rs (feature-gated)

pub struct WorkloadBuilder {
    name: String,
    ns: String,
    flake_ref: String,
    replicas: i32,
    phase: Option<String>,
    store_path: Option<String>,
    generation: i64,
    finalizers: Vec<String>,
    deletion: bool,
}

impl WorkloadBuilder {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            ns: "default".into(),
            flake_ref: "github:test/repo#pkg".into(),
            replicas: 1,
            phase: None,
            store_path: None,
            generation: 1,
            finalizers: vec![],
            deletion: false,
        }
    }

    pub fn replicas(mut self, n: i32) -> Self { self.replicas = n; self }
    pub fn phase(mut self, p: &str) -> Self { self.phase = Some(p.into()); self }
    pub fn evaluated(mut self, store_path: &str) -> Self {
        self.phase = Some("Evaluated".into());
        self.store_path = Some(store_path.into());
        self
    }
    pub fn with_finalizer(mut self) -> Self {
        self.finalizers.push("niphas.io/workload-cleanup".into());
        self
    }
    pub fn deleting(mut self) -> Self { self.deletion = true; self }
    pub fn generation(mut self, g: i64) -> Self { self.generation = g; self }

    pub fn build(self) -> NiphasWorkload { /* constroi o CRD */ }
}

// Uso em testes:
let w = WorkloadBuilder::new("app").replicas(3).evaluated("/nix/store/abc-hello").build();
```

**Cross-crate sharing**: usar feature flag `testutils` em vez de `#[cfg(test)]`:

```toml
# niphas-core/Cargo.toml
[features]
testutils = []

# niphas-operator/Cargo.toml
[dev-dependencies]
niphas-core = { workspace = true, features = ["testutils"] }
```

Isso resolve o problema de `#[cfg(test)]` nao ser visivel entre crates (cargo issue #8379).

---

### 5. Snapshot Testing com insta

Para outputs complexos (JSON de K8s resources, respostas HTTP, schemas CRD), snapshots sao melhores que assertions manuais. Eles capturam regressoes que voce nao pensou em testar.

```rust
use insta::assert_json_snapshot;

#[test]
fn deployment_snapshot_basic() {
    let w = WorkloadBuilder::new("hello").replicas(1).build();
    let e = EvalResultBuilder::new("/nix/store/abc-hello").build();
    let json = build_deployment(&w, &e, "hello", "default");

    assert_json_snapshot!("deployment-basic", json);
    // Primeira execucao: cria snapshots/deployment-basic.snap
    // Proximas: compara. cargo insta review para aceitar mudancas.
}

#[test]
fn deployment_snapshot_multi_replica() {
    let w = WorkloadBuilder::new("hello").replicas(3).build();
    let e = EvalResultBuilder::new("/nix/store/abc-hello").build();
    let json = build_deployment(&w, &e, "hello", "default");

    assert_json_snapshot!("deployment-multi-replica", json);
    // Verifica: topologySpreadConstraints presente, PDB criado
}
```

**Redactions para campos nao-deterministicos:**
```rust
assert_json_snapshot!(status, {
    ".lastTransitionTime" => "[timestamp]",
    ".observedGeneration" => "[gen]",
});
```

**Onde usar no Niphas:**

| Alvo | Por que snapshot |
|------|-----------------|
| `build_deployment` JSON | 200+ linhas de construcao condicional, regressoes sutis |
| `build_service` JSON | Mapeamento de portas, tipo de servico |
| `build_ingress` JSON | TLS, paths, hosts -- facil quebrar |
| `build_pdb` JSON | Condicional em replicas >= 2 |
| `AppError::into_response()` | Status codes + JSON body por variante |
| CRD schema (`NiphasWorkload::crd()`) | Schema drift entre versoes |
| Status patches do reconciler | JSON patches complexos |

---

### 6. Property Testing com proptest

Para parsers de protocolo e codecs, property tests encontram edge cases que testes manuais nunca cobrem. Especialmente valioso para o formato NAR (binary protocol com invariantes estritas).

```rust
use proptest::prelude::*;

// Gerar caracteres validos de nix-base32
fn nix_base32_char() -> impl Strategy<Value = char> {
    prop::sample::select("0123456789abcdfghijklmnpqrsvwxyz".chars().collect::<Vec<_>>())
}

// Gerar store paths validos
fn arb_store_path() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(nix_base32_char(), 32),
        "[a-z][a-z0-9._-]{0,20}"
    ).prop_map(|(hash, name)| {
        let h: String = hash.into_iter().collect();
        format!("/nix/store/{h}-{name}")
    })
}

proptest! {
    // Roundtrip: parse(display(x)) == x
    #[test]
    fn store_path_roundtrip(path in arb_store_path()) {
        let parsed = StorePath::parse(&path).unwrap();
        let roundtripped = StorePath::parse(&parsed.to_path_string()).unwrap();
        prop_assert_eq!(parsed.hash_str(), roundtripped.hash_str());
        prop_assert_eq!(parsed.name(), roundtripped.name());
    }

    // Nix-base32: decode(encode(x)) == x
    #[test]
    fn nix_base32_roundtrip(data in prop::collection::vec(any::<u8>(), 0..64)) {
        let encoded = nix_base32_encode(&data);
        let decoded = nix_base32_decode(&encoded).unwrap();
        prop_assert_eq!(data, decoded);
    }

    // narinfo parser nunca faz panic em input arbitrario
    #[test]
    fn narinfo_no_panic(input in "\\PC*") {
        let _ = NarInfo::parse(&input); // Err e ok, panic nao
    }

    // NAR entry names: validate rejeita todos os padroes proibidos
    #[test]
    fn nar_entry_name_rejects_forbidden(
        name in prop_oneof![
            Just("".to_string()),
            Just(".".to_string()),
            Just("..".to_string()),
            ".*/.+",           // contem /
            ".*\0.+",          // contem null
        ]
    ) {
        prop_assert!(validate_entry_name(&name).is_err());
    }
}
```

**Invariantes property-testaveis no Niphas:**

| Modulo | Invariante | Property |
|--------|-----------|----------|
| `hash.rs` | base32 roundtrip | `decode(encode(x)) == x` |
| `store_path.rs` | parse roundtrip | `parse(display(x)) == x` |
| `nar.rs` | padding e 0..7 bytes de zeros | `pad_len(n) == (8 - n%8) % 8` |
| `nar.rs` | entries devem ser sorted | entries fora de ordem -> Err |
| `nar.rs` | depth limit funciona | depth > max -> Err |
| `narinfo.rs` | no panic on arbitrary input | `parse(random) != panic` |
| `config.rs` | duration parse roundtrip | `parse(format(d)) == d` |

---

### 7. kube-rs Operator Testing: tower_test

O pattern canonico de [controller-rs](https://github.com/kube-rs/controller-rs). `kube::Client` aceita qualquer `tower::Service` -- injetamos um mock.

```rust
use tower_test::mock;
use kube::client::Body;

fn mock_client() -> (kube::Client, mock::Handle<http::Request<Body>, http::Response<Body>>) {
    let (svc, handle) = mock::pair();
    let client = kube::Client::new(svc, "default");
    (client, handle)
}

// Verificador de cenario
struct ApiServerVerifier(mock::Handle<http::Request<Body>, http::Response<Body>>);

impl ApiServerVerifier {
    async fn expect_status_patch(mut self, expected_phase: &str) -> Self {
        let (req, send) = self.0.next_request().await.expect("expected API call");
        assert_eq!(req.method(), http::Method::PATCH);
        assert!(req.uri().to_string().contains("/status"));
        // Verificar body contem a phase esperada
        send.send_response(http::Response::builder()
            .body(Body::from("{}"))
            .unwrap());
        self
    }
}

#[tokio::test]
async fn reconcile_new_workload_adds_finalizer() {
    let (client, handle) = mock_client();
    let ctx = Arc::new(Context::test(client));
    let workload = WorkloadBuilder::new("app").build(); // sem finalizer

    let verifier = ApiServerVerifier(handle);
    let mock_task = tokio::spawn(async move {
        verifier.expect_finalizer_patch().await;
    });

    let result = reconcile(Arc::new(workload), ctx).await.unwrap();
    assert_eq!(result, Action::requeue(Duration::ZERO));
    mock_task.await.unwrap();
}
```

---

### 8. Tonic gRPC Testing: Chamada direta

Servicos tonic sao traits Rust. Chamar os metodos diretamente sem transporte de rede:

```rust
use tonic::Request;
use crate::csi::identity_server::Identity;

#[tokio::test]
async fn identity_returns_correct_name() {
    let svc = IdentityService;
    let resp = svc.get_plugin_info(Request::new(GetPluginInfoRequest {}))
        .await
        .unwrap();
    assert_eq!(resp.into_inner().name, "niphas.io.csi");
}

#[tokio::test]
async fn publish_rejects_empty_volume_id() {
    let svc = NodeService::new("node-1".into(), cache, FakeMountOps::new());
    let req = NodePublishVolumeRequest {
        volume_id: "".into(),
        ..Default::default()
    };
    let err = svc.node_publish_volume(Request::new(req)).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}
```

---

### 9. Axum Handler Testing: tower oneshot

```rust
use axum::body::Body;
use http::Request;
use tower::ServiceExt;

#[tokio::test]
async fn healthz_returns_200() {
    let app = health_router(HealthState { ready: Arc::new(AtomicBool::new(false)) });
    let resp = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn readyz_returns_503_when_cold() {
    let app = health_router(HealthState { ready: Arc::new(AtomicBool::new(false)) });
    let resp = app
        .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}
```

---

## Tiering

### Tier 1 -- Fast (< 5s, todo commit)

Sem rede, sem K8s, sem root, sem nix. Usa fakes, builders, snapshots.

```bash
cargo test --workspace
```

### Tier 2 -- Integration (CI com setup)

Precisa de infra. Marcados `#[ignore]`.

```bash
cargo test --workspace -- --ignored
```

| Tier | Requer | Exemplos |
|------|--------|----------|
| T1 | Nada | builders puros, closure BFS com fake cache, CSI publish com fake mount |
| T2 | kind cluster | reconciler contra API real, CRD lifecycle |
| T2 | nix binary | nix eval real, closure resolution contra cache.nixos.org |
| T2 | root | bind mount + unmount real |

---

## Organizacao de Arquivos

```
crates/
  niphas-core/
    src/
      testutils.rs          # feature "testutils": builders, fakes, helpers
      nix/
        cache_client.rs     # BinaryCacheClient trait + CacheClient impl
        closure.rs          # generico sobre BinaryCacheClient, inline tests com fake
        nar.rs              # inline tests existentes + proptest
        hash.rs             # inline tests existentes + proptest roundtrip
        narinfo.rs          # inline tests existentes
        store_path.rs       # inline tests existentes + proptest roundtrip
        signature.rs        # inline tests existentes
      config.rs             # inline tests novos
    Cargo.toml              # [features] testutils = []

  niphas-operator/
    src/
      reconciler.rs         # inline tests para funcoes puras
      resources.rs          # build_* puras + inline tests + snapshots
      eval.rs               # inline tests para EvalResult::from_status, resolved_command
      health.rs             # inline tests com oneshot
      context.rs            # http_client adicionado
    tests/
      reconciler_mock.rs    # tower_test scenarios (T1)
      kind_integration.rs   # #[ignore] (T2)
    Cargo.toml              # [dev-dependencies] tower-test, insta, niphas-core/testutils

  niphas-eval/
    src/
      evaluator.rs          # generico sobre NixEval + BinaryCacheClient
      error.rs              # inline tests + snapshots
      handlers.rs           # inline tests com oneshot
    tests/
      eval_integration.rs   # #[ignore] (T2)

  niphas-csi/
    src/
      mount.rs              # MountOps trait + LinuxMountOps
      node.rs               # generico sobre MountOps, inline tests com FakeMountOps
      identity.rs           # inline tests (chamada direta tonic)
      cache.rs              # generico sobre BinaryCacheClient
    tests/
      mount_integration.rs  # #[ignore] (T2)
```

---

## Dependencias de Teste

```toml
# Cargo.toml (workspace)
[workspace.dependencies]
async-trait = "0.1"
proptest = "1"
insta = { version = "1", features = ["yaml", "json"] }
tower-test = "0.4"
```

```toml
# niphas-core/Cargo.toml
[dependencies]
async-trait = { workspace = true }  # BinaryCacheClient trait e publico

[dev-dependencies]
proptest = { workspace = true }
insta = { workspace = true }
tokio = { workspace = true, features = ["test-util"] }
```

```toml
# niphas-operator/Cargo.toml
[dev-dependencies]
tower-test = { workspace = true }
tower = { workspace = true, features = ["util"] }
insta = { workspace = true }
niphas-core = { workspace = true, features = ["testutils"] }
http = "1"
```

```toml
# niphas-csi/Cargo.toml
[dev-dependencies]
niphas-core = { workspace = true, features = ["testutils"] }
```

```toml
# niphas-eval/Cargo.toml
[dev-dependencies]
tower = { workspace = true, features = ["util"] }
niphas-core = { workspace = true, features = ["testutils"] }
http = "1"
http-body-util = "0.1"
```

---

## Ordem de Execucao

```
Passo 1: Foundation
  1.1  async-trait + BinaryCacheClient trait em cache_client.rs
  1.2  Atualizar closure.rs para generico <C: BinaryCacheClient>
  1.3  Feature "testutils" com WorkloadBuilder + FakeCacheClient
  1.4  Testes de closure com FakeCacheClient (8 testes)

Passo 2: Operator
  2.1  Split build_*/apply_* em resources.rs
  2.2  Snapshot tests dos builders (insta)
  2.3  Testes puros: needs_eval, has_finalizer, error_policy
  2.4  Testes puros: EvalResult::from_status, resolved_command
  2.5  Health handler tests com oneshot
  2.6  HTTP client no Context

Passo 3: CSI
  3.1  MountOps trait + LinuxMountOps
  3.2  NodeService<M: MountOps> generico
  3.3  FakeMountOps + testes node publish/unpublish
  3.4  Identity service tests (chamada direta)

Passo 4: Eval
  4.1  NixEval trait + SubprocessNixEval
  4.2  Evaluator<E, C> generico
  4.3  Error response tests + snapshots
  4.4  Handler tests com oneshot

Passo 5: Property Tests
  5.1  proptest para hash roundtrip
  5.2  proptest para store_path roundtrip
  5.3  proptest para narinfo no-panic
  5.4  proptest para NAR entry name validation

Passo 6: Tier 2
  6.1  mount_integration.rs (#[ignore])
  6.2  eval_integration.rs (#[ignore])
  6.3  kind_integration.rs (#[ignore])
```

## Resultado Esperado

| Metrica | Antes | Depois |
|---------|-------|--------|
| Testes Tier 1 | 32 | ~100 |
| Crates com testes | 2/5 | 4/5 |
| Tempo Tier 1 | <1s | <5s |
| Traits extraidos | 0 | 3 |
| Snapshot tests | 0 | ~15 |
| Property tests | 0 | ~8 |
| Test builders | 0 | WorkloadBuilder, EvalResultBuilder, NarInfoBuilder |
