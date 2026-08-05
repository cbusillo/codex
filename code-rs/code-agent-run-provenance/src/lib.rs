use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::Signature;
use ed25519_dalek::Signer as _;
use ed25519_dalek::SigningKey;
use ed25519_dalek::Verifier as _;
use ed25519_dalek::VerifyingKey;
use ed25519_dalek::pkcs8::DecodePrivateKey;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use fslock::LockFile;
use rand::TryRngCore;
use rand::rngs::OsRng;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest as _;
use sha2::Sha256;
use thiserror::Error;
use url::Url;

pub const CHALLENGE_VERSION: &str = "every-code.agent-run-challenge.v1";
pub const CLAIMS_VERSION: &str = "every-code.agent-run-provenance.v1";
pub const TRANSCRIPT_FORMAT: &str = "every-code.transcript-bytes.v1";
pub const MAX_VERIFICATION_LEEWAY_SECONDS: u64 = 300;

#[derive(Debug, Error)]
pub enum ProvenanceError {
    #[error("invalid {0}")]
    InvalidField(&'static str),
    #[error("challenge is not active")]
    ChallengeNotActive,
    #[error("challenge constraint does not match runtime-observed {0}")]
    ChallengeMismatch(&'static str),
    #[error("invalid compact JWS")]
    InvalidCompactJws,
    #[error("unsupported compact JWS header")]
    UnsupportedJwsHeader,
    #[error("JWS key id is not trusted")]
    UntrustedKey,
    #[error("JWS signature verification failed")]
    InvalidSignature,
    #[error("token issuer does not match")]
    IssuerMismatch,
    #[error("token audience does not match")]
    AudienceMismatch,
    #[error("token is not yet valid")]
    TokenNotYetValid,
    #[error("token has expired")]
    TokenExpired,
    #[error("token was issued in the future")]
    TokenIssuedInFuture,
    #[error("token nonce does not match the challenge")]
    NonceMismatch,
    #[error("token challenge digest does not match")]
    ChallengeDigestMismatch,
    #[error("token model family does not match the runtime model slug")]
    ModelFamilyMismatch,
    #[error("issuance replay rejected")]
    Replay,
    #[error("run role conflict rejected")]
    RoleConflict,
    #[error("invalid issuance ledger")]
    InvalidLedger,
    #[error("JSON processing failed")]
    Json(#[from] serde_json::Error),
    #[error("I/O operation failed")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ProvenanceError>;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunRole {
    Implementer,
    Reviewer,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChallengeTarget {
    pub repository: String,
    pub pull_request: Option<u64>,
    pub head_sha: String,
    pub tree_sha: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// A verifier-issued constraint set.
///
/// The challenge digest binds these values into the provenance token, but the
/// challenge is not self-authenticating. Issuers must receive it over an
/// authenticated channel, and verifiers must compare against the original
/// challenge from their own trusted storage.
pub struct AgentRunChallenge {
    pub version: String,
    pub challenge_id: String,
    pub audience: String,
    pub nonce: String,
    pub issued_at: u64,
    pub not_before: u64,
    pub expires_at: u64,
    pub role: RunRole,
    pub run_id: String,
    pub target: ChallengeTarget,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunIdentity {
    pub run_id: String,
    pub session_id: String,
    pub root_thread_id: String,
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeModelIdentity {
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelClaims {
    pub provider: String,
    pub model: String,
    pub family: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskIdentity {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    pub runtime_id: String,
    pub harness: String,
    pub version: String,
    pub operating_system: String,
    pub architecture: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TranscriptEvidence {
    pub format: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentRunClaims {
    pub version: String,
    pub iss: String,
    pub aud: String,
    pub kid: String,
    pub jti: String,
    pub iat: u64,
    pub nbf: u64,
    pub exp: u64,
    pub nonce: String,
    pub challenge_sha256: String,
    pub role: RunRole,
    pub target: ChallengeTarget,
    pub run: RunIdentity,
    pub model: ModelClaims,
    pub task: TaskIdentity,
    pub transcript: TranscriptEvidence,
    pub runtime: RuntimeIdentity,
}

#[derive(Clone)]
pub struct ObservedRepository {
    remote_url: String,
    pub pull_request: Option<u64>,
    pub head_sha: String,
    pub tree_sha: String,
}

impl ObservedRepository {
    pub fn new(
        remote_url: impl Into<String>,
        pull_request: Option<u64>,
        head_sha: impl Into<String>,
        tree_sha: impl Into<String>,
    ) -> Self {
        Self {
            remote_url: remote_url.into(),
            pull_request,
            head_sha: head_sha.into(),
            tree_sha: tree_sha.into(),
        }
    }

    fn normalized_target(&self) -> Result<ChallengeTarget> {
        Ok(ChallengeTarget {
            repository: normalize_remote_url(&self.remote_url)?,
            pull_request: self.pull_request,
            head_sha: self.head_sha.clone(),
            tree_sha: self.tree_sha.clone(),
        })
    }
}

impl fmt::Debug for ObservedRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObservedRepository")
            .field("remote_url", &"<redacted>")
            .field("pull_request", &self.pull_request)
            .field("head_sha", &self.head_sha)
            .field("tree_sha", &self.tree_sha)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct ObservedAgentRun {
    pub role: RunRole,
    pub repository: ObservedRepository,
    pub run: RunIdentity,
    pub model: RuntimeModelIdentity,
    pub task: TaskIdentity,
    pub transcript: TranscriptEvidence,
    pub runtime: RuntimeIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueParameters {
    pub issuer: String,
    pub jti: String,
    pub issued_at: u64,
}

impl IssueParameters {
    pub fn generate(issuer: impl Into<String>, issued_at: u64) -> Result<Self> {
        let mut bytes = [0u8; 16];
        OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|_| ProvenanceError::InvalidField("jti entropy"))?;
        Ok(Self {
            issuer: issuer.into(),
            jti: URL_SAFE_NO_PAD.encode(bytes),
            issued_at,
        })
    }
}

pub struct TranscriptHasher {
    hasher: Sha256,
    bytes: u64,
}

impl TranscriptHasher {
    pub fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(TRANSCRIPT_FORMAT.as_bytes());
        hasher.update([0]);
        Self {
            hasher,
            bytes: 0,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
    }

    pub fn finalize(self) -> TranscriptEvidence {
        TranscriptEvidence {
            format: TRANSCRIPT_FORMAT.to_string(),
            sha256: hex_lower(&self.hasher.finalize()),
            bytes: self.bytes,
        }
    }
}

impl Default for TranscriptHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Write for TranscriptHasher {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct ProvenanceSigningKey {
    kid: String,
    signing_key: SigningKey,
}

impl fmt::Debug for ProvenanceSigningKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProvenanceSigningKey")
            .field("kid", &self.kid)
            .field("signing_key", &"<redacted>")
            .finish()
    }
}

impl ProvenanceSigningKey {
    pub fn generate(kid: impl Into<String>) -> Result<Self> {
        let mut seed = [0u8; 32];
        OsRng
            .try_fill_bytes(&mut seed)
            .map_err(|_| ProvenanceError::InvalidField("signing key entropy"))?;
        Self::from_seed(kid, seed)
    }

    pub fn from_seed(kid: impl Into<String>, seed: [u8; 32]) -> Result<Self> {
        let kid = kid.into();
        require_nonempty(&kid, "key id")?;
        Ok(Self {
            kid,
            signing_key: SigningKey::from_bytes(&seed),
        })
    }

    pub fn from_pkcs8_der(kid: impl Into<String>, bytes: &[u8]) -> Result<Self> {
        let kid = kid.into();
        require_nonempty(&kid, "key id")?;
        let signing_key = SigningKey::from_pkcs8_der(bytes)
            .map_err(|_| ProvenanceError::InvalidField("PKCS#8 signing key"))?;
        Ok(Self { kid, signing_key })
    }

    pub fn from_pkcs8_base64(kid: impl Into<String>, encoded: &str) -> Result<Self> {
        let bytes = BASE64_STANDARD
            .decode(encoded)
            .map_err(|_| ProvenanceError::InvalidField("base64 PKCS#8 signing key"))?;
        Self::from_pkcs8_der(kid, &bytes)
    }

    pub fn to_pkcs8_base64(&self) -> Result<String> {
        let document = self
            .signing_key
            .to_pkcs8_der()
            .map_err(|_| ProvenanceError::InvalidField("PKCS#8 signing key"))?;
        Ok(BASE64_STANDARD.encode(document.as_bytes()))
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    pub fn public_jwk(&self) -> Ed25519Jwk {
        Ed25519Jwk {
            kty: "OKP".to_string(),
            crv: "Ed25519".to_string(),
            key_use: Some("sig".to_string()),
            alg: Some("EdDSA".to_string()),
            key_ops: None,
            kid: self.kid.clone(),
            x: URL_SAFE_NO_PAD.encode(self.signing_key.verifying_key().as_bytes()),
        }
    }

    pub fn jwks(&self) -> Ed25519Jwks {
        Ed25519Jwks {
            keys: vec![self.public_jwk()],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Ed25519Jwks {
    pub keys: Vec<Ed25519Jwk>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Ed25519Jwk {
    pub kty: String,
    pub crv: String,
    #[serde(default, rename = "use", skip_serializing_if = "Option::is_none")]
    pub key_use: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_ops: Option<Vec<String>>,
    pub kid: String,
    pub x: String,
}

#[derive(Clone, Debug)]
pub struct VerificationContext<'a> {
    pub issuer: &'a str,
    pub audience: &'a str,
    pub challenge: &'a AgentRunChallenge,
    pub now: u64,
    pub leeway_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct JwsHeader {
    alg: String,
    typ: String,
    kid: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LedgerEntry {
    jti: String,
    run_id: String,
    role: RunRole,
    issued_at: u64,
    token_sha256: String,
}

pub struct IssuanceLedger {
    path: PathBuf,
    lock_path: PathBuf,
    entries: Vec<LedgerEntry>,
    jtis: BTreeSet<String>,
    runs: BTreeMap<String, RunRole>,
}

impl IssuanceLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        OpenOptions::new().create(true).append(true).open(&path)?;
        let lock_path = path.with_extension("lock");
        let mut lock = LockFile::open(&lock_path)?;
        lock.lock()?;
        let mut ledger = Self {
            path,
            lock_path,
            entries: Vec::new(),
            jtis: BTreeSet::new(),
            runs: BTreeMap::new(),
        };
        ledger.reload()?;
        Ok(ledger)
    }

    pub fn entries_len(&self) -> usize {
        self.entries.len()
    }

    fn load_entry(&mut self, entry: LedgerEntry) -> Result<()> {
        if !self.jtis.insert(entry.jti.clone()) || self.runs.contains_key(&entry.run_id) {
            return Err(ProvenanceError::InvalidLedger);
        }
        self.runs.insert(entry.run_id.clone(), entry.role);
        self.entries.push(entry);
        Ok(())
    }

    fn record(&mut self, claims: &AgentRunClaims, token: &str) -> Result<()> {
        let mut lock = LockFile::open(&self.lock_path)?;
        lock.lock()?;
        self.reload()?;
        if self.jtis.contains(&claims.jti) {
            return Err(ProvenanceError::Replay);
        }
        if let Some(role) = self.runs.get(&claims.run.run_id) {
            return if *role == claims.role {
                Err(ProvenanceError::Replay)
            } else {
                Err(ProvenanceError::RoleConflict)
            };
        }

        let entry = LedgerEntry {
            jti: claims.jti.clone(),
            run_id: claims.run.run_id.clone(),
            role: claims.role,
            issued_at: claims.iat,
            token_sha256: sha256_hex(token.as_bytes()),
        };
        let mut line = canonical_json_bytes(&entry)?;
        line.push(b'\n');
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        file.write_all(&line)?;
        file.sync_data()?;
        self.jtis.insert(entry.jti.clone());
        self.runs.insert(entry.run_id.clone(), entry.role);
        self.entries.push(entry);
        Ok(())
    }

    fn reload(&mut self) -> Result<()> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;
        if !contents.is_empty() && !contents.ends_with(b"\n") {
            let complete_len = contents
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |position| position + 1);
            let final_fragment = &contents[complete_len..];
            if serde_json::from_slice::<LedgerEntry>(final_fragment).is_ok() {
                file.write_all(b"\n")?;
                contents.push(b'\n');
            } else {
                file.set_len(complete_len as u64)?;
                contents.truncate(complete_len);
            }
            file.sync_data()?;
        }
        self.entries.clear();
        self.jtis.clear();
        self.runs.clear();
        for line in contents.split(|byte| *byte == b'\n') {
            if line.iter().all(|byte| byte.is_ascii_whitespace()) {
                continue;
            }
            let entry: LedgerEntry =
                serde_json::from_slice(line).map_err(|_| ProvenanceError::InvalidLedger)?;
            self.load_entry(entry)?;
        }
        Ok(())
    }
}

pub fn issue_agent_run_provenance(
    signing_key: &ProvenanceSigningKey,
    challenge: &AgentRunChallenge,
    observed: &ObservedAgentRun,
    parameters: &IssueParameters,
    ledger: &mut IssuanceLedger,
) -> Result<(AgentRunClaims, String)> {
    let claims = build_claims(signing_key, challenge, observed, parameters)?;
    let token = sign_claims(signing_key, &claims)?;
    ledger.record(&claims, &token)?;
    Ok((claims, token))
}

pub fn build_claims(
    signing_key: &ProvenanceSigningKey,
    challenge: &AgentRunChallenge,
    observed: &ObservedAgentRun,
    parameters: &IssueParameters,
) -> Result<AgentRunClaims> {
    validate_challenge(challenge, parameters.issued_at, 0)?;
    require_nonempty(&parameters.issuer, "issuer")?;
    require_nonempty(&parameters.jti, "jti")?;
    validate_observed(observed)?;

    let target = observed.repository.normalized_target()?;
    if observed.role != challenge.role {
        return Err(ProvenanceError::ChallengeMismatch("role"));
    }
    if observed.run.run_id != challenge.run_id {
        return Err(ProvenanceError::ChallengeMismatch("run id"));
    }
    if target != challenge.target {
        return Err(ProvenanceError::ChallengeMismatch("repository target"));
    }

    Ok(AgentRunClaims {
        version: CLAIMS_VERSION.to_string(),
        iss: parameters.issuer.clone(),
        aud: challenge.audience.clone(),
        kid: signing_key.kid().to_string(),
        jti: parameters.jti.clone(),
        iat: parameters.issued_at,
        nbf: challenge.not_before,
        exp: challenge.expires_at,
        nonce: challenge.nonce.clone(),
        challenge_sha256: challenge_digest(challenge)?,
        role: observed.role,
        target,
        run: observed.run.clone(),
        model: ModelClaims {
            provider: observed.model.provider.clone(),
            model: observed.model.model.clone(),
            family: derive_model_family(&observed.model.model)?,
        },
        task: observed.task.clone(),
        transcript: observed.transcript.clone(),
        runtime: observed.runtime.clone(),
    })
}

pub fn verify_agent_run_provenance(
    token: &str,
    jwks: &Ed25519Jwks,
    context: &VerificationContext<'_>,
) -> Result<AgentRunClaims> {
    let (header_segment, payload_segment, signature_segment) = split_compact_jws(token)?;
    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_segment)
        .map_err(|_| ProvenanceError::InvalidCompactJws)?;
    let header: JwsHeader = serde_json::from_slice(&header_bytes)
        .map_err(|_| ProvenanceError::InvalidCompactJws)?;
    if header.alg != "EdDSA" || header.typ != "JWT" || header.kid.is_empty() {
        return Err(ProvenanceError::UnsupportedJwsHeader);
    }

    let matching_keys = jwks
        .keys
        .iter()
        .filter(|candidate| candidate.kid == header.kid)
        .collect::<Vec<_>>();
    let [jwk] = matching_keys.as_slice() else {
        return Err(ProvenanceError::UntrustedKey);
    };
    let verifying_key = verifying_key_from_jwk(jwk)?;
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature_segment)
        .map_err(|_| ProvenanceError::InvalidCompactJws)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| ProvenanceError::InvalidCompactJws)?;
    verifying_key
        .verify(
            format!("{header_segment}.{payload_segment}").as_bytes(),
            &signature,
        )
        .map_err(|_| ProvenanceError::InvalidSignature)?;

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_segment)
        .map_err(|_| ProvenanceError::InvalidCompactJws)?;
    let claims: AgentRunClaims = serde_json::from_slice(&payload_bytes)
        .map_err(|_| ProvenanceError::InvalidCompactJws)?;
    validate_verified_claims(&claims, &header, context)?;
    Ok(claims)
}

pub fn challenge_digest(challenge: &AgentRunChallenge) -> Result<String> {
    Ok(sha256_hex(&canonical_json_bytes(challenge)?))
}

pub fn normalize_remote_url(remote_url: &str) -> Result<String> {
    let remote_url = remote_url.trim();
    if remote_url.is_empty() {
        return Err(ProvenanceError::InvalidField("repository remote"));
    }

    if let Ok(url) = Url::parse(remote_url) {
        if !matches!(url.scheme(), "http" | "https" | "ssh" | "git") {
            return Err(ProvenanceError::InvalidField("repository remote"));
        }
        let host = url
            .host_str()
            .ok_or(ProvenanceError::InvalidField("repository remote"))?
            .to_ascii_lowercase();
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        let port = url.port().filter(|port| {
            Some(*port)
                != match url.scheme() {
                    "http" => Some(80),
                    "https" => Some(443),
                    "ssh" => Some(22),
                    "git" => Some(9418),
                    _ => None,
                }
        });
        let authority = match port {
            Some(port) => format!("{host}:{port}"),
            None => host,
        };
        return normalized_repository(authority, url.path());
    }

    let without_user = remote_url
        .rsplit_once('@')
        .map_or(remote_url, |(_, remainder)| remainder);
    let (host, path) = without_user
        .split_once(':')
        .ok_or(ProvenanceError::InvalidField("repository remote"))?;
    if host.is_empty() || host.contains('/') || path.contains('\\') {
        return Err(ProvenanceError::InvalidField("repository remote"));
    }
    normalized_repository(host.to_ascii_lowercase(), path)
}

pub fn derive_model_family(runtime_model_slug: &str) -> Result<String> {
    let normalized = runtime_model_slug.trim().to_ascii_lowercase();
    require_nonempty(&normalized, "runtime model slug")?;
    if runtime_model_slug.trim() != runtime_model_slug
        || normalized.chars().any(char::is_whitespace)
        || normalized.chars().any(char::is_control)
    {
        return Err(ProvenanceError::InvalidField("runtime model slug"));
    }
    let slug = normalized.rsplit('/').next().unwrap_or(&normalized);
    let slug = slug.strip_prefix("code-").unwrap_or(slug);
    require_nonempty(slug, "runtime model slug")?;
    let parts = slug.split('-').collect::<Vec<_>>();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(ProvenanceError::InvalidField("runtime model slug"));
    }

    if parts.first() == Some(&"gpt") && parts.len() >= 2 {
        return Ok(format!("gpt-{}", parts[1]));
    }
    if parts
        .first()
        .is_some_and(|part| {
            part.len() > 1
                && part.starts_with('o')
                && part[1..].chars().all(|character| character.is_ascii_digit())
        })
    {
        return Ok(parts[0].to_string());
    }
    if parts.first() == Some(&"gemini") && parts.len() >= 2 {
        return Ok(format!("gemini-{}", parts[1]));
    }
    if parts.first() == Some(&"claude") && parts.len() >= 3 {
        return Ok(parts[..3].join("-"));
    }

    const VARIANT_SUFFIXES: &[&str] = &[
        "chat", "instruct", "latest", "mini", "nano", "preview", "pro",
    ];
    if parts.len() > 1 && VARIANT_SUFFIXES.contains(&parts[parts.len() - 1]) {
        return Ok(parts[..parts.len() - 1].join("-"));
    }
    Ok(slug.to_string())
}

pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value)?;
    let mut output = Vec::new();
    write_canonical_json(&value, &mut output)?;
    Ok(output)
}

fn validate_challenge(
    challenge: &AgentRunChallenge,
    now: u64,
    leeway_seconds: u64,
) -> Result<()> {
    validate_leeway(leeway_seconds)?;
    if challenge.version != CHALLENGE_VERSION {
        return Err(ProvenanceError::InvalidField("challenge version"));
    }
    require_nonempty(&challenge.challenge_id, "challenge id")?;
    require_nonempty(&challenge.audience, "audience")?;
    require_nonempty(&challenge.nonce, "nonce")?;
    require_nonempty(&challenge.run_id, "run id")?;
    validate_target(&challenge.target)?;
    if challenge.issued_at > challenge.not_before
        || challenge.not_before > challenge.expires_at
    {
        return Err(ProvenanceError::ChallengeNotActive);
    }
    if is_before_with_leeway(now, challenge.not_before, leeway_seconds)
        || is_expired_with_leeway(now, challenge.expires_at, leeway_seconds)
    {
        return Err(ProvenanceError::ChallengeNotActive);
    }
    Ok(())
}

fn validate_observed(observed: &ObservedAgentRun) -> Result<()> {
    validate_run_identity(&observed.run)?;
    require_nonempty(&observed.model.provider, "runtime model provider")?;
    require_nonempty(&observed.model.model, "runtime model slug")?;
    require_nonempty(&observed.task.kind, "task kind")?;
    require_nonempty(&observed.task.id, "task id")?;
    require_nonempty(&observed.runtime.runtime_id, "runtime id")?;
    require_nonempty(&observed.runtime.harness, "runtime harness")?;
    require_nonempty(&observed.runtime.version, "runtime version")?;
    require_nonempty(&observed.runtime.operating_system, "runtime operating system")?;
    require_nonempty(&observed.runtime.architecture, "runtime architecture")?;
    if observed.transcript.format != TRANSCRIPT_FORMAT {
        return Err(ProvenanceError::InvalidField("transcript format"));
    }
    validate_digest(&observed.transcript.sha256, "transcript digest")?;
    let target = observed.repository.normalized_target()?;
    validate_target(&target)
}

fn validate_verified_claims(
    claims: &AgentRunClaims,
    header: &JwsHeader,
    context: &VerificationContext<'_>,
) -> Result<()> {
    validate_leeway(context.leeway_seconds)?;
    validate_challenge(context.challenge, context.now, 0)?;
    if claims.version != CLAIMS_VERSION {
        return Err(ProvenanceError::InvalidField("claims version"));
    }
    if claims.kid != header.kid {
        return Err(ProvenanceError::UntrustedKey);
    }
    if claims.iss != context.issuer {
        return Err(ProvenanceError::IssuerMismatch);
    }
    if claims.aud != context.audience || claims.aud != context.challenge.audience {
        return Err(ProvenanceError::AudienceMismatch);
    }
    require_nonempty(&claims.jti, "jti")?;
    if is_before_with_leeway(context.now, claims.iat, context.leeway_seconds) {
        return Err(ProvenanceError::TokenIssuedInFuture);
    }
    if is_before_with_leeway(context.now, claims.nbf, context.leeway_seconds) {
        return Err(ProvenanceError::TokenNotYetValid);
    }
    if is_expired_with_leeway(context.now, claims.exp, context.leeway_seconds) {
        return Err(ProvenanceError::TokenExpired);
    }
    if claims.iat < context.challenge.issued_at
        || claims.iat < claims.nbf
        || claims.iat >= claims.exp
    {
        return Err(ProvenanceError::InvalidField("token issued-at time"));
    }
    if claims.nbf != context.challenge.not_before || claims.exp != context.challenge.expires_at {
        return Err(ProvenanceError::ChallengeMismatch("validity window"));
    }
    if claims.nonce != context.challenge.nonce {
        return Err(ProvenanceError::NonceMismatch);
    }
    if claims.challenge_sha256 != challenge_digest(context.challenge)? {
        return Err(ProvenanceError::ChallengeDigestMismatch);
    }
    if claims.role != context.challenge.role {
        return Err(ProvenanceError::ChallengeMismatch("role"));
    }
    if claims.run.run_id != context.challenge.run_id {
        return Err(ProvenanceError::ChallengeMismatch("run id"));
    }
    if claims.target != context.challenge.target {
        return Err(ProvenanceError::ChallengeMismatch("repository target"));
    }
    validate_target(&claims.target)?;
    validate_run_identity(&claims.run)?;
    require_nonempty(&claims.model.provider, "runtime model provider")?;
    if claims.model.family != derive_model_family(&claims.model.model)? {
        return Err(ProvenanceError::ModelFamilyMismatch);
    }
    require_nonempty(&claims.task.kind, "task kind")?;
    require_nonempty(&claims.task.id, "task id")?;
    if claims.transcript.format != TRANSCRIPT_FORMAT {
        return Err(ProvenanceError::InvalidField("transcript format"));
    }
    validate_digest(&claims.transcript.sha256, "transcript digest")?;
    require_nonempty(&claims.runtime.runtime_id, "runtime id")?;
    require_nonempty(&claims.runtime.harness, "runtime harness")?;
    require_nonempty(&claims.runtime.version, "runtime version")?;
    require_nonempty(&claims.runtime.operating_system, "runtime operating system")?;
    require_nonempty(&claims.runtime.architecture, "runtime architecture")?;
    Ok(())
}

fn validate_target(target: &ChallengeTarget) -> Result<()> {
    require_nonempty(&target.repository, "repository")?;
    let reparsed = normalize_remote_url(&format!("ssh://{}", target.repository))?;
    if reparsed != target.repository {
        return Err(ProvenanceError::InvalidField("normalized repository"));
    }
    if target.pull_request == Some(0) {
        return Err(ProvenanceError::InvalidField("pull request"));
    }
    validate_git_sha(&target.head_sha, "head SHA")?;
    validate_git_sha(&target.tree_sha, "tree SHA")
}

fn validate_run_identity(run: &RunIdentity) -> Result<()> {
    require_nonempty(&run.run_id, "run id")?;
    require_nonempty(&run.session_id, "session id")?;
    require_nonempty(&run.root_thread_id, "root thread id")?;
    require_nonempty(&run.thread_id, "thread id")?;
    require_nonempty(&run.turn_id, "turn id")
}

fn validate_git_sha(value: &str, field: &'static str) -> Result<()> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProvenanceError::InvalidField(field));
    }
    Ok(())
}

fn validate_digest(value: &str, field: &'static str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProvenanceError::InvalidField(field));
    }
    Ok(())
}

fn validate_leeway(leeway_seconds: u64) -> Result<()> {
    if leeway_seconds > MAX_VERIFICATION_LEEWAY_SECONDS {
        return Err(ProvenanceError::InvalidField("verification leeway"));
    }
    Ok(())
}

fn is_before_with_leeway(now: u64, timestamp: u64, leeway_seconds: u64) -> bool {
    timestamp > now && timestamp - now > leeway_seconds
}

fn is_expired_with_leeway(now: u64, expires_at: u64, leeway_seconds: u64) -> bool {
    now >= expires_at && now - expires_at >= leeway_seconds
}

fn require_nonempty(value: &str, field: &'static str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(ProvenanceError::InvalidField(field));
    }
    Ok(())
}

fn normalized_repository(host: String, path: &str) -> Result<String> {
    let mut path = path.trim_matches('/');
    if let Some(stripped) = path.strip_suffix(".git") {
        path = stripped;
    }
    if host.is_empty()
        || path.is_empty()
        || path.contains('@')
        || path.contains('?')
        || path.contains('#')
    {
        return Err(ProvenanceError::InvalidField("repository remote"));
    }
    Ok(format!("{host}/{path}"))
}

fn sign_claims(signing_key: &ProvenanceSigningKey, claims: &AgentRunClaims) -> Result<String> {
    let header = JwsHeader {
        alg: "EdDSA".to_string(),
        typ: "JWT".to_string(),
        kid: signing_key.kid.clone(),
    };
    let header = URL_SAFE_NO_PAD.encode(canonical_json_bytes(&header)?);
    let payload = URL_SAFE_NO_PAD.encode(canonical_json_bytes(claims)?);
    let signing_input = format!("{header}.{payload}");
    let signature = signing_key.signing_key.sign(signing_input.as_bytes());
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    ))
}

fn split_compact_jws(token: &str) -> Result<(&str, &str, &str)> {
    let mut parts = token.split('.');
    let segments = match (parts.next(), parts.next(), parts.next()) {
        (Some(header), Some(payload), Some(signature))
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty() =>
        {
            (header, payload, signature)
        }
        _ => return Err(ProvenanceError::InvalidCompactJws),
    };
    if parts.next().is_some() {
        return Err(ProvenanceError::InvalidCompactJws);
    }
    Ok(segments)
}

fn verifying_key_from_jwk(jwk: &Ed25519Jwk) -> Result<VerifyingKey> {
    if jwk.kty != "OKP"
        || jwk.crv != "Ed25519"
        || jwk
            .key_use
            .as_deref()
            .is_some_and(|key_use| key_use != "sig")
        || jwk.alg.as_deref().is_some_and(|alg| alg != "EdDSA")
        || jwk.key_ops.as_ref().is_some_and(|key_ops| {
            !key_ops.iter().any(|operation| operation == "verify")
        })
    {
        return Err(ProvenanceError::UntrustedKey);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(&jwk.x)
        .map_err(|_| ProvenanceError::UntrustedKey)?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| ProvenanceError::UntrustedKey)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| ProvenanceError::UntrustedKey)
}

fn write_canonical_json(value: &Value, output: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(true) => output.extend_from_slice(b"true"),
        Value::Bool(false) => output.extend_from_slice(b"false"),
        Value::Number(number) => output.extend_from_slice(number.to_string().as_bytes()),
        Value::String(string) => output.extend_from_slice(serde_json::to_string(string)?.as_bytes()),
        Value::Array(values) => {
            output.push(b'[');
            for (index, child) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_canonical_json(child, output)?;
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            for (index, (key, child)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                output.extend_from_slice(serde_json::to_string(key)?.as_bytes());
                output.push(b':');
                write_canonical_json(child, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;

    use pretty_assertions::assert_eq;
    use serde::Deserialize;
    use serde::Serialize;
    use tempfile::TempDir;

    use super::*;

    const FIXED_CLOCK: u64 = 1_785_931_200;
    const FIXED_ISSUER: &str = "https://code.every.dev/agent-run-provenance";
    const FIXED_KID: &str = "every-code-test-2026-08-05";

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    struct GoldenVerification {
        issuer: String,
        audience: String,
        now: u64,
        leeway_seconds: u64,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    struct GoldenNegativeCase {
        name: String,
        token: String,
        verify_at: u64,
        expected_error: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    struct GoldenVector {
        schema: String,
        fixed_seed_sha256: String,
        fixed_clock: u64,
        jwks: Ed25519Jwks,
        challenge: AgentRunChallenge,
        canonical_challenge_json: String,
        claims: AgentRunClaims,
        canonical_claims_json: String,
        token: String,
        verification: GoldenVerification,
        negative_cases: Vec<GoldenNegativeCase>,
    }

    #[test]
    fn signs_and_verifies_runtime_observed_claims() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let mut ledger = IssuanceLedger::open(temp_dir.path().join("ledger.jsonl"))?;
        let key = fixed_key()?;
        let challenge = fixed_challenge();
        let observed = fixed_observed();
        let parameters = fixed_parameters("jti-round-trip");

        let (claims, token) = issue_agent_run_provenance(
            &key,
            &challenge,
            &observed,
            &parameters,
            &mut ledger,
        )?;
        let verified = verify_agent_run_provenance(
            &token,
            &key.jwks(),
            &fixed_verification_context(&challenge, FIXED_CLOCK),
        )?;

        assert_eq!(verified, claims);
        assert_eq!(verified.model.model, "code-gpt-5.6-sol");
        assert_eq!(verified.model.family, "gpt-5.6");
        assert_eq!(verified.target.repository, "github.com/cbusillo/code");
        assert!(!token.contains("oauth2"));
        assert!(!token.contains("secret"));
        assert_eq!(ledger.entries_len(), 1);
        Ok(())
    }

    #[test]
    fn challenge_cannot_override_runtime_run_role_or_target() -> Result<()> {
        let key = fixed_key()?;
        let mut challenge = fixed_challenge();
        let observed = fixed_observed();
        let parameters = fixed_parameters("jti-constraints");

        challenge.run_id = "caller-selected-run".to_string();
        assert!(matches!(
            build_claims(&key, &challenge, &observed, &parameters),
            Err(ProvenanceError::ChallengeMismatch("run id"))
        ));

        challenge = fixed_challenge();
        challenge.role = RunRole::Reviewer;
        assert!(matches!(
            build_claims(&key, &challenge, &observed, &parameters),
            Err(ProvenanceError::ChallengeMismatch("role"))
        ));

        challenge = fixed_challenge();
        challenge.target.head_sha = "c".repeat(40);
        assert!(matches!(
            build_claims(&key, &challenge, &observed, &parameters),
            Err(ProvenanceError::ChallengeMismatch("repository target"))
        ));

        let mut challenge_json = serde_json::to_value(fixed_challenge())?;
        challenge_json["model"] = Value::String("caller-model".to_string());
        assert!(serde_json::from_value::<AgentRunChallenge>(challenge_json).is_err());
        Ok(())
    }

    #[test]
    fn transcript_digest_streams_without_disclosure() -> Result<()> {
        let mut streamed = TranscriptHasher::new();
        streamed.update(b"private ");
        streamed.write_all(b"transcript")?;

        let mut contiguous = TranscriptHasher::new();
        contiguous.update(b"private transcript");

        let evidence = streamed.finalize();
        assert_eq!(evidence, contiguous.finalize());
        assert_eq!(evidence.format, TRANSCRIPT_FORMAT);
        assert_eq!(evidence.bytes, 18);
        let serialized = serde_json::to_string(&evidence)?;
        assert!(!serialized.contains("private"));
        assert!(!serialized.contains("private transcript"));
        Ok(())
    }

    #[test]
    fn digest_validation_requires_canonical_lowercase_hex() {
        assert!(validate_digest(&"a".repeat(64), "digest").is_ok());
        assert!(matches!(
            validate_digest(&"A".repeat(64), "digest"),
            Err(ProvenanceError::InvalidField("digest"))
        ));
    }

    #[test]
    fn remote_normalization_removes_credentials_and_transport_details() -> Result<()> {
        assert_eq!(
            normalize_remote_url(
                "https://oauth2:secret@GitHub.com/cbusillo/code.git?token=hidden#fragment"
            )?,
            "github.com/cbusillo/code"
        );
        assert_eq!(
            normalize_remote_url("git@GitHub.com:cbusillo/code.git")?,
            "github.com/cbusillo/code"
        );
        assert_eq!(
            normalize_remote_url("ssh://git@github.com:2222/cbusillo/code.git")?,
            "github.com:2222/cbusillo/code"
        );
        assert_eq!(
            normalize_remote_url("ssh://git@github.com:22/cbusillo/code.git")?,
            "github.com/cbusillo/code"
        );
        assert_eq!(
            normalize_remote_url("https://github.com:443/cbusillo/code.git")?,
            "github.com/cbusillo/code"
        );

        let observed = ObservedRepository::new(
            "https://oauth2:secret@github.com/cbusillo/code.git",
            Some(442),
            "a".repeat(40),
            "b".repeat(40),
        );
        let debug = format!("{observed:?}");
        assert!(!debug.contains("oauth2"));
        assert!(!debug.contains("secret"));
        assert!(debug.contains("<redacted>"));
        Ok(())
    }

    #[test]
    fn model_family_is_derived_from_runtime_slug() -> Result<()> {
        assert_eq!(derive_model_family("code-gpt-5.6-sol")?, "gpt-5.6");
        assert_eq!(derive_model_family("openai/o3-mini")?, "o3");
        assert_eq!(
            derive_model_family("anthropic/claude-sonnet-4.6")?,
            "claude-sonnet-4.6"
        );
        assert_eq!(
            derive_model_family("google/gemini-3.1-pro")?,
            "gemini-3.1"
        );
        for invalid in ["openai/", "code-", "gpt--preview", " gpt-5.6", "gpt 5.6"] {
            assert!(derive_model_family(invalid).is_err(), "{invalid}");
        }
        Ok(())
    }

    #[test]
    fn signing_keys_round_trip_through_pkcs8_and_jwks() -> Result<()> {
        let key = fixed_key()?;
        let encoded = key.to_pkcs8_base64()?;
        let loaded = ProvenanceSigningKey::from_pkcs8_base64(FIXED_KID, &encoded)?;
        assert_eq!(loaded.public_jwk(), key.public_jwk());
        assert!(!format!("{loaded:?}").contains(&encoded));

        let generated = ProvenanceSigningKey::generate("generated-key")?;
        assert_eq!(generated.public_jwk().x.len(), 43);
        Ok(())
    }

    #[test]
    fn ledger_rejects_replay_and_role_conflict_after_reopen() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let ledger_path = temp_dir.path().join("ledger.jsonl");
        let key = fixed_key()?;
        let challenge = fixed_challenge();
        let observed = fixed_observed();
        let mut ledger = IssuanceLedger::open(&ledger_path)?;

        issue_agent_run_provenance(
            &key,
            &challenge,
            &observed,
            &fixed_parameters("jti-first"),
            &mut ledger,
        )?;
        assert!(matches!(
            issue_agent_run_provenance(
                &key,
                &challenge,
                &observed,
                &fixed_parameters("jti-second"),
                &mut ledger,
            ),
            Err(ProvenanceError::Replay)
        ));

        let mut second_run_challenge = challenge.clone();
        second_run_challenge.run_id = "run-443-fixed".to_string();
        let mut second_run_observed = observed.clone();
        second_run_observed.run.run_id = "run-443-fixed".to_string();
        assert!(matches!(
            issue_agent_run_provenance(
                &key,
                &second_run_challenge,
                &second_run_observed,
                &fixed_parameters("jti-first"),
                &mut ledger,
            ),
            Err(ProvenanceError::Replay)
        ));

        let mut reviewer_challenge = challenge.clone();
        reviewer_challenge.role = RunRole::Reviewer;
        let mut reviewer_observed = observed.clone();
        reviewer_observed.role = RunRole::Reviewer;
        assert!(matches!(
            issue_agent_run_provenance(
                &key,
                &reviewer_challenge,
                &reviewer_observed,
                &fixed_parameters("jti-reviewer"),
                &mut ledger,
            ),
            Err(ProvenanceError::RoleConflict)
        ));

        drop(ledger);
        let reopened = IssuanceLedger::open(&ledger_path)?;
        assert_eq!(reopened.entries_len(), 1);
        assert_eq!(fs::read_to_string(ledger_path)?.lines().count(), 1);
        Ok(())
    }

    #[test]
    fn ledger_discards_only_an_incomplete_final_write() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let ledger_path = temp_dir.path().join("ledger.jsonl");
        let key = fixed_key()?;
        let challenge = fixed_challenge();
        let observed = fixed_observed();
        let mut ledger = IssuanceLedger::open(&ledger_path)?;
        issue_agent_run_provenance(
            &key,
            &challenge,
            &observed,
            &fixed_parameters("jti-complete"),
            &mut ledger,
        )?;
        drop(ledger);

        let mut file = OpenOptions::new().append(true).open(&ledger_path)?;
        file.write_all(br#"{"jti":"partial""#)?;
        file.sync_data()?;
        drop(file);

        let recovered = IssuanceLedger::open(&ledger_path)?;
        assert_eq!(recovered.entries_len(), 1);
        assert!(fs::read(&ledger_path)?.ends_with(b"\n"));

        fs::write(&ledger_path, b"not-json\n")?;
        assert!(matches!(
            IssuanceLedger::open(&ledger_path),
            Err(ProvenanceError::InvalidLedger)
        ));
        Ok(())
    }

    #[test]
    fn ledger_preserves_a_complete_final_entry_without_newline() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let ledger_path = temp_dir.path().join("ledger.jsonl");
        let key = fixed_key()?;
        let challenge = fixed_challenge();
        let observed = fixed_observed();
        let mut ledger = IssuanceLedger::open(&ledger_path)?;
        issue_agent_run_provenance(
            &key,
            &challenge,
            &observed,
            &fixed_parameters("jti-no-newline"),
            &mut ledger,
        )?;
        drop(ledger);

        let mut contents = fs::read(&ledger_path)?;
        assert_eq!(contents.pop(), Some(b'\n'));
        assert!(serde_json::from_slice::<LedgerEntry>(&contents).is_ok());
        fs::write(&ledger_path, contents)?;

        let recovered = IssuanceLedger::open(&ledger_path)?;
        assert_eq!(recovered.entries_len(), 1);
        assert!(fs::read(&ledger_path)?.ends_with(b"\n"));
        Ok(())
    }

    #[test]
    fn ledger_serializes_concurrent_role_conflicts() -> Result<()> {
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::thread;

        let temp_dir = TempDir::new()?;
        let ledger_path = temp_dir.path().join("concurrent-ledger.jsonl");
        let barrier = Arc::new(Barrier::new(2));
        let spawn_issuer = |role: RunRole, jti: &'static str| {
            let barrier = Arc::clone(&barrier);
            let ledger_path = ledger_path.clone();
            thread::spawn(move || -> Result<(AgentRunClaims, String)> {
                let key = fixed_key()?;
                let mut challenge = fixed_challenge();
                challenge.role = role;
                let mut observed = fixed_observed();
                observed.role = role;
                let mut ledger = IssuanceLedger::open(ledger_path)?;
                barrier.wait();
                issue_agent_run_provenance(
                    &key,
                    &challenge,
                    &observed,
                    &fixed_parameters(jti),
                    &mut ledger,
                )
            })
        };

        let implementer = spawn_issuer(RunRole::Implementer, "jti-concurrent-implementer");
        let reviewer = spawn_issuer(RunRole::Reviewer, "jti-concurrent-reviewer");
        let implementer = implementer
            .join()
            .map_err(|_| ProvenanceError::InvalidField("implementer thread"))?;
        let reviewer = reviewer
            .join()
            .map_err(|_| ProvenanceError::InvalidField("reviewer thread"))?;

        assert_eq!(
            [implementer.is_ok(), reviewer.is_ok()]
                .into_iter()
                .filter(|accepted| *accepted)
                .count(),
            1
        );
        assert_eq!(
            [&implementer, &reviewer]
                .into_iter()
                .filter(|outcome| matches!(outcome, Err(ProvenanceError::RoleConflict)))
                .count(),
            1
        );
        assert_eq!(IssuanceLedger::open(&ledger_path)?.entries_len(), 1);
        Ok(())
    }

    #[test]
    fn verifier_applies_leeway_and_accepts_standard_jwk_extensions() -> Result<()> {
        let key = fixed_key()?;
        let challenge = fixed_challenge();
        let claims = build_claims(
            &key,
            &challenge,
            &fixed_observed(),
            &fixed_parameters("jti-leeway"),
        )?;
        let token = sign_claims(&key, &claims)?;
        let mut jwks_value = serde_json::to_value(key.jwks())?;
        let jwk = jwks_value["keys"][0]
            .as_object_mut()
            .ok_or(ProvenanceError::InvalidField("test JWK"))?;
        jwk.remove("use");
        jwk.remove("alg");
        jwk.insert("key_ops".to_string(), serde_json::json!(["verify"]));
        jwk.insert(
            "standard_extension".to_string(),
            Value::String("accepted".to_string()),
        );
        let jwks: Ed25519Jwks = serde_json::from_value(jwks_value)?;
        let context = VerificationContext {
            issuer: FIXED_ISSUER,
            audience: "launchplane",
            challenge: &challenge,
            now: FIXED_CLOCK - 30,
            leeway_seconds: 60,
        };

        assert_eq!(verify_agent_run_provenance(&token, &jwks, &context)?, claims);
        Ok(())
    }

    #[test]
    fn verifier_rejects_excessive_clock_leeway() -> Result<()> {
        let key = fixed_key()?;
        let challenge = fixed_challenge();
        let claims = build_claims(
            &key,
            &challenge,
            &fixed_observed(),
            &fixed_parameters("jti-excessive-leeway"),
        )?;
        let token = sign_claims(&key, &claims)?;
        let context = VerificationContext {
            issuer: FIXED_ISSUER,
            audience: "launchplane",
            challenge: &challenge,
            now: u64::MAX,
            leeway_seconds: u64::MAX,
        };

        assert!(matches!(
            verify_agent_run_provenance(&token, &key.jwks(), &context),
            Err(ProvenanceError::InvalidField("verification leeway"))
        ));
        Ok(())
    }

    #[test]
    fn verifier_rejects_all_golden_negative_cases() -> Result<()> {
        let vector = build_golden_vector()?;
        for negative in &vector.negative_cases {
            let context = fixed_verification_context(&vector.challenge, negative.verify_at);
            let error = verify_agent_run_provenance(&negative.token, &vector.jwks, &context)
                .err()
                .ok_or(ProvenanceError::InvalidField("negative vector acceptance"))?;
            assert_eq!(error_code(&error), negative.expected_error, "{}", negative.name);
        }
        Ok(())
    }

    #[test]
    fn golden_vector_matches_fixture() -> Result<()> {
        let vector = build_golden_vector()?;
        let generated = format!("{}\n", serde_json::to_string_pretty(&vector)?);
        if std::env::var_os("PRINT_AGENT_RUN_PROVENANCE_VECTOR").is_some() {
            println!("BEGIN_AGENT_RUN_PROVENANCE_VECTOR\n{generated}END_AGENT_RUN_PROVENANCE_VECTOR");
            return Ok(());
        }
        let fixture = include_str!("../tests/fixtures/agent_run_provenance_v1.json");
        assert_eq!(generated, fixture);
        Ok(())
    }

    fn build_golden_vector() -> Result<GoldenVector> {
        let key = fixed_key()?;
        let challenge = fixed_challenge();
        let claims = build_claims(
            &key,
            &challenge,
            &fixed_observed(),
            &fixed_parameters("jti-fixed-2026-08-05"),
        )?;
        let token = sign_claims(&key, &claims)?;
        let negative_cases = negative_cases(&key, &claims, &token)?;
        Ok(GoldenVector {
            schema: CLAIMS_VERSION.to_string(),
            fixed_seed_sha256: sha256_hex(&[0x42; 32]),
            fixed_clock: FIXED_CLOCK,
            jwks: key.jwks(),
            challenge: challenge.clone(),
            canonical_challenge_json: String::from_utf8(canonical_json_bytes(&challenge)?)
                .map_err(|_| ProvenanceError::InvalidField("canonical challenge UTF-8"))?,
            claims: claims.clone(),
            canonical_claims_json: String::from_utf8(canonical_json_bytes(&claims)?)
                .map_err(|_| ProvenanceError::InvalidField("canonical claims UTF-8"))?,
            token,
            verification: GoldenVerification {
                issuer: FIXED_ISSUER.to_string(),
                audience: challenge.audience.clone(),
                now: FIXED_CLOCK,
                leeway_seconds: 0,
            },
            negative_cases,
        })
    }

    fn negative_cases(
        key: &ProvenanceSigningKey,
        claims: &AgentRunClaims,
        token: &str,
    ) -> Result<Vec<GoldenNegativeCase>> {
        let mut cases = Vec::new();
        let mut add_signed =
            |name: &str, expected_error: &str, mutated: AgentRunClaims, verify_at: u64| {
                cases.push(GoldenNegativeCase {
                    name: name.to_string(),
                    token: sign_claims(key, &mutated)?,
                    verify_at,
                    expected_error: expected_error.to_string(),
                });
                Result::Ok(())
            };

        let mut mutated = claims.clone();
        mutated.iss = "https://invalid.example/issuer".to_string();
        add_signed("issuer_tampered", "issuer_mismatch", mutated, FIXED_CLOCK)?;

        let mut mutated = claims.clone();
        mutated.aud = "wrong-audience".to_string();
        add_signed("audience_tampered", "audience_mismatch", mutated, FIXED_CLOCK)?;

        let mut mutated = claims.clone();
        mutated.exp = FIXED_CLOCK;
        add_signed("expired", "token_expired", mutated, FIXED_CLOCK)?;

        let mut mutated = claims.clone();
        mutated.iat = FIXED_CLOCK + 1;
        add_signed(
            "issued_in_future",
            "token_issued_in_future",
            mutated,
            FIXED_CLOCK,
        )?;

        let mut mutated = claims.clone();
        mutated.nbf = FIXED_CLOCK + 1;
        add_signed(
            "not_yet_valid",
            "token_not_yet_valid",
            mutated,
            FIXED_CLOCK,
        )?;

        let mut mutated = claims.clone();
        mutated.iat = claims.nbf - 1;
        add_signed(
            "issued_before_not_before",
            "invalid_token_issued_at",
            mutated,
            FIXED_CLOCK,
        )?;

        let mut mutated = claims.clone();
        mutated.nonce = "wrong-nonce".to_string();
        add_signed("nonce_tampered", "nonce_mismatch", mutated, FIXED_CLOCK)?;

        let mut mutated = claims.clone();
        mutated.challenge_sha256 = "0".repeat(64);
        add_signed(
            "challenge_digest_tampered",
            "challenge_digest_mismatch",
            mutated,
            FIXED_CLOCK,
        )?;

        let mut mutated = claims.clone();
        mutated.target.repository = "github.com/other/repository".to_string();
        add_signed(
            "repository_tampered",
            "challenge_target_mismatch",
            mutated,
            FIXED_CLOCK,
        )?;

        let mut mutated = claims.clone();
        mutated.target.pull_request = Some(443);
        add_signed(
            "pull_request_tampered",
            "challenge_target_mismatch",
            mutated,
            FIXED_CLOCK,
        )?;

        let mut mutated = claims.clone();
        mutated.target.head_sha = "c".repeat(40);
        add_signed(
            "head_sha_tampered",
            "challenge_target_mismatch",
            mutated,
            FIXED_CLOCK,
        )?;

        let mut mutated = claims.clone();
        mutated.target.tree_sha = "d".repeat(40);
        add_signed(
            "tree_sha_tampered",
            "challenge_target_mismatch",
            mutated,
            FIXED_CLOCK,
        )?;

        let mut mutated = claims.clone();
        mutated.role = RunRole::Reviewer;
        add_signed(
            "role_tampered",
            "challenge_role_mismatch",
            mutated,
            FIXED_CLOCK,
        )?;

        let mut mutated = claims.clone();
        mutated.run.run_id = "run-caller-selected".to_string();
        add_signed(
            "run_identity_tampered",
            "challenge_run_mismatch",
            mutated,
            FIXED_CLOCK,
        )?;

        let mut mutated = claims.clone();
        mutated.model.family = "gpt-4".to_string();
        add_signed(
            "model_family_tampered",
            "model_family_mismatch",
            mutated,
            FIXED_CLOCK,
        )?;

        cases.push(GoldenNegativeCase {
            name: "challenge_expired".to_string(),
            token: token.to_string(),
            verify_at: claims.exp,
            expected_error: "challenge_not_active".to_string(),
        });

        let mut bad_signature = token.as_bytes().to_vec();
        if let Some(last) = bad_signature.last_mut() {
            *last = if *last == b'A' { b'B' } else { b'A' };
        }
        cases.push(GoldenNegativeCase {
            name: "signature_tampered".to_string(),
            token: String::from_utf8(bad_signature)
                .map_err(|_| ProvenanceError::InvalidCompactJws)?,
            verify_at: FIXED_CLOCK,
            expected_error: "invalid_signature".to_string(),
        });

        Ok(cases)
    }

    fn fixed_key() -> Result<ProvenanceSigningKey> {
        ProvenanceSigningKey::from_seed(FIXED_KID, [0x42; 32])
    }

    fn fixed_challenge() -> AgentRunChallenge {
        AgentRunChallenge {
            version: CHALLENGE_VERSION.to_string(),
            challenge_id: "challenge-442-fixed".to_string(),
            audience: "launchplane".to_string(),
            nonce: "bm9uY2UtNDQyLWZpeGVk".to_string(),
            issued_at: FIXED_CLOCK - 60,
            not_before: FIXED_CLOCK - 60,
            expires_at: FIXED_CLOCK + 600,
            role: RunRole::Implementer,
            run_id: "run-442-fixed".to_string(),
            target: ChallengeTarget {
                repository: "github.com/cbusillo/code".to_string(),
                pull_request: Some(442),
                head_sha: "a".repeat(40),
                tree_sha: "b".repeat(40),
            },
        }
    }

    fn fixed_observed() -> ObservedAgentRun {
        let mut transcript = TranscriptHasher::new();
        transcript.update(b"private transcript bytes are never disclosed");
        ObservedAgentRun {
            role: RunRole::Implementer,
            repository: ObservedRepository::new(
                "https://oauth2:secret@github.com/cbusillo/code.git",
                Some(442),
                "a".repeat(40),
                "b".repeat(40),
            ),
            run: RunIdentity {
                run_id: "run-442-fixed".to_string(),
                session_id: "session-442-fixed".to_string(),
                root_thread_id: "root-thread-442-fixed".to_string(),
                thread_id: "thread-442-fixed".to_string(),
                turn_id: "turn-7".to_string(),
            },
            model: RuntimeModelIdentity {
                provider: "openai".to_string(),
                model: "code-gpt-5.6-sol".to_string(),
            },
            task: TaskIdentity {
                kind: "github_issue".to_string(),
                id: "cbusillo/code#442".to_string(),
            },
            transcript: transcript.finalize(),
            runtime: RuntimeIdentity {
                runtime_id: "every-code-runtime-test".to_string(),
                harness: "every-code-cli".to_string(),
                version: "0.0.0-test".to_string(),
                operating_system: "macos".to_string(),
                architecture: "aarch64".to_string(),
            },
        }
    }

    fn fixed_parameters(jti: &str) -> IssueParameters {
        IssueParameters {
            issuer: FIXED_ISSUER.to_string(),
            jti: jti.to_string(),
            issued_at: FIXED_CLOCK,
        }
    }

    fn fixed_verification_context<'a>(
        challenge: &'a AgentRunChallenge,
        now: u64,
    ) -> VerificationContext<'a> {
        VerificationContext {
            issuer: FIXED_ISSUER,
            audience: "launchplane",
            challenge,
            now,
            leeway_seconds: 0,
        }
    }

    fn error_code(error: &ProvenanceError) -> String {
        match error {
            ProvenanceError::InvalidSignature => "invalid_signature",
            ProvenanceError::IssuerMismatch => "issuer_mismatch",
            ProvenanceError::AudienceMismatch => "audience_mismatch",
            ProvenanceError::ChallengeNotActive => "challenge_not_active",
            ProvenanceError::TokenIssuedInFuture => "token_issued_in_future",
            ProvenanceError::TokenNotYetValid => "token_not_yet_valid",
            ProvenanceError::TokenExpired => "token_expired",
            ProvenanceError::InvalidField("token issued-at time") => {
                "invalid_token_issued_at"
            }
            ProvenanceError::NonceMismatch => "nonce_mismatch",
            ProvenanceError::ChallengeDigestMismatch => "challenge_digest_mismatch",
            ProvenanceError::ChallengeMismatch("repository target") => {
                "challenge_target_mismatch"
            }
            ProvenanceError::ChallengeMismatch("role") => "challenge_role_mismatch",
            ProvenanceError::ChallengeMismatch("run id") => "challenge_run_mismatch",
            ProvenanceError::ModelFamilyMismatch => "model_family_mismatch",
            _ => "unexpected_error",
        }
        .to_string()
    }
}
