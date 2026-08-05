import base64
import hashlib
import importlib
import json
from pathlib import Path
from typing import Any


jwt = importlib.import_module("jwt")


FIXTURE_PATH = Path(__file__).parent / "fixtures" / "agent_run_provenance_v1.json"
MAX_VERIFICATION_LEEWAY_SECONDS = 300


def canonical_json(value: object) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def model_family(model: str) -> str:
    slug = model.lower().rsplit("/", 1)[-1]
    if slug.startswith("code-"):
        slug = slug[5:]
    parts = slug.split("-")
    if parts[0] == "gpt" and len(parts) >= 2:
        return f"gpt-{parts[1]}"
    if parts[0].startswith("o") and parts[0][1:].isdigit():
        return parts[0]
    if parts[0] == "gemini" and len(parts) >= 2:
        return f"gemini-{parts[1]}"
    if parts[0] == "claude" and len(parts) >= 3:
        return "-".join(parts[:3])
    if len(parts) > 1 and parts[-1] in {
        "chat",
        "instruct",
        "latest",
        "mini",
        "nano",
        "preview",
        "pro",
    }:
        return "-".join(parts[:-1])
    return slug


def decode_signed(token: str, public_key: Any) -> dict[str, Any]:
    return jwt.decode(
        token,
        public_key,
        algorithms=["EdDSA"],
        options={
            "require": ["iss", "aud", "iat", "nbf", "exp"],
            "verify_aud": False,
            "verify_exp": False,
            "verify_iat": False,
            "verify_iss": False,
            "verify_nbf": False,
        },
    )


def rejection_code(
    token: str,
    public_key: Any,
    fixture: dict[str, Any],
    verify_at: int,
) -> str | None:
    try:
        claims = decode_signed(token, public_key)
    except jwt.InvalidSignatureError:
        return "invalid_signature"

    challenge = fixture["challenge"]
    verification = fixture["verification"]
    leeway = verification["leeway_seconds"]
    if leeway > MAX_VERIFICATION_LEEWAY_SECONDS:
        return "invalid_verification_leeway"
    if verify_at < challenge["not_before"] or verify_at >= challenge["expires_at"]:
        return "challenge_not_active"
    header = jwt.get_unverified_header(token)
    if claims["kid"] != header.get("kid"):
        return "untrusted_key"
    if claims["version"] != "every-code.agent-run-provenance.v1":
        return "invalid_claims_version"
    if claims["iss"] != verification["issuer"]:
        return "issuer_mismatch"
    if claims["aud"] != verification["audience"] or claims["aud"] != challenge["audience"]:
        return "audience_mismatch"
    if claims["iat"] > verify_at and claims["iat"] - verify_at > leeway:
        return "token_issued_in_future"
    if claims["nbf"] > verify_at and claims["nbf"] - verify_at > leeway:
        return "token_not_yet_valid"
    if verify_at >= claims["exp"] and verify_at - claims["exp"] >= leeway:
        return "token_expired"
    if (
        claims["iat"] < challenge["issued_at"]
        or claims["iat"] < claims["nbf"]
        or claims["iat"] >= claims["exp"]
    ):
        return "invalid_token_issued_at"
    if claims["nbf"] != challenge["not_before"] or claims["exp"] != challenge["expires_at"]:
        return "challenge_validity_mismatch"
    if claims["nonce"] != challenge["nonce"]:
        return "nonce_mismatch"
    digest = hashlib.sha256(canonical_json(challenge).encode()).hexdigest()
    if claims["challenge_sha256"] != digest:
        return "challenge_digest_mismatch"
    if claims["target"] != challenge["target"]:
        return "challenge_target_mismatch"
    if claims["role"] != challenge["role"]:
        return "challenge_role_mismatch"
    if claims["run"]["run_id"] != challenge["run_id"]:
        return "challenge_run_mismatch"
    if claims["model"]["family"] != model_family(claims["model"]["model"]):
        return "model_family_mismatch"
    return None


def main() -> None:
    fixture_text = FIXTURE_PATH.read_text(encoding="utf-8")
    fixture = json.loads(fixture_text)
    expected_keys = {
        "canonical_challenge_json",
        "canonical_claims_json",
        "challenge",
        "claims",
        "fixed_clock",
        "fixed_seed_sha256",
        "jwks",
        "negative_cases",
        "schema",
        "token",
        "verification",
    }
    assert set(fixture) == expected_keys
    assert "oauth2" not in fixture_text
    assert "secret@" not in fixture_text
    assert "private transcript bytes" not in fixture_text

    challenge_json = canonical_json(fixture["challenge"])
    claims_json = canonical_json(fixture["claims"])
    assert challenge_json == fixture["canonical_challenge_json"]
    assert claims_json == fixture["canonical_claims_json"]
    assert hashlib.sha256(challenge_json.encode()).hexdigest() == fixture["claims"][
        "challenge_sha256"
    ]

    jwks = fixture["jwks"]
    assert len(jwks["keys"]) == 1
    jwk = jwks["keys"][0]
    assert jwk["kty"] == "OKP"
    assert jwk["crv"] == "Ed25519"
    assert jwk["alg"] == "EdDSA"
    public_key = jwt.PyJWK.from_dict(jwk).key

    header_segment, payload_segment, signature_segment = fixture["token"].split(".")
    assert signature_segment
    decode_segment = lambda segment: base64.urlsafe_b64decode(
        segment + "=" * (-len(segment) % 4)
    )
    assert decode_segment(header_segment) == canonical_json(
        {"alg": "EdDSA", "kid": jwk["kid"], "typ": "JWT"}
    ).encode()
    assert decode_segment(payload_segment) == claims_json.encode()
    assert jwt.get_unverified_header(fixture["token"])["kid"] == jwk["kid"]
    assert decode_signed(fixture["token"], public_key) == fixture["claims"]
    excessive_leeway = json.loads(json.dumps(fixture))
    excessive_leeway["verification"]["leeway_seconds"] = MAX_VERIFICATION_LEEWAY_SECONDS + 1
    assert (
        rejection_code(
            excessive_leeway["token"],
            public_key,
            excessive_leeway,
            excessive_leeway["fixed_clock"],
        )
        == "invalid_verification_leeway"
    )

    for negative in fixture["negative_cases"]:
        actual = rejection_code(
            negative["token"], public_key, fixture, negative["verify_at"]
        )
        assert actual == negative["expected_error"], (
            negative["name"],
            negative["expected_error"],
            actual,
        )

    print("PyJWT agent-run provenance vector verification: OK")


if __name__ == "__main__":
    main()
