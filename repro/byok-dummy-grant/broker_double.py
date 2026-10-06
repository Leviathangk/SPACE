#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["grpcio==1.75.1", "grpcio-tools==1.75.1", "protobuf>=6.31,<7"]
# ///
"""REPRO ONLY: grant-validating IntermediateKeyBroker test double.

The in-repo fixture (crates/apps/mock/branching-key-provider) ignores
AuthorizationEvidence, so it cannot show what a broker that validates the
federated grant does with a placeholder. This double implements the public v2
contract (proto/credential/provider/v2/intermediate_key_broker.proto) with the
EvidenceRequired profile:

  * evidence absent                       -> AUTHORIZATION_REJECTED "space grant is required"
  * scheme != --grant-scheme              -> AUTHORIZATION_REJECTED "unsupported grant scheme"
  * evidence not base64url-decodable      -> AUTHORIZATION_REJECTED "space grant is malformed"
  * decoded grant is not the synthetic
    JSON envelope {"sub", "org", "exp"}   -> AUTHORIZATION_REJECTED "space grant is malformed"
  * sub/org mismatch with the request     -> AUTHORIZATION_REJECTED "space grant does not match"
  * exp in the past                       -> AUTHORIZATION_REJECTED "space grant is expired"

Keys are derived deterministically from the tenant, exactly like the in-repo
mock, so issue -> persist -> unwrap round-trips. It is a fixture, not a key
authority. Evidence bytes are never logged; only their length, a short digest
and the decision are, one JSON line per RPC on stdout, so the log doubles as
the retry-cadence evidence.
"""

from __future__ import annotations

import argparse
import base64
import binascii
import hashlib
import importlib
import json
import sys
import tempfile
import time
from concurrent import futures
from pathlib import Path

import grpc
from grpc_tools import protoc

REPO = Path(__file__).resolve().parents[2]
PROTO_ROOT = REPO / "proto"
PROTO = "credential/provider/v2/intermediate_key_broker.proto"


def load_stubs():
    out = Path(tempfile.mkdtemp(prefix="ikb-stubs-"))
    include = Path(protoc.__file__).parent / "_proto"
    rc = protoc.main([
        "protoc", f"-I{PROTO_ROOT}", f"-I{include}",
        f"--python_out={out}", f"--grpc_python_out={out}", PROTO,
    ])
    if rc != 0:
        sys.exit(f"protoc failed: {rc}")
    for pkg in ("credential", "credential/provider", "credential/provider/v2"):
        (out / pkg / "__init__.py").touch()
    sys.path.insert(0, str(out))
    pb = importlib.import_module("credential.provider.v2.intermediate_key_broker_pb2")
    rpc = importlib.import_module("credential.provider.v2.intermediate_key_broker_pb2_grpc")
    return pb, rpc


def b64url_decode_strict(raw: bytes) -> bytes:
    """base64url with or without padding; any other alphabet byte is an error."""
    text = raw.decode("ascii")  # UnicodeDecodeError is a ValueError
    if any(c not in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_=" for c in text):
        raise ValueError("non-base64url byte")
    stripped = text.rstrip("=")
    if len(stripped) % 4 == 1:
        raise ValueError("impossible base64 length")
    return base64.urlsafe_b64decode(stripped + "=" * (-len(stripped) % 4))


def log(**fields) -> None:
    fields = {"ts": round(time.time(), 3), **fields}
    print(json.dumps(fields, sort_keys=True), flush=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", default="0.0.0.0:50051")
    parser.add_argument("--grant-scheme", default="urn:example:space-grant:v1")
    parser.add_argument("--max-cache-ttl", type=int, default=120,
                        help="unwrap max_cache_ttl seconds (in-repo mock uses 120)")
    args = parser.parse_args()
    pb, rpc = load_stubs()

    def scope(tenant) -> str:
        return f"{tenant.namespace}:{tenant.id}"

    def derived(domain: str, tenant) -> bytes:
        return hashlib.sha256(f"{domain}:{scope(tenant)}".encode()).digest()

    def key_id(tenant) -> str:
        return f"repro-broker-{scope(tenant)}"

    def check_evidence(tenant, authorization, present: bool) -> str | None:
        """Return a rejection detail, or None when authorization is accepted."""
        if not present:
            return "space grant is required"
        if authorization.scheme != args.grant_scheme:
            return "unsupported grant scheme"
        try:
            decoded = b64url_decode_strict(authorization.evidence)
            envelope = json.loads(decoded)
            sub, org, exp = envelope["sub"], envelope["org"], int(envelope["exp"])
        except (ValueError, binascii.Error, KeyError, TypeError):
            return "space grant is malformed"
        if sub != authorization.subject or org != tenant.id:
            return "space grant does not match the request"
        if exp <= time.time():
            return "space grant is expired"
        return None

    def evidence_fields(authorization, present: bool) -> dict:
        if not present:
            return {"evidence": None}
        ev = bytes(authorization.evidence)
        return {
            "scheme": authorization.scheme,
            "subject": authorization.subject if authorization.HasField("subject") else None,
            "evidence_len": len(ev),
            "evidence_sha256_12": hashlib.sha256(ev).hexdigest()[:12],
        }

    def bearer_ok(context) -> bool:
        md = dict(context.invocation_metadata())
        value = md.get("authorization", "")
        return value.startswith("Bearer ") and len(value) > len("Bearer ")

    class Broker(rpc.IntermediateKeyBrokerServicer):
        def IssueIntermediateKey(self, request, context):
            if not bearer_ok(context):
                log(rpc="Issue", decision="unauthenticated")
                context.abort(grpc.StatusCode.UNAUTHENTICATED, "missing bearer credential")
            tenant = request.tenant
            present = request.HasField("authorization")
            fields = {"rpc": "Issue", "tenant": scope(tenant), **evidence_fields(request.authorization, present)}
            if not tenant.namespace or not tenant.id:
                log(**fields, decision="INVALID_TENANT")
                return pb.IssueIntermediateKeyResponse(failure=pb.IssueFailure(
                    reason=pb.ISSUE_FAILURE_REASON_INVALID_TENANT,
                    detail="tenant namespace and id are required"))
            detail = check_evidence(tenant, request.authorization, present)
            if detail:
                log(**fields, decision="AUTHORIZATION_REJECTED", detail=detail)
                return pb.IssueIntermediateKeyResponse(failure=pb.IssueFailure(
                    reason=pb.ISSUE_FAILURE_REASON_AUTHORIZATION_REJECTED, detail=detail))
            log(**fields, decision="OK", key_id=key_id(tenant))
            return pb.IssueIntermediateKeyResponse(key=pb.WrappedIntermediateKey(
                key_id=key_id(tenant), wrapped_key=derived("wrapped", tenant)))

        def UnwrapIntermediateKey(self, request, context):
            if not bearer_ok(context):
                log(rpc="Unwrap", decision="unauthenticated")
                context.abort(grpc.StatusCode.UNAUTHENTICATED, "missing bearer credential")
            tenant = request.tenant
            present = request.HasField("authorization")
            fields = {"rpc": "Unwrap", "tenant": scope(tenant), **evidence_fields(request.authorization, present)}
            if not (tenant.namespace and tenant.id and request.key_id == key_id(tenant)
                    and request.wrapped_key == derived("wrapped", tenant)):
                log(**fields, decision="INVALID_KEY")
                return pb.UnwrapIntermediateKeyResponse(failure=pb.UnwrapFailure(
                    reason=pb.UNWRAP_FAILURE_REASON_INVALID_KEY, detail="unknown repro key"))
            detail = check_evidence(tenant, request.authorization, present)
            if detail:
                log(**fields, decision="AUTHORIZATION_REJECTED", detail=detail)
                return pb.UnwrapIntermediateKeyResponse(failure=pb.UnwrapFailure(
                    reason=pb.UNWRAP_FAILURE_REASON_AUTHORIZATION_REJECTED, detail=detail))
            log(**fields, decision="OK", max_cache_ttl=args.max_cache_ttl)
            material = pb.IntermediateKeyMaterial(plaintext_key=derived("plaintext", tenant))
            material.max_cache_ttl.seconds = args.max_cache_ttl
            return pb.UnwrapIntermediateKeyResponse(key=material)

    server = grpc.server(futures.ThreadPoolExecutor(max_workers=8))
    rpc.add_IntermediateKeyBrokerServicer_to_server(Broker(), server)
    server.add_insecure_port(args.listen)
    server.start()
    log(event="listening", listen=args.listen, grant_scheme=args.grant_scheme,
        max_cache_ttl=args.max_cache_ttl)
    server.wait_for_termination()


if __name__ == "__main__":
    main()
