# Niphas -- Problemas Conhecidos e Plano de Correção

Análise completa da codebase em 2026-06-09. 80 testes passando, 0 warnings.

---

## Severidade: Crítico

### P1. Injeção de expressão Nix via flake_ref/attribute

**Arquivo:** `niphas-eval/src/evaluator.rs:92-98`

O `flake_ref` e `attribute` do usuário são interpolados diretamente numa string Nix:

```rust
let expr = format!(
    r#"let drv = (builtins.getFlake "{pinned_ref}").{attribute}; in ..."#
);
```

Um flake ref contendo `"` ou escape sequences Nix pode injetar expressões arbitrárias.
O allowlist (glob matching) valida apenas o padrão, não o conteúdo de caracteres especiais.

**Impacto:** Execução arbitrária de código Nix no node do eval service.

**Correção:**

1. Validar `flake_ref` com regex restrita: `^[a-zA-Z][a-zA-Z0-9+\-\.]*:[a-zA-Z0-9/_\-\.]+$`
2. Validar `attribute` com regex: `^[a-zA-Z_][a-zA-Z0-9_\-]*(\.[a-zA-Z_][a-zA-Z0-9_\-]*)*$`
3. Rejeitar qualquer input com `"`, `\`, `$`, `;`, `(`, `)` antes da interpolação
4. Adicionar funções `validate_flake_ref()` e `validate_attribute()` em `niphas-core/src/eval.rs`

```
niphas-core/src/eval.rs      -- adicionar validate_flake_ref(), validate_attribute()
niphas-eval/src/evaluator.rs -- chamar validações antes de construir expr
```

---

### P2. Sem leader election no operator

**Arquivo:** `niphas-operator/src/main.rs`

Múltiplas réplicas do operator reconciliam simultaneamente, causando chamadas eval
duplicadas, conflitos de status, e race conditions no SSA.

**Impacto:** Comportamento indefinido em HA. Recursos duplicados ou corrompidos.

**Correção:**

Usar `kube::runtime::Controller` com `LeaderElection` via `coordination.k8s.io/v1` Lease.
O `kube-runtime` já suporta isso nativamente.

```rust
// main.rs
use kube::runtime::watcher::Config as WatcherConfig;

let lease = kube::runtime::controller::LeaseConfig {
    lease_name: "niphas-operator-leader".into(),
    lease_namespace: std::env::var("POD_NAMESPACE").unwrap_or("niphas-system".into()),
    identity: std::env::var("POD_NAME").unwrap_or_else(|_| hostname()),
    lease_duration: Duration::from_secs(15),
    renew_deadline: Duration::from_secs(10),
    retry_period: Duration::from_secs(2),
};
```

Alternativa mais simples (fase 1): usar `--replicas=1` no Deployment do operator e
documentar que HA requer leader election. Implementar Lease-based election como fase 2.

```
niphas-operator/src/main.rs  -- adicionar leader election
niphas-operator/Cargo.toml   -- verificar feature flags do kube-runtime
```

---

## Severidade: Alto

### P3. Campo revision sem validação

**Arquivo:** `niphas-core/src/crd.rs:32-33`

Doc diz "Must be a 6-40 char hex string" mas nenhuma validação existe.
Qualquer string flui direto para `format!("{}/{}", flake_ref, rev)` no subprocess Nix.

**Impacto:** Input malicioso pode manipular o flake ref resultante.

**Correção:**

Adicionar validação no `evaluate()` do eval service, antes de construir o pinned_ref:

```rust
fn validate_revision(rev: &str) -> Result<(), NiphasError> {
    if rev.len() < 6 || rev.len() > 40 {
        return Err(NiphasError::InvalidInput("revision must be 6-40 chars".into()));
    }
    if !rev.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(NiphasError::InvalidInput("revision must be hex".into()));
    }
    Ok(())
}
```

```
niphas-core/src/eval.rs          -- adicionar validate_revision()
niphas-eval/src/evaluator.rs:42  -- chamar antes de usar
```

---

### P4. Sem limite de concorrência no eval subprocess

**Arquivo:** `niphas-eval/src/main.rs`

Cada request POST /evaluate spawna um subprocess `nix eval` sem nenhum limite.
Sob carga, pode fork-bomb o node.

**Impacto:** DoS no node do eval service.

**Correção:**

Adicionar `tokio::sync::Semaphore` ao `Evaluator` com permits configuráveis:

```rust
pub struct Evaluator {
    config: NiphasConfig,
    cache_client: CacheClient,
    warm: AtomicBool,
    eval_semaphore: Semaphore,  // novo
}

// Em evaluate():
let _permit = self.eval_semaphore.acquire().await
    .map_err(|_| NiphasError::NixEval("eval service shutting down".into()))?;
```

Adicionar campo `max_concurrent_evals` ao `NiphasConfig` (default: 4).

```
niphas-core/src/config.rs       -- adicionar max_concurrent_evals
niphas-eval/src/evaluator.rs    -- adicionar Semaphore
```

---

### P5. NAR inteiro em memória (3x)

**Arquivos:** `niphas-core/src/nix/nar.rs:28`, `niphas-csi/src/cache.rs:178-200`

Fluxo atual: bytes comprimidos (mem) -> descomprimidos (mem) -> árvore parsed com
todos os conteúdos (mem) -> escrito em disco. Para pacotes grandes (gcc ~500MB),
consome ~1.5GB de RAM.

**Impacto:** OOM kill em nodes com memória limitada. DoS via pacote grande.

**Correção (fase 1 -- limitar):**

Adicionar limite global ao `decompress_nar`: rejeitar NARs descomprimidos > 2GB.
Isso é simples e cobre o caso de DoS.

**Correção (fase 2 -- streaming):**

Redesenhar `NarReader` para streaming: em vez de retornar `NarNode` com `Vec<u8>`,
usar um visitor/callback pattern que escreve diretamente no disco:

```rust
pub trait NarVisitor {
    fn regular_file(&mut self, path: &Path, executable: bool, contents: &mut dyn AsyncRead) -> Result<()>;
    fn symlink(&mut self, path: &Path, target: &str) -> Result<()>;
    fn directory(&mut self, path: &Path) -> Result<()>;
}
```

Isso elimina a árvore em memória completamente. É um refactor grande que muda a API
do `NarReader`. Fazer depois do MVP.

```
niphas-csi/src/cache.rs         -- fase 1: limite de 2GB no decompress
niphas-core/src/nix/nar.rs      -- fase 2: NarVisitor streaming
```

---

### P6. Ready flag antes do controller iniciar

**Arquivo:** `niphas-operator/src/main.rs:58`

```rust
ready.store(true, Ordering::Relaxed);  // linha 58
Controller::new(workloads, ...)         // linha 60
```

O readiness probe retorna 200 antes do controller sequer começar a assistir.
Em rolling updates, requests podem ser roteados para uma instância que ainda
não está processando.

**Impacto:** Window de indisponibilidade durante rolling updates.

**Correção:**

Mover o `ready.store(true)` para dentro do primeiro `for_each` callback,
ou melhor, usar o `Controller::graceful_shutdown_on` e setar ready após
o primeiro reconcile:

```rust
// Setar ready quando o controller começar a receber eventos
let ready_flag = ready.clone();
Controller::new(workloads, Default::default())
    .owns(deployments, Default::default())
    .owns(pods, Default::default())
    .run(reconciler::reconcile, reconciler::error_policy, ctx)
    .for_each(|res| {
        ready_flag.store(true, Ordering::Relaxed);
        async move { /* ... */ }
    })
    .await;
```

```
niphas-operator/src/main.rs:58  -- mover ready flag para dentro do for_each
```

---

## Severidade: Médio

### P7. Falhas silenciosas em resource building

**Arquivo:** `niphas-operator/src/resources.rs` (10 ocorrências)

`serde_json::to_value(...).unwrap_or_default()` insere `null` silenciosamente se
a serialização falhar. Pode criar Deployments quebrados sem nenhum log.

**Correção:**

Mudar a signature dos builders para `Result<Value, OperatorError>` e propagar o erro:

```rust
pub(crate) fn build_deployment(...) -> Result<serde_json::Value, OperatorError> {
    // ...
    if let Some(ref env) = workload.spec.env {
        container_obj.insert("env".into(), serde_json::to_value(env)?);
    }
}
```

Isso é seguro porque `serde_json::to_value` em tipos K8s que já implementam
Serialize nunca deveria falhar, mas se falhar, é melhor saber.

```
niphas-operator/src/resources.rs  -- builders retornam Result
niphas-operator/src/reconciler.rs -- propagar ? no call site
```

---

### P8. Glob matching com complexidade exponencial

**Arquivo:** `niphas-eval/src/allowlist.rs:49`

A recursão do `matches_glob` para `*` pode ter comportamento exponencial com
patterns adversariais como `*a*a*a*a*b` contra `aaaaaaa...`.

**Impacto:** Potencial ReDoS no path crítico de segurança (allowlist).

**Correção:**

Substituir pelo crate `glob-match` (zero-alloc, O(n) worst-case) ou implementar
matching iterativo com stack explícito:

```rust
// Cargo.toml
glob-match = "0.2"

// allowlist.rs
fn matches_glob(pattern: &str, value: &str) -> bool {
    glob_match::glob_match(pattern, value)
}
```

```
niphas-eval/Cargo.toml      -- adicionar glob-match
niphas-eval/src/allowlist.rs -- substituir matches_glob
```

---

### P9. Lock map cresce sem limite

**Arquivo:** `niphas-csi/src/cache.rs:27-28`

`locks: HashMap<String, Arc<Mutex<()>>>` nunca é limpo. Em nodes de longa duração,
acumula uma entrada por store path já baixado.

**Correção:**

Limpar o lock do mapa após o download completar:

```rust
// Após fetch_and_extract:
{
    let mut locks = self.locks.lock().await;
    locks.remove(basename);
}
```

Cada entrada é ~100 bytes (String + Arc + Mutex), então mesmo 100k paths
são apenas ~10MB. Baixa prioridade, mas bom higiene.

```
niphas-csi/src/cache.rs:95  -- remover lock após uso
```

---

### P10. Imagem runner hardcoded com :latest

**Arquivo:** `niphas-operator/src/resources.rs:14`

```rust
const RUNNER_IMAGE: &str = "ghcr.io/fullzer4/niphas-runner:latest";
```

`:latest` em produção é anti-pattern. Impede rollbacks, cache inconsistente,
e comportamento não-reproduzível.

**Correção:**

Adicionar `runner_image` ao `NiphasConfig` com default versionado:

```rust
// config.rs
#[serde(default = "default_runner_image")]
pub runner_image: String,

fn default_runner_image() -> String {
    format!("ghcr.io/fullzer4/niphas-runner:v{}", env!("CARGO_PKG_VERSION"))
}
```

Permitir override por workload no CRD:

```rust
// crd.rs
#[serde(default, skip_serializing_if = "Option::is_none")]
pub runner_image: Option<String>,
```

```
niphas-core/src/config.rs                -- adicionar runner_image
niphas-core/src/crd.rs                   -- adicionar runner_image override
niphas-operator/src/resources.rs:14      -- usar config.runner_image
```

---

### P11. ownerReference com UID vazio

**Arquivo:** `niphas-operator/src/resources.rs:447`

```rust
"uid": workload.metadata.uid.as_deref().unwrap_or("")
```

UID vazio quebra garbage collection do K8s silenciosamente. Se o workload
não tiver UID, os child resources nunca serão limpos.

**Correção:**

Retornar erro se UID for None em vez de usar string vazia:

```rust
fn owner_reference(workload: &NiphasWorkload) -> Result<serde_json::Value, OperatorError> {
    let uid = workload.metadata.uid.as_deref()
        .ok_or_else(|| OperatorError::Internal("workload missing UID".into()))?;
    Ok(json!({
        "uid": uid,
        // ...
    }))
}
```

```
niphas-operator/src/resources.rs:442-451  -- retornar Result
```

---

## Severidade: Baixo

### P12. Dependências workspace não usadas

**Arquivo:** `Cargo.toml:40-80`

Declaradas mas não usadas por nenhum crate ativo:
`libp2p`, `rkyv`, `smallvec`, `compact_str`, `bumpalo`, `memmap2`,
`lockfree-object-pool`, `garde`, `serde_yaml`.

**Correção:** Remover do workspace `Cargo.toml`. Re-adicionar quando forem necessárias.

---

### P13. niphas-mesh é stub vazio

**Arquivo:** `niphas-mesh/src/main.rs`

Crate inteiro é `fn main() { info!("starting"); }`. Puxa deps desnecessariamente.

**Correção:** Remover do workspace até ter implementação real, ou manter apenas
com `niphas-core` e `tokio` como deps mínimas (já foi feito parcialmente).

---

### P14. AppError sem Display/Error trait

**Arquivo:** `niphas-eval/src/error.rs:5-13`

`AppError` implementa `IntoResponse` mas não `Display` nem `Error`.
Não pode ser usado com `?` fora de handlers Axum.

**Correção:** Adicionar `#[derive(Debug, thiserror::Error)]` com `#[error("...")]` em cada variante.

---

### P15. humantime_serde hand-rolled

**Arquivo:** `niphas-core/src/config.rs:288-333`

Módulo `humantime_serde` reimplementa o que o crate `humantime-serde` já faz,
e suporta menos formatos ("300s", "5m", "1h" mas não "5m30s", "2h 30m").

**Correção:** Substituir pelo crate `humantime-serde = "1"`.

---

### P16. Sem integration tests

Nenhum crate tem diretório `tests/`. O fluxo operator -> eval -> CSI nunca é
testado end-to-end.

**Correção:** Implementar conforme o plano de testes Tier 2 existente (kind cluster + nix binary).

---

## Ordem de implementação sugerida

```
Fase 1 (segurança):
  P1  Validação de flake_ref/attribute
  P3  Validação de revision
  P4  Semaphore no eval
  P8  glob-match crate

Fase 2 (correções operacionais):
  P6  Ready flag
  P11 UID vazio
  P7  Builders retornam Result
  P10 Runner image configurável
  P12 Limpar deps não usadas

Fase 3 (robustez):
  P2  Leader election
  P5  Limite de NAR size (fase 1)
  P9  Cleanup do lock map
  P14 AppError com thiserror
  P15 humantime-serde crate

Fase 4 (futuro):
  P5  NAR streaming (fase 2)
  P16 Integration tests
  P13 Decidir sobre niphas-mesh
```
