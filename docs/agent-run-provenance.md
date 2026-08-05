# Signed Agent-Run Provenance

Every Code signs agent-run provenance as an Ed25519 compact JWS. The library
contract lives in `code-rs/code-agent-run-provenance`; CLI and exec export surfaces
are intentionally outside this contract slice.

## Trust Boundary

- Runtime observations are the only source for repository, pull request,
  commit/tree, run, conversation, model/provider/family, task, transcript, and
  runtime identity claims.
- A challenge constrains role, run, target, audience, nonce, and validity. It
  cannot supply claim values: issuance fails when a constraint differs from an
  observation.
- A challenge is not self-authenticating. The issuer must receive it through an
  authenticated channel, and the verifier must compare the token against the
  original challenge retained in trusted verifier storage. The digest proves
  which challenge was signed; it does not establish who created the challenge.
- Model family is derived from the observed model slug. Callers cannot provide
  it.
- Repository remotes are reduced to `host[:non-default-port]/path` before
  signing. User names, passwords, query strings, fragments, default transport
  ports, and raw remote URLs are never emitted.
- Transcript bytes are streamed into a domain-separated SHA-256 digest and
  discarded. The token records the `every-code.transcript-bytes.v1` format,
  digest, and byte count, but never transcript contents.
- Provenance keys are generated and loaded independently from account-scoped
  backend agent-identity keys.
- Possession of a provenance signing key defines the trusted Every Code runtime
  boundary. The future exec integration must construct `ObservedAgentRun` only
  from runtime state; caller text and challenge fields are not observation
  sources.
- This crate prevents duplicate or conflicting issuance. Verifier-side proof
  consumption, challenge-origin authentication, approval policy, and accepted
  token replay handling remain Launchplane responsibilities.

## Wire Contract

The protected header is canonical JSON with `alg=EdDSA`, `typ=JWT`, and `kid`.
Claims use ordinary JWT `iss`, `aud`, `jti`, `iat`, `nbf`, and `exp` fields plus
the challenge nonce and SHA-256 digest. Challenge and JWS JSON use UTF-8,
lexicographically sorted schema keys, no insignificant whitespace, and JSON
string escaping without ASCII-only rewriting before hashing or signing. Python
implementations can reproduce this with `sort_keys=True`, `ensure_ascii=False`,
and separators `(',', ':')`; signed contract structs reject unknown fields.

Verification requires the expected issuer/audience, a matching Ed25519 JWKS
key, a live validity window, the original challenge, normalized target/run/role
equality, and a model family re-derived from the signed runtime model slug.
Verifier clock leeway is bounded to 300 seconds so configuration cannot silently
disable token expiry. Leeway applies to token timestamps only; it never extends
the verifier-issued challenge authorization window.

## Replay Rules

The issuance ledger is append-only JSON Lines. A per-ledger OS file lock covers
reload, uniqueness checks, one-buffer append, and sync so independent issuers
cannot race. A `jti` can be recorded once. A run ID can also be recorded once:
a second issuance with the same role is a replay, while a different role is a
role conflict. Existing entries are checked when the ledger opens. The ledger
assumes its directory is writable only by the trusted Every Code runtime; it is
not an integrity anchor against an attacker who can rewrite local state. If a
crash leaves an incomplete final JSON line, reopening truncates only that
uncommitted suffix. A complete final entry missing only its newline is preserved
and repaired; complete malformed entries remain a hard failure.

## Interoperability Vector

The fixture under
`code-rs/code-agent-run-provenance/tests/fixtures/agent_run_provenance_v1.json`
uses a fixed Ed25519 seed and fixed clock. It contains the public JWKS,
challenge, claims, compact token, verifier inputs, and signed negative tokens.
PyJWT verifiers should select `kid`, require `EdDSA`, `iss`, `aud`, `iat`,
`nbf`, and `exp`, then apply the challenge and Every Code claim checks described
above.

Run the independent cross-language fixture check with:

```console
uv run --with pyjwt --with cryptography python \
  code-rs/code-agent-run-provenance/tests/verify_vector.py
```
