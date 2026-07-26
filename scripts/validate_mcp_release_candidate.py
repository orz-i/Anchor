#!/usr/bin/env python3
"""Validate a live Coding Tools MCP release-candidate endpoint.

The script uses only the Python standard library. It performs a real
Streamable HTTP MCP session, validates the effective catalog digest, invokes
read/preflight tools, optionally performs and rolls back an isolated write,
and records the remaining ChatGPT web UI evidence gates.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import secrets
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


PROTOCOL_VERSION = "2025-11-25"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[no-untyped-def]
        return None


@dataclass
class Check:
    name: str
    ok: bool
    detail: str
    evidence: dict[str, Any] = field(default_factory=dict)


class Validator:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.base_url = args.base_url.rstrip("/")
        self.mcp_url = f"{self.base_url}/mcp"
        self.session_id: str | None = None
        self.request_id = 1
        self.checks: list[Check] = []
        self.ssl_context = ssl.create_default_context()
        if args.insecure:
            self.ssl_context.check_hostname = False
            self.ssl_context.verify_mode = ssl.CERT_NONE
        self.auth_header: str | None = None
        self.oauth_refresh_token: str | None = None
        self.oauth_client_id: str | None = None
        self.oauth_client_secret: str = ""
        if args.auth == "bearer":
            token = os.environ.get("MCP_BEARER_TOKEN", "").strip()
            if not token:
                raise SystemExit("MCP_BEARER_TOKEN is required for --auth bearer")
            self.auth_header = f"Bearer {token}"
        elif args.auth == "oauth":
            self.oauth_client_id = os.environ.get("MCP_OAUTH_CLIENT_ID", "").strip()
            password = os.environ.get("MCP_OAUTH_PASSWORD", "").strip()
            self.oauth_client_secret = os.environ.get("MCP_OAUTH_CLIENT_SECRET", "").strip()
            if not self.oauth_client_id or not password:
                raise SystemExit(
                    "MCP_OAUTH_CLIENT_ID and MCP_OAUTH_PASSWORD are required for --auth oauth"
                )
            self.oauth_password = password

    def add(self, name: str, ok: bool, detail: str, **evidence: Any) -> None:
        self.checks.append(Check(name, ok, detail, evidence))
        if not ok and self.args.fail_fast:
            raise RuntimeError(f"{name}: {detail}")

    def request(
        self,
        method: str,
        url: str,
        payload: dict[str, Any] | None = None,
        *,
        headers: dict[str, str] | None = None,
    ) -> tuple[int, dict[str, str], bytes]:
        request_headers = {
            "Accept": "application/json, text/event-stream",
            "User-Agent": "coding-tools-mcp-rc-validator/1",
        }
        if payload is not None:
            request_headers["Content-Type"] = "application/json"
        if self.auth_header:
            request_headers["Authorization"] = self.auth_header
        if headers:
            request_headers.update(headers)
        data = None if payload is None else json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            url,
            data=data,
            headers=request_headers,
            method=method,
        )
        try:
            with urllib.request.urlopen(
                request,
                timeout=self.args.timeout,
                context=self.ssl_context,
            ) as response:
                return response.status, dict(response.headers.items()), response.read()
        except urllib.error.HTTPError as error:
            return error.code, dict(error.headers.items()), error.read()

    def request_form(
        self,
        method: str,
        url: str,
        form: dict[str, str],
        *,
        follow_redirects: bool = True,
    ) -> tuple[int, dict[str, str], bytes]:
        request_headers = {
            "Accept": "application/json, text/html, */*",
            "Content-Type": "application/x-www-form-urlencoded",
            "User-Agent": "coding-tools-mcp-rc-validator/1",
        }
        data = urllib.parse.urlencode(form).encode("utf-8")
        request = urllib.request.Request(url, data=data, headers=request_headers, method=method)
        handlers: list[Any] = [urllib.request.HTTPSHandler(context=self.ssl_context)]
        if not follow_redirects:
            handlers.append(NoRedirect())
        opener = urllib.request.build_opener(*handlers)
        try:
            with opener.open(request, timeout=self.args.timeout) as response:
                return response.status, dict(response.headers.items()), response.read()
        except urllib.error.HTTPError as error:
            return error.code, dict(error.headers.items()), error.read()

    def rpc(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        request_id = self.request_id
        self.request_id += 1
        payload: dict[str, Any] = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
        }
        if params is not None:
            payload["params"] = params
        headers: dict[str, str] = {}
        if self.session_id:
            headers["MCP-Session-Id"] = self.session_id
            headers["MCP-Protocol-Version"] = PROTOCOL_VERSION
        status, response_headers, body = self.request(
            "POST", self.mcp_url, payload, headers=headers
        )
        if status != 200:
            raise RuntimeError(f"RPC {method} returned HTTP {status}: {body[:500]!r}")
        decoded = json.loads(body)
        if decoded.get("id") != request_id:
            raise RuntimeError(f"RPC {method} returned an unexpected id: {decoded}")
        if "error" in decoded:
            raise RuntimeError(f"RPC {method} failed: {decoded['error']}")
        if not self.session_id:
            self.session_id = header(response_headers, "MCP-Session-Id")
        return decoded["result"]

    def notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        payload: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            payload["params"] = params
        headers = {
            "MCP-Session-Id": required(self.session_id, "MCP session"),
            "MCP-Protocol-Version": PROTOCOL_VERSION,
        }
        status, _, body = self.request("POST", self.mcp_url, payload, headers=headers)
        if status != 202:
            raise RuntimeError(
                f"Notification {method} returned HTTP {status}: {body[:500]!r}"
            )

    def call_tool(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        result = self.rpc("tools/call", {"name": name, "arguments": arguments})
        structured = result.get("structuredContent")
        if not isinstance(structured, dict):
            raise RuntimeError(f"Tool {name} omitted structuredContent: {result}")
        if result.get("isError") is True or structured.get("ok") is False:
            raise RuntimeError(f"Tool {name} returned an error: {structured}")
        return structured

    def run(self) -> dict[str, Any]:
        started = time.time()
        self.check_get_contract()
        self.check_wrong_origin()
        self.check_oauth_metadata()
        if self.args.auth == "oauth":
            self.perform_oauth_pkce_flow()
        self.initialize()
        try:
            tools = self.check_catalog()
            self.check_read_and_preflight(tools)
            if self.args.allow_write:
                self.check_isolated_write(tools)
            if self.args.auth == "oauth":
                self.check_refresh_rotation()
        finally:
            self.delete_session()
        return {
            "schema_version": 1,
            "base_url": self.base_url,
            "auth": self.args.auth,
            "protocol_version": PROTOCOL_VERSION,
            "started_at": started,
            "duration_seconds": round(time.time() - started, 3),
            "ok": all(check.ok for check in self.checks),
            "checks": [check.__dict__ for check in self.checks],
            "chatgpt_manual_gates": [
                {
                    "name": "tool_scan_and_oauth",
                    "status": "pending_human_ui",
                    "evidence_required": "ChatGPT web developer mode scans tools and completes OAuth without callback enrollment prompts",
                },
                {
                    "name": "read_tool_invocation",
                    "status": "pending_human_ui",
                    "evidence_required": "A new chat invokes server_info/read_file and displays a successful result",
                },
                {
                    "name": "write_confirmation_and_execution",
                    "status": "pending_human_ui" if self.args.allow_write else "not_requested",
                    "evidence_required": "ChatGPT presents the expected write confirmation and apply_patch completes exactly once",
                },
                {
                    "name": "frozen_catalog_refresh",
                    "status": "pending_human_ui",
                    "evidence_required": "The approved app snapshot digest matches this report or the app is explicitly refreshed",
                },
            ],
        }

    def check_get_contract(self) -> None:
        status, headers, body = self.request("GET", self.mcp_url)
        allow = header(headers, "Allow") or ""
        self.add(
            "mcp_get_405",
            status == 405 and "POST" in allow.upper(),
            f"HTTP {status}, Allow={allow!r}",
            response_preview=body[:200].decode("utf-8", "replace"),
        )

    def check_wrong_origin(self) -> None:
        status, _, _ = self.request(
            "POST",
            self.mcp_url,
            {
                "jsonrpc": "2.0",
                "id": 999_001,
                "method": "initialize",
                "params": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "rc-origin-negative", "version": "1"},
                },
            },
            headers={"Origin": "https://attacker.invalid"},
        )
        self.add("wrong_origin_rejected", status == 403, f"HTTP {status}")

    def check_oauth_metadata(self) -> None:
        if not self.args.oauth_metadata and self.args.auth != "oauth":
            return
        for path, name in [
            ("/.well-known/oauth-protected-resource", "oauth_protected_resource_metadata"),
            ("/.well-known/oauth-authorization-server", "oauth_authorization_server_metadata"),
        ]:
            status, _, body = self.request("GET", f"{self.base_url}{path}")
            valid_json = False
            keys: list[str] = []
            try:
                parsed = json.loads(body)
                valid_json = isinstance(parsed, dict)
                keys = sorted(parsed.keys()) if valid_json else []
            except json.JSONDecodeError:
                pass
            self.add(name, status == 200 and valid_json, f"HTTP {status}", keys=keys)

    def perform_oauth_pkce_flow(self) -> None:
        initialize_payload = {
            "jsonrpc": "2.0",
            "id": 999_002,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "rc-oauth-negative", "version": "1"},
            },
        }
        status, headers, _ = self.request("POST", self.mcp_url, initialize_payload)
        challenge = header(headers, "WWW-Authenticate") or ""
        self.add(
            "oauth_unauthenticated_challenge",
            status == 401 and "resource_metadata" in challenge,
            f"HTTP {status}; WWW-Authenticate present={bool(challenge)}",
        )

        verifier = base64.urlsafe_b64encode(secrets.token_bytes(48)).decode().rstrip("=")
        challenge_value = base64.urlsafe_b64encode(
            hashlib.sha256(verifier.encode()).digest()
        ).decode().rstrip("=")
        state = secrets.token_urlsafe(18)
        redirect_uri = os.environ.get(
            "MCP_OAUTH_REDIRECT_URI",
            "https://chatgpt.com/connector/oauth/rc-validator",
        ).strip()
        client_id = required(self.oauth_client_id, "OAuth client id")
        authorize_params = {
            "response_type": "code",
            "client_id": client_id,
            "redirect_uri": redirect_uri,
            "code_challenge": challenge_value,
            "code_challenge_method": "S256",
            "state": state,
            "resource": self.mcp_url,
        }
        authorize_url = (
            f"{self.base_url}/oauth/authorize?"
            + urllib.parse.urlencode(authorize_params)
        )
        page_status, _, page = self.request("GET", authorize_url)
        self.add(
            "oauth_authorization_page",
            page_status == 200 and b"name='password'" in page,
            f"HTTP {page_status}",
        )

        approval_status, approval_headers, approval_body = self.request_form(
            "POST",
            f"{self.base_url}/oauth/authorize",
            {
                "client_id": client_id,
                "redirect_uri": redirect_uri,
                "code_challenge": challenge_value,
                "code_challenge_method": "S256",
                "state": state,
                "resource": self.mcp_url,
                "password": self.oauth_password,
            },
            follow_redirects=False,
        )
        location = header(approval_headers, "Location") or ""
        parsed = urllib.parse.urlparse(location)
        query = urllib.parse.parse_qs(parsed.query)
        code = query.get("code", [""])[0]
        returned_state = query.get("state", [""])[0]
        approval_ok = (
            approval_status in {302, 303}
            and location.startswith(redirect_uri)
            and bool(code)
            and returned_state == state
        )
        self.add(
            "oauth_authorization_code",
            approval_ok,
            f"HTTP {approval_status}; callback_host={parsed.hostname or ''}",
            response_preview=approval_body[:120].decode("utf-8", "replace"),
        )
        if not approval_ok:
            raise RuntimeError("OAuth authorization code flow did not produce a valid callback")

        token_form = {
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": redirect_uri,
            "code_verifier": verifier,
            "client_id": client_id,
            "resource": self.mcp_url,
        }
        if self.oauth_client_secret:
            token_form["client_secret"] = self.oauth_client_secret
        token_status, _, token_body = self.request_form(
            "POST", f"{self.base_url}/oauth/token", token_form
        )
        token_json = json.loads(token_body) if token_body else {}
        access_token = token_json.get("access_token", "")
        refresh_token = token_json.get("refresh_token", "")
        self.add(
            "oauth_token_exchange",
            token_status == 200 and bool(access_token) and bool(refresh_token),
            f"HTTP {token_status}; token_type={token_json.get('token_type')}",
            expires_in=token_json.get("expires_in"),
            scope=token_json.get("scope"),
        )
        if not access_token or not refresh_token:
            raise RuntimeError("OAuth token exchange did not return access and refresh tokens")
        self.auth_header = f"Bearer {access_token}"
        self.oauth_refresh_token = refresh_token

    def check_refresh_rotation(self) -> None:
        refresh_token = required(self.oauth_refresh_token, "OAuth refresh token")
        client_id = required(self.oauth_client_id, "OAuth client id")
        refresh_form = {
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": client_id,
            "resource": self.mcp_url,
        }
        if self.oauth_client_secret:
            refresh_form["client_secret"] = self.oauth_client_secret
        status, _, body = self.request_form(
            "POST", f"{self.base_url}/oauth/token", refresh_form
        )
        payload = json.loads(body) if body else {}
        new_access = payload.get("access_token", "")
        new_refresh = payload.get("refresh_token", "")
        rotated = status == 200 and bool(new_access) and bool(new_refresh) and new_refresh != refresh_token
        self.add(
            "oauth_refresh_rotation",
            rotated,
            f"HTTP {status}; rotated={new_refresh != refresh_token if new_refresh else False}",
        )
        if not rotated:
            return
        self.auth_header = f"Bearer {new_access}"
        self.oauth_refresh_token = new_refresh
        info = self.call_tool("server_info", {})
        self.add(
            "oauth_refreshed_access_token",
            info.get("server") == "coding-tools-mcp",
            f"server={info.get('server')}",
        )
        reused_status, _, reused_body = self.request_form(
            "POST", f"{self.base_url}/oauth/token", refresh_form
        )
        try:
            reused_payload = json.loads(reused_body) if reused_body else {}
        except json.JSONDecodeError:
            reused_payload = {}
        self.add(
            "oauth_old_refresh_rejected",
            reused_status == 400 and reused_payload.get("error") == "invalid_grant",
            f"HTTP {reused_status}; error={reused_payload.get('error')}",
        )

    def initialize(self) -> None:
        result = self.rpc(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "coding-tools-mcp-rc-validator", "version": "1"},
            },
        )
        negotiated = result.get("protocolVersion")
        server = result.get("serverInfo", {})
        ok = (
            negotiated == PROTOCOL_VERSION
            and bool(self.session_id)
            and server.get("name") == "coding-tools-mcp"
        )
        self.add(
            "initialize",
            ok,
            f"protocol={negotiated}, session={bool(self.session_id)}",
            server_info=server,
        )
        self.notify("notifications/initialized", {})

    def check_catalog(self) -> list[dict[str, Any]]:
        result = self.rpc("tools/list")
        tools = result.get("tools")
        if not isinstance(tools, list):
            raise RuntimeError("tools/list did not return an array")
        names = [tool.get("name") for tool in tools]
        unique = len(names) == len(set(names)) and all(isinstance(name, str) for name in names)
        schemas = all(
            isinstance(tool.get("inputSchema"), dict)
            and isinstance(tool.get("outputSchema"), dict)
            for tool in tools
        )
        digest = canonical_digest(tools)
        self.add(
            "tools_list_contract",
            unique and schemas and names == sorted(names),
            f"count={len(tools)}, digest={digest}",
            names=names,
            digest=digest,
        )
        info = self.call_tool("server_info", {})
        self.add(
            "server_info_catalog_digest",
            info.get("catalog_digest") == digest
            and info.get("tool_count") == len(tools)
            and info.get("tools") == names,
            f"server={info.get('catalog_digest')}, computed={digest}",
            tool_profile=info.get("tool_profile"),
            catalog_bytes=info.get("catalog_bytes"),
        )
        if self.args.expected_profile:
            self.add(
                "expected_tool_profile",
                info.get("tool_profile") == self.args.expected_profile,
                f"actual={info.get('tool_profile')}, expected={self.args.expected_profile}",
            )
        return tools

    def check_read_and_preflight(self, tools: list[dict[str, Any]]) -> None:
        names = {tool.get("name") for tool in tools}
        if "read_file" in names:
            result = self.call_tool("read_file", {"path": self.args.read_path})
            self.add(
                "read_file",
                result.get("path") is not None and result.get("encoding") == "utf-8",
                f"path={result.get('path')}, bytes={result.get('bytes_read')}",
            )
        if "patch_check" in names:
            patch = (
                "*** Begin Patch\n"
                "*** Add File: .rc-validator-preflight.txt\n"
                "+release candidate preflight\n"
                "*** End Patch\n"
            )
            result = self.call_tool("patch_check", {"patch": patch})
            self.add(
                "patch_check",
                result.get("dry_run") is True and result.get("preflight") is True,
                f"affected={len(result.get('affected_files', []))}",
            )

    def check_isolated_write(self, tools: list[dict[str, Any]]) -> None:
        names = {tool.get("name") for tool in tools}
        if not {"apply_patch", "read_file"}.issubset(names):
            self.add("isolated_write", False, "apply_patch/read_file not exposed")
            return
        filename = f".rc-validator-{int(time.time())}.txt"
        add_patch = (
            "*** Begin Patch\n"
            f"*** Add File: {filename}\n"
            "+release candidate write probe\n"
            "*** End Patch\n"
        )
        remove_patch = (
            "*** Begin Patch\n"
            f"*** Delete File: {filename}\n"
            "*** End Patch\n"
        )
        added = self.call_tool("apply_patch", {"patch": add_patch})
        read = self.call_tool("read_file", {"path": filename})
        removed = self.call_tool("apply_patch", {"patch": remove_patch})
        self.add(
            "isolated_write_and_rollback",
            filename in added.get("files_created", [])
            and "release candidate write probe" in read.get("content", "")
            and filename in removed.get("files_deleted", []),
            f"file={filename}",
            add_change_id=added.get("change_id"),
            remove_change_id=removed.get("change_id"),
        )

    def delete_session(self) -> None:
        if not self.session_id:
            return
        status, _, body = self.request(
            "DELETE",
            self.mcp_url,
            headers={
                "MCP-Session-Id": self.session_id,
                "MCP-Protocol-Version": PROTOCOL_VERSION,
            },
        )
        self.add(
            "session_delete",
            status == 204,
            f"HTTP {status}",
            response_preview=body[:200].decode("utf-8", "replace"),
        )


def canonical_digest(value: Any) -> str:
    encoded = json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def header(headers: dict[str, str], name: str) -> str | None:
    lowered = name.lower()
    for key, value in headers.items():
        if key.lower() == lowered:
            return value
    return None


def required(value: str | None, label: str) -> str:
    if not value:
        raise RuntimeError(f"Missing {label}")
    return value


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", required=True, help="Remote MCP site root")
    parser.add_argument("--auth", choices=["noauth", "bearer", "oauth"], default="noauth")
    parser.add_argument("--read-path", default="README.md")
    parser.add_argument("--expected-profile", choices=["core", "read-only", "advanced"])
    parser.add_argument("--oauth-metadata", action="store_true")
    parser.add_argument("--allow-write", action="store_true")
    parser.add_argument("--insecure", action="store_true")
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--fail-fast", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    validator = Validator(args)
    try:
        report = validator.run()
    except Exception as error:  # noqa: BLE001 - CLI boundary
        validator.add("validator_exception", False, str(error))
        report = {
            "schema_version": 1,
            "base_url": validator.base_url,
            "ok": False,
            "checks": [check.__dict__ for check in validator.checks],
        }
    rendered = json.dumps(report, ensure_ascii=False, indent=2) + "\n"
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(rendered, encoding="utf-8")
    sys.stdout.write(rendered)
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    raise SystemExit(main())
