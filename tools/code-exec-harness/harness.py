#!/usr/bin/env python3
from __future__ import annotations

import argparse
import http.server
import json
import os
import re
import shutil
import socketserver
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, cast
from urllib.parse import urlparse


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUTPUT_ROOT = ROOT / ".tmp" / "code-exec-harness"


@dataclass
class RunPaths:
    run_dir: Path
    workspace: Path
    code_home: Path
    bin_dir: Path
    shell_home: Path
    artifacts: Path


class HarnessError(RuntimeError):
    pass


class FakeResponsesServer:
    def __init__(self, fixture: dict[str, Any], artifacts: Path) -> None:
        self.fixture = fixture
        self.artifacts = artifacts
        self.requests: list[dict[str, Any]] = []
        self._responses = list(fixture.get("responses", []))
        self._next_response_index = 0
        self._lock = threading.Lock()
        self._httpd: socketserver.ThreadingTCPServer | None = None
        self._thread: threading.Thread | None = None
        self.base_url = ""

    def __enter__(self) -> "FakeResponsesServer":
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_POST(self) -> None:  # noqa: N802
                owner._handle_post(self)

            def log_message(self, format: str, *args: Any) -> None:
                return

        self._httpd = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
        self._httpd.daemon_threads = True
        port = self._httpd.server_address[1]
        self.base_url = f"http://127.0.0.1:{port}/v1"
        self._thread = threading.Thread(target=self._httpd.serve_forever, daemon=True)
        self._thread.start()
        return self

    def __exit__(self, _exc_type: object, _exc: object, _tb: object) -> None:
        if self._httpd is not None:
            self._httpd.shutdown()
            self._httpd.server_close()
        if self._thread is not None:
            self._thread.join(timeout=5)
        self.write_artifacts()

    def _record_request_and_next_response(self, request: dict[str, Any]) -> dict[str, Any]:
        with self._lock:
            index = self._next_response_index
            self._next_response_index += 1
            self.requests.append(request)
            if index >= len(self._responses):
                return {"status": 500, "body": {"error": f"missing fake response for request {index + 1}"}}
            return self._responses[index]

    def _handle_post(self, handler: http.server.BaseHTTPRequestHandler) -> None:
        parsed = urlparse(handler.path)
        length = int(handler.headers.get("content-length", "0"))
        raw_body = handler.rfile.read(length).decode("utf-8")
        try:
            body = json.loads(raw_body) if raw_body else None
        except json.JSONDecodeError:
            body = {"_invalid_json": raw_body}
        response = self._record_request_and_next_response(
            {"method": "POST", "path": parsed.path, "body": body}
        )

        if not parsed.path.endswith("/responses"):
            self._send_json(handler, 404, {"error": f"unexpected path {parsed.path}"})
            return

        status = int(response.get("status", 200))
        if response.get("body") is not None:
            self._send_json(handler, status, response.get("body", {}))
        else:
            self._send_sse(handler, status, response_sse_body(response))

    def _send_json(self, handler: http.server.BaseHTTPRequestHandler, status: int, body: Any) -> None:
        data = json.dumps(body).encode("utf-8")
        handler.send_response(status)
        handler.send_header("content-type", "application/json")
        handler.send_header("content-length", str(len(data)))
        handler.end_headers()
        handler.wfile.write(data)

    def _send_sse(self, handler: http.server.BaseHTTPRequestHandler, status: int, body: str) -> None:
        data = body.encode("utf-8")
        handler.send_response(status)
        handler.send_header("content-type", "text/event-stream")
        handler.send_header("cache-control", "no-cache")
        handler.send_header("content-length", str(len(data)))
        handler.end_headers()
        handler.wfile.write(data)

    def write_artifacts(self) -> None:
        put_json(self.artifacts / "responses-requests.json", self.requests)


def data_image_payload_bytes(value: Any) -> int:
    if isinstance(value, str):
        return len(value) if value.startswith("data:image/") else 0
    if isinstance(value, list):
        return sum(data_image_payload_bytes(item) for item in value)
    if isinstance(value, dict):
        return sum(data_image_payload_bytes(item) for item in value.values())
    return 0


def contains_text(value: Any, needle: str) -> bool:
    if isinstance(value, str):
        return needle in value
    if isinstance(value, list):
        return any(contains_text(item, needle) for item in value)
    if isinstance(value, dict):
        return any(contains_text(item, needle) for item in value.values())
    return False


def count_text(value: Any, needle: str) -> int:
    if isinstance(value, str):
        return value.count(needle)
    if isinstance(value, list):
        return sum(count_text(item, needle) for item in value)
    if isinstance(value, dict):
        return sum(count_text(item, needle) for item in value.values())
    return 0


def expand_fixture_value(value: Any) -> Any:
    if isinstance(value, dict):
        if set(value.keys()) == {"$repeat", "count"}:
            return str(value["$repeat"]) * int(value["count"])
        return {key: expand_fixture_value(child) for key, child in value.items()}
    if isinstance(value, list):
        return [expand_fixture_value(item) for item in value]
    return value


def sse_event(event_type: str, payload: dict[str, Any]) -> str:
    event = expand_fixture_value(payload)
    return f"event: {event_type}\ndata: {json.dumps(event)}\n\n"


def completed_sse(response_id: str) -> str:
    return sse_event(
        "response.completed",
        {
            "type": "response.completed",
            "response": {
                "id": response_id,
                "usage": {
                    "input_tokens": 0,
                    "input_tokens_details": None,
                    "output_tokens": 0,
                    "output_tokens_details": None,
                    "total_tokens": 0,
                },
                "output": [],
            },
        },
    )


def response_sse_body(response: dict[str, Any]) -> str:
    if "sse" in response:
        return str(response["sse"])
    chunks: list[str] = []
    for event in response.get("events", []):
        if not isinstance(event, dict):
            raise HarnessError("responses_api events must be objects")
        event_type = str(event.get("event", event.get("type", "response.output_item.done")))
        payload = event.get("payload")
        if payload is None and "item" in event:
            payload = {"type": "response.output_item.done", "item": event["item"]}
            event_type = "response.output_item.done"
        if not isinstance(payload, dict):
            raise HarnessError("responses_api event payload must be an object")
        chunks.append(sse_event(event_type, payload))
    if response.get("completed", True):
        chunks.append(completed_sse(str(response.get("response_id", "resp_harness"))))
    return "".join(chunks)


def read_text(path: Path) -> str:
    with path.open("r", encoding="utf-8") as handle:
        return handle.read()


def put_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        handle.write(text)


def load_json(path: Path) -> dict[str, Any]:
    try:
        return json.loads(read_text(path))
    except json.JSONDecodeError as exc:
        raise HarnessError(f"invalid JSON in {path}: {exc}") from exc


def put_json(path: Path, data: Any) -> None:
    put_text(path, json.dumps(data, indent=2, sort_keys=True) + "\n")


def scenario_name(path: Path, scenario: dict[str, Any]) -> str:
    raw = str(scenario.get("name") or path.stem)
    name = re.sub(r"[^A-Za-z0-9_.-]+", "-", raw).strip("-._")
    return name or "scenario"


def make_paths(output_root: Path, name: str) -> RunPaths:
    stamp = time.strftime("%Y%m%d-%H%M%S")
    run_dir = output_root / f"{stamp}-{name}"
    paths = RunPaths(
        run_dir=run_dir,
        workspace=run_dir / "workspace",
        code_home=run_dir / "code-home",
        bin_dir=run_dir / "bin",
        shell_home=run_dir / "shell-home",
        artifacts=run_dir / "artifacts",
    )
    for path in (paths.workspace, paths.code_home, paths.bin_dir, paths.shell_home, paths.artifacts):
        path.mkdir(parents=True, exist_ok=True)
    return paths


def resolve_path(value: str, base: Path) -> Path:
    expanded = Path(os.path.expandvars(os.path.expanduser(value)))
    if expanded.is_absolute():
        return expanded
    return (base / expanded).resolve()


def copy_or_link(src: Path, dst: Path, *, symlink: bool) -> None:
    if dst.exists() or dst.is_symlink():
        if dst.is_dir() and not dst.is_symlink():
            shutil.rmtree(dst)
        else:
            dst.unlink()
    dst.parent.mkdir(parents=True, exist_ok=True)
    if symlink:
        dst.symlink_to(src, target_is_directory=src.is_dir())
    elif src.is_dir():
        shutil.copytree(src, dst, symlinks=True)
    else:
        shutil.copy2(src, dst)


def run_quiet(command: list[str], cwd: Path) -> None:
    subprocess.run(command, cwd=cwd, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def materialize_workspace(scenario: dict[str, Any], paths: RunPaths) -> None:
    files = scenario.get("files", {})
    if not isinstance(files, dict):
        raise HarnessError("scenario `files` must be an object mapping paths to content")
    for relative, content in files.items():
        destination = paths.workspace / relative
        put_text(destination, str(content))

    copies = scenario.get("copy", [])
    if not isinstance(copies, list):
        raise HarnessError("scenario `copy` must be a list")
    for entry in copies:
        if not isinstance(entry, dict) or "source" not in entry or "target" not in entry:
            raise HarnessError("each `copy` entry must contain source and target")
        source = resolve_path(str(entry["source"]), ROOT)
        target = paths.workspace / str(entry["target"])
        copy_or_link(source, target, symlink=False)

    if scenario.get("git_init", True):
        run_quiet(["git", "init", "-q"], cwd=paths.workspace)
        run_quiet(["git", "config", "user.email", "harness@example.invalid"], cwd=paths.workspace)
        run_quiet(["git", "config", "user.name", "Code Exec Harness"], cwd=paths.workspace)
        run_quiet(["git", "add", "."], cwd=paths.workspace)
        run_quiet(["git", "commit", "-q", "--allow-empty", "-m", "Initial fixture"], cwd=paths.workspace)


def materialize_skills(scenario: dict[str, Any], paths: RunPaths, scenario_dir: Path, extra_roots: list[Path]) -> None:
    skills_dir = paths.code_home / "skills"
    skills_dir.mkdir(parents=True, exist_ok=True)
    roots: list[Path] = []
    for value in scenario.get("skill_roots", []):
        roots.append(resolve_path(str(value), scenario_dir))
    roots.extend(extra_roots)

    for root in roots:
        if not root.exists():
            raise HarnessError(f"skill root does not exist: {root}")
        if (root / "SKILL.md").is_file():
            copy_or_link(root, skills_dir / root.name, symlink=True)
            continue
        for child in sorted(root.iterdir()):
            if child.is_dir() and (child / "SKILL.md").is_file():
                copy_or_link(child, skills_dir / child.name, symlink=True)


def write_config(scenario: dict[str, Any], paths: RunPaths) -> None:
    config = str(scenario.get("config_toml", "")).strip()
    if config:
        put_text(paths.code_home / "config.toml", config + "\n")


def gh_config_source() -> Path:
    if os.environ.get("GH_CONFIG_DIR"):
        return Path(os.environ["GH_CONFIG_DIR"]).expanduser()
    xdg_config_home = os.environ.get("XDG_CONFIG_HOME")
    if xdg_config_home:
        return Path(xdg_config_home).expanduser() / "gh"
    return Path.home() / ".config" / "gh"


def inherit_code_auth(paths: RunPaths) -> None:
    source_home = Path(os.environ.get("CODE_HOME") or os.environ.get("CODEX_HOME") or Path.home() / ".code")
    for name in ("auth.json", ".credentials.json"):
        source = source_home / name
        if source.is_file():
            shutil.copy2(source, paths.code_home / name)


def inherit_gh_auth(paths: RunPaths) -> dict[str, str]:
    source_gh_config = gh_config_source()
    if not source_gh_config.is_dir():
        return {}

    target_gh_config = paths.shell_home / ".config" / "gh"
    copy_or_link(source_gh_config, target_gh_config, symlink=False)
    return {"GH_CONFIG_DIR": str(target_gh_config)}


def auth_inheritance_requested(scenario: dict[str, Any], args: argparse.Namespace) -> bool:
    return bool(args.inherit_auth or scenario.get("inherit_auth", False))


def fake_responses_enabled(scenario: dict[str, Any]) -> bool:
    return isinstance(scenario.get("responses_api"), dict)


FAKE_GH = r'''#!/usr/bin/env python3
import json
import os
import re
import sys
import time
from pathlib import Path

fixture_path = Path(os.environ["CODE_EXEC_HARNESS_GH_FIXTURE"])
log_path = Path(os.environ["CODE_EXEC_HARNESS_GH_LOG"])
state_path = Path(os.environ["CODE_EXEC_HARNESS_GH_STATE"])

def slurp(path):
    with path.open("r", encoding="utf-8") as handle:
        return handle.read()

def put(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        handle.write(text)

fixture = json.loads(slurp(fixture_path)) if fixture_path.exists() else {}
args = sys.argv[1:]
argv_text = " ".join(args)
log_path.parent.mkdir(parents=True, exist_ok=True)
with log_path.open("a", encoding="utf-8") as log:
    log.write(json.dumps({"argv": args, "cwd": os.getcwd(), "time": time.time()}) + "\n")

def load_state():
    if state_path.exists():
        return json.loads(slurp(state_path))
    issues = {str(issue.get("number")): issue for issue in fixture.get("issues", [])}
    next_issue = int(fixture.get("next_issue", 1000))
    return {"issues": issues, "next_issue": next_issue, "links": []}

def save_state(state):
    put(state_path, json.dumps(state, indent=2, sort_keys=True) + "\n")

def finish(stdout="", stderr="", exit_code=0):
    if stdout:
        print(stdout)
    if stderr:
        print(stderr, file=sys.stderr)
    raise SystemExit(exit_code)

def fields_from_args():
    fields = {}
    index = 0
    while index < len(args):
        arg = args[index]
        if arg in {"-F", "--field", "-f", "--raw-field"} and index + 1 < len(args):
            field = args[index + 1]
            if "=" in field:
                key, value = field.split("=", 1)
                fields[key] = value
                index += 2
                continue
            if index + 2 < len(args):
                fields[field] = args[index + 2]
                index += 3
                continue
        index += 1
    return fields

for response in fixture.get("responses", []):
    match = response.get("match", {})
    matched = True
    if "exact" in match:
        matched = argv_text == match["exact"]
    if matched and "prefix" in match:
        matched = argv_text.startswith(match["prefix"])
    if matched and "contains" in match:
        contains = match["contains"]
        if isinstance(contains, str):
            contains = [contains]
        matched = all(item in argv_text for item in contains)
    if matched and "regex" in match:
        matched = re.search(match["regex"], argv_text) is not None
    if matched:
        finish(response.get("stdout", ""), response.get("stderr", ""), int(response.get("exit_code", 0)))

repo = fixture.get("repo", "owner/repo")
state = load_state()

if args[:2] == ["repo", "view"]:
    finish(json.dumps({"nameWithOwner": repo, "defaultBranchRef": {"name": fixture.get("default_branch", "main")}}))

if args[:2] == ["issue", "list"]:
    issues = list(state["issues"].values())
    if "--json" in args:
        finish(json.dumps(issues))
    finish("\n".join(f"{issue.get('number')}\t{issue.get('state', 'OPEN')}\t{issue.get('title', '')}" for issue in issues))

if args[:2] == ["issue", "create"]:
    number = int(state["next_issue"])
    state["next_issue"] = number + 1
    title = "Untitled"
    body = ""
    labels = []
    for index, arg in enumerate(args):
        if arg == "--title" and index + 1 < len(args):
            title = args[index + 1]
        elif arg == "--body" and index + 1 < len(args):
            body = args[index + 1]
        elif arg == "--body-file" and index + 1 < len(args):
            path = args[index + 1]
            if path == "-":
                body = sys.stdin.read()
            else:
                body = slurp(Path(path))
        elif arg == "--label" and index + 1 < len(args):
            labels.extend(part.strip() for part in args[index + 1].split(",") if part.strip())
    issue = {
        "number": number,
        "title": title,
        "body": body,
        "labels": [{"name": label} for label in labels],
        "state": "OPEN",
        "url": f"https://github.com/{repo}/issues/{number}",
        "subIssues": [],
    }
    state["issues"][str(number)] = issue
    save_state(state)
    if "--json" in args:
        finish(json.dumps(issue))
    finish(issue["url"])

if args[:2] == ["issue", "edit"] and len(args) >= 3:
    number = args[2].lstrip("#")
    issue = state["issues"].setdefault(number, {"number": int(number), "title": "", "state": "OPEN", "subIssues": []})
    for index, arg in enumerate(args):
        if arg == "--title" and index + 1 < len(args):
            issue["title"] = args[index + 1]
        elif arg == "--body" and index + 1 < len(args):
            issue["body"] = args[index + 1]
        elif arg == "--body-file" and index + 1 < len(args):
            path = args[index + 1]
            issue["body"] = sys.stdin.read() if path == "-" else slurp(Path(path))
    if "--add-sub-issue" in args:
        for index, arg in enumerate(args):
            if arg == "--add-sub-issue" and index + 1 < len(args):
                child = args[index + 1].lstrip("#")
                issue.setdefault("subIssues", []).append({"number": int(child)})
                state.setdefault("links", []).append({"type": "subissue", "parent": int(number), "child": int(child)})
    save_state(state)
    finish(issue.get("url", f"https://github.com/{repo}/issues/{number}"))

if args and args[0] == "api":
    joined = " ".join(args)
    match = re.search(r"repos/([^/]+)/([^/]+)/issues/(\d+)/sub_issues", joined)
    child = fields_from_args().get("sub_issue_id")
    if match and child:
        parent = match.group(3)
        issue = state["issues"].setdefault(parent, {"number": int(parent), "title": "", "state": "OPEN", "subIssues": []})
        issue.setdefault("subIssues", []).append({"number": int(child)})
        state.setdefault("links", []).append({"type": "subissue", "parent": int(parent), "child": int(child)})
        save_state(state)
        finish(json.dumps({"parent": int(parent), "child": int(child)}))

if args[:2] == ["issue", "view"] and len(args) >= 3:
    number = args[2].lstrip("#")
    issue = state["issues"].get(number)
    if not issue:
        finish(stderr=f"issue not found: {number}", exit_code=1)
    finish(json.dumps(issue) if "--json" in args else issue.get("body", issue.get("title", "")))

if args[:2] == ["issue", "comment"] and len(args) >= 3:
    finish(f"https://github.com/{repo}/issues/{args[2].lstrip('#')}#issuecomment-1")

default = fixture.get("default_response")
if default:
    finish(default.get("stdout", ""), default.get("stderr", ""), int(default.get("exit_code", 0)))
finish(stderr=f"fake gh has no response for: {argv_text}", exit_code=1)
'''


def write_fake_gh(scenario: dict[str, Any], paths: RunPaths) -> dict[str, Path] | None:
    gh_fixture = scenario.get("gh")
    if gh_fixture is None:
        return None
    fixture_path = paths.artifacts / "gh-fixture.json"
    log_path = paths.artifacts / "gh-calls.jsonl"
    state_path = paths.artifacts / "gh-state.json"
    put_json(fixture_path, gh_fixture)
    shim = paths.bin_dir / "gh"
    put_text(shim, FAKE_GH)
    shim.chmod(0o755)
    put_text(paths.shell_home / ".zshenv", f"gh() {{ {shim} \"$@\"; }}\n")
    return {"fixture": fixture_path, "log": log_path, "state": state_path}


def build_command(scenario: dict[str, Any], args: argparse.Namespace, paths: RunPaths) -> list[str]:
    return build_command_for_prompt(scenario, args, paths, str(scenario.get("prompt", "")), None)


def build_command_for_prompt(
    scenario: dict[str, Any],
    args: argparse.Namespace,
    paths: RunPaths,
    prompt: str,
    resume_session_id: str | None,
) -> list[str]:
    code_bin_value = args.code_bin or shutil.which("code")
    if not code_bin_value:
        raise HarnessError("could not find `code`; pass --code-bin")
    code_bin = Path(code_bin_value)
    command = [str(code_bin), "exec", "--json", "--skip-git-repo-check"]
    max_seconds = scenario.get("max_seconds", args.max_seconds)
    if max_seconds:
        command.extend(["--max-seconds", str(max_seconds)])
    command.extend(["-C", str(paths.workspace)])
    if scenario.get("include_plan_tool", False):
        command.append("--include-plan-tool")
    if scenario.get("auto", False):
        command.append("--auto")
    if scenario.get("auto_review", False):
        command.append("--auto-review")
    model = scenario.get("model") or args.model
    if model:
        command.extend(["-m", str(model)])
    sandbox = scenario.get("sandbox") or args.sandbox
    if sandbox:
        command.extend(["--sandbox", str(sandbox)])
    for override in scenario.get("config_overrides", []):
        command.extend(["-c", str(override)])
    if resume_session_id:
        command.extend(["resume", resume_session_id, prompt])
    else:
        command.append(prompt)
    return command


def run_exec(command: list[str], scenario: dict[str, Any], paths: RunPaths, env: dict[str, str]) -> tuple[int, list[dict[str, Any]]]:
    timeout = int(scenario.get("timeout_seconds", 180))
    proc = subprocess.Popen(command, cwd=paths.workspace, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        stdout, stderr = proc.communicate()
        stderr = (stderr or "") + f"\nHARNESS TIMEOUT after {timeout}s\n"
    put_text(paths.artifacts / "stdout.jsonl", stdout or "")
    put_text(paths.artifacts / "stderr.log", stderr or "")

    events: list[dict[str, Any]] = []
    for line_number, line in enumerate((stdout or "").splitlines(), start=1):
        if not line.strip():
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError as exc:
            events.append({"type": "harness.invalid_json", "line": line_number, "text": line, "error": str(exc)})
    return proc.returncode, events


def run_exec_capture(
    command: list[str],
    scenario: dict[str, Any],
    paths: RunPaths,
    env: dict[str, str],
    label: str,
) -> tuple[int, list[dict[str, Any]]]:
    returncode, events = run_exec(command, scenario, paths, env)
    stdout_path = paths.artifacts / "stdout.jsonl"
    stderr_path = paths.artifacts / "stderr.log"
    if stdout_path.exists():
        shutil.copy2(stdout_path, paths.artifacts / f"{label}-stdout.jsonl")
    if stderr_path.exists():
        shutil.copy2(stderr_path, paths.artifacts / f"{label}-stderr.log")
    return returncode, events


def summarize(events: list[dict[str, Any]], paths: RunPaths, returncode: int, command: list[str]) -> dict[str, Any]:
    final_message = None
    commands = []
    running_commands: dict[str, str] = {}
    file_changes = []
    usage = None
    thread_id = None
    errors = []
    for event in events:
        event_type = event.get("type")
        raw_msg = event.get("msg")
        msg: dict[str, Any] = raw_msg if isinstance(raw_msg, dict) else {}
        msg_type = msg.get("type")
        if event_type == "thread.started":
            thread_id = event.get("thread_id")
        elif event_type == "turn.completed":
            usage = event.get("usage")
        elif msg_type == "session_configured":
            thread_id = msg.get("session_id") or thread_id
        elif msg_type == "token_count":
            usage = (msg.get("info") or {}).get("total_token_usage") or usage
        elif msg_type == "agent_message":
            final_message = msg.get("message")
        elif msg_type == "exec_command_begin":
            call_id = msg.get("call_id")
            if isinstance(call_id, str):
                raw_command = msg.get("command", [])
                running_commands[call_id] = " ".join(raw_command) if isinstance(raw_command, list) else str(raw_command)
        elif msg_type == "exec_command_end":
            call_id = msg.get("call_id")
            commands.append({
                "command": running_commands.pop(call_id, None) if isinstance(call_id, str) else None,
                "exit_code": msg.get("exit_code"),
                "status": "completed" if msg.get("exit_code") == 0 else "failed",
                "stdout": msg.get("stdout"),
                "stderr": msg.get("stderr"),
            })
        elif msg_type == "custom_tool_call_end" and msg.get("tool_name") == "wait":
            waited_call_id = ((msg.get("parameters") or {}).get("call_id"))
            result_payload = msg.get("result") or {}
            wait_result = result_payload.get("Ok") if "Ok" in result_payload else result_payload.get("Err")
            completed = parse_wait_completed_command(wait_result)
            if isinstance(waited_call_id, str) and completed is not None:
                commands.append({
                    "command": running_commands.pop(waited_call_id, None),
                    "exit_code": completed.get("exit_code"),
                    "status": "completed" if completed.get("exit_code") == 0 else "failed",
                    "stdout": completed.get("stdout") or completed.get("output"),
                    "stderr": completed.get("stderr"),
                })
        elif event_type in {"error", "turn.failed"}:
            errors.append(event)
        item = event.get("item") or {}
        item_type = item.get("type")
        if event_type == "item.completed" and item_type == "agent_message":
            final_message = item.get("text")
        elif event_type == "item.completed" and item_type == "command_execution":
            commands.append({"command": item.get("command"), "exit_code": item.get("exit_code"), "status": item.get("status")})
        elif event_type == "item.completed" and item_type == "file_change":
            file_changes.append(item)

    for call_id, command in running_commands.items():
        commands.append({
            "command": command,
            "exit_code": None,
            "status": "started",
            "call_id": call_id,
        })

    gh_calls = []
    gh_log = paths.artifacts / "gh-calls.jsonl"
    if gh_log.exists():
        for line in read_text(gh_log).splitlines():
            gh_calls.append(json.loads(line))
    gh_state = None
    gh_state_path = paths.artifacts / "gh-state.json"
    if gh_state_path.exists():
        gh_state = load_json(gh_state_path)

    return {
        "returncode": returncode,
        "thread_id": thread_id,
        "usage": usage,
        "event_count": len(events),
        "final_message": final_message,
        "commands": commands,
        "file_changes": file_changes,
        "errors": errors,
        "gh_calls": gh_calls,
        "gh_state": gh_state,
        "command": command,
        "run_dir": str(paths.run_dir),
    }


def parse_wait_completed_command(wait_result: Any) -> dict[str, Any] | None:
    if not isinstance(wait_result, str):
        return None
    try:
        payload = json.loads(wait_result)
    except json.JSONDecodeError:
        return None
    if not isinstance(payload, dict):
        return None
    if "exit_code" not in payload and "metadata" in payload and isinstance(payload["metadata"], dict):
        payload = payload | {"exit_code": payload["metadata"].get("exit_code")}
    if "exit_code" not in payload and "output" in payload:
        payload = payload | {"exit_code": 1}
    if "exit_code" not in payload:
        return None
    return payload


def session_id_from_summary(summary: dict[str, Any]) -> str:
    thread_id = summary.get("thread_id")
    if not isinstance(thread_id, str) or not thread_id:
        raise HarnessError("could not determine session id from first turn")
    return thread_id


def latest_session_id(paths: RunPaths) -> str | None:
    catalog = paths.code_home / "sessions" / "index" / "catalog.jsonl"
    if not catalog.is_file():
        return None
    latest: tuple[str, str] | None = None
    for line in read_text(catalog).splitlines():
        if not line.strip():
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        session_id = entry.get("session_id")
        last_event_at = entry.get("last_event_at") or entry.get("created_at") or ""
        if isinstance(session_id, str) and isinstance(last_event_at, str):
            candidate = (last_event_at, session_id)
            if latest is None or candidate > latest:
                latest = candidate
    return latest[1] if latest else None


def session_id_from_summary_or_catalog(summary: dict[str, Any], paths: RunPaths) -> str:
    try:
        return session_id_from_summary(summary)
    except HarnessError:
        session_id = latest_session_id(paths)
        if session_id:
            return session_id
        raise


def request_at(summary: dict[str, Any], index: int) -> dict[str, Any]:
    requests = summary.get("responses_requests")
    if not isinstance(requests, list) or index >= len(requests):
        raise HarnessError(f"missing captured responses request at index {index}")
    request = requests[index]
    if not isinstance(request, dict):
        raise HarnessError(f"captured responses request {index} is not an object")
    return request


def request_assertion_target(request: dict[str, Any], assertion: dict[str, Any]) -> Any:
    body = request.get("body")
    scope = str(assertion.get("scope", "body"))
    if scope == "body":
        return body
    if scope == "input":
        return body.get("input") if isinstance(body, dict) else None
    if scope == "instructions":
        return body.get("instructions") if isinstance(body, dict) else None
    raise HarnessError(f"unsupported responses assertion scope: {scope}")


def assert_expectations(summary: dict[str, Any], scenario: dict[str, Any]) -> list[str]:
    failures: list[str] = []
    expect = scenario.get("expect", {})
    final_message = summary.get("final_message") or ""
    for needle in expect.get("assistant_contains", []):
        if str(needle) not in final_message:
            failures.append(f"assistant message did not contain {needle!r}")
    for needle in expect.get("command_contains", []):
        if not any(str(needle) in str(command.get("command")) for command in summary.get("commands", [])):
            failures.append(f"no command contained {needle!r}")
    for needle in expect.get("gh_contains", []):
        text = "\n".join(" ".join(call.get("argv", [])) for call in summary.get("gh_calls", []))
        if str(needle) not in text:
            failures.append(f"no fake gh call contained {needle!r}")
    stderr_log = ""
    run_dir = summary.get("run_dir")
    if isinstance(run_dir, str):
        stderr_path = Path(run_dir) / "artifacts" / "stderr.log"
        if stderr_path.exists():
            stderr_log = read_text(stderr_path)
    for needle in expect.get("stderr_contains", []):
        if str(needle) not in stderr_log:
            failures.append(f"stderr log did not contain {needle!r}")
    if "returncode" in expect and int(expect["returncode"]) != int(summary.get("returncode", -1)):
        failures.append(f"returncode expected {expect['returncode']}, got {summary.get('returncode')}")
    if "responses_request_count" in expect:
        actual = len(summary.get("responses_requests") or [])
        expected = int(expect["responses_request_count"])
        if actual != expected:
            failures.append(f"responses request count expected {expected}, got {actual}")
    for assertion in expect.get("responses", []):
        if not isinstance(assertion, dict):
            failures.append("responses expectation entries must be objects")
            continue
        try:
            request = request_at(summary, int(assertion.get("request", 0)))
        except HarnessError as exc:
            failures.append(str(exc))
            continue
        try:
            target = request_assertion_target(request, assertion)
        except HarnessError as exc:
            failures.append(str(exc))
            continue
        if "image_payload_bytes" in assertion:
            actual = data_image_payload_bytes(target)
            expected = int(assertion["image_payload_bytes"])
            if actual != expected:
                failures.append(
                    f"responses request {assertion.get('request', 0)} image payload bytes expected {expected}, got {actual}"
                )
        if "contains" in assertion and not contains_text(target, str(assertion["contains"])):
            failures.append(
                f"responses request {assertion.get('request', 0)} did not contain {assertion['contains']!r}"
            )
        for needle in assertion.get("contains_all", []):
            if not contains_text(target, str(needle)):
                failures.append(
                    f"responses request {assertion.get('request', 0)} did not contain {needle!r}"
                )
        if "not_contains" in assertion and contains_text(target, str(assertion["not_contains"])):
            failures.append(
                f"responses request {assertion.get('request', 0)} unexpectedly contained {assertion['not_contains']!r}"
            )
        counts = assertion.get("count")
        if isinstance(counts, dict):
            for needle, expected in counts.items():
                actual = count_text(target, str(needle))
                if actual != int(expected):
                    failures.append(
                        f"responses request {assertion.get('request', 0)} count for {needle!r} expected {expected}, got {actual}"
                    )
    return failures


def run_scenario(path: Path, args: argparse.Namespace) -> int:
    scenario = load_json(path)
    name = scenario_name(path, scenario)
    paths = make_paths(resolve_path(str(args.output_root), Path.cwd()), name)
    scenario_dir = path.parent.resolve()
    extra_roots = [resolve_path(value, Path.cwd()) for value in args.skill_root]

    materialize_workspace(scenario, paths)
    materialize_skills(scenario, paths, scenario_dir, extra_roots)
    write_config(scenario, paths)
    use_fake_responses = fake_responses_enabled(scenario)
    fake_responses = cast(dict[str, Any], scenario["responses_api"]) if use_fake_responses else None
    inherit_requested = auth_inheritance_requested(scenario, args)
    code_auth_inherited = False
    inherited_env: dict[str, str] = {}
    if inherit_requested:
        inherited_env = inherit_gh_auth(paths)
        if not use_fake_responses:
            inherit_code_auth(paths)
            code_auth_inherited = True
    gh_paths = write_fake_gh(scenario, paths)

    env = os.environ.copy()
    env.update({
        "CODE_HOME": str(paths.code_home),
        "CODEX_HOME": str(paths.code_home),
        "CODEX_SQLITE_HOME": str(paths.code_home),
        "HOME": str(paths.shell_home),
        "PATH": f"{paths.bin_dir}{os.pathsep}{env.get('PATH', '')}",
        "XDG_CACHE_HOME": str(paths.shell_home / ".cache"),
        "XDG_CONFIG_HOME": str(paths.shell_home / ".config"),
        "ZDOTDIR": str(paths.shell_home),
    })
    env.pop("GH_CONFIG_DIR", None)
    env.update(inherited_env)
    for key, value in scenario.get("env", {}).items():
        env[str(key)] = str(value)
    if gh_paths:
        env["CODE_EXEC_HARNESS_GH_FIXTURE"] = str(gh_paths["fixture"])
        env["CODE_EXEC_HARNESS_GH_LOG"] = str(gh_paths["log"])
        env["CODE_EXEC_HARNESS_GH_STATE"] = str(gh_paths["state"])

    server_context = FakeResponsesServer(fake_responses, paths.artifacts) if fake_responses is not None else None

    def run_with_env(
        fake_server: FakeResponsesServer | None,
    ) -> tuple[int, list[dict[str, Any]], list[list[str]]]:
        run_env = env.copy()
        if fake_server is not None:
            run_env["OPENAI_BASE_URL"] = fake_server.base_url
            run_env["OPENAI_API_KEY"] = "harness-test-key"
            run_env["OPENAI_WIRE_API"] = "responses"

        turn_prompts = scenario.get("turns")
        if isinstance(turn_prompts, list) and turn_prompts:
            all_events: list[dict[str, Any]] = []
            commands: list[list[str]] = []
            session_id: str | None = None
            last_returncode = 0
            for index, turn in enumerate(turn_prompts, start=1):
                prompt = str(turn.get("prompt", "") if isinstance(turn, dict) else turn)
                command = build_command_for_prompt(scenario, args, paths, prompt, session_id)
                commands.append(command)
                returncode, events = run_exec_capture(command, scenario, paths, run_env, f"turn-{index}")
                all_events.extend(events)
                last_returncode = returncode
                turn_summary = summarize(events, paths, returncode, command)
                put_json(paths.artifacts / f"turn-{index}-summary.json", turn_summary)
                if returncode != 0:
                    break
                if session_id is None:
                    session_id = session_id_from_summary_or_catalog(turn_summary, paths)
            return last_returncode, all_events, commands

        command = build_command(scenario, args, paths)
        returncode, events = run_exec_capture(command, scenario, paths, run_env, "turn-1")
        return returncode, events, [command]

    put_json(paths.artifacts / "manifest.json", {
        "scenario": str(path),
        "code_home": str(paths.code_home),
        "fake_responses": use_fake_responses,
        "code_auth_inherited": code_auth_inherited,
        "gh_auth_inherited": bool(inherited_env),
        "inherit_auth_requested": inherit_requested,
        "inherit_auth_applied": code_auth_inherited or bool(inherited_env),
        "inherit_auth_suppressed": bool(inherit_requested and use_fake_responses),
        "code_auth_suppressed": bool(inherit_requested and use_fake_responses),
        "workspace": str(paths.workspace),
    })
    if args.dry_run:
        print(paths.run_dir)
        return 0

    if server_context is None:
        returncode, events, commands = run_with_env(None)
        responses_requests: list[dict[str, Any]] = []
    else:
        with server_context as fake_server:
            returncode, events, commands = run_with_env(fake_server)
            responses_requests = list(fake_server.requests)

    summary_command: list[str] = commands[-1] if commands else []
    summary = summarize(events, paths, returncode, summary_command)
    summary["commands"] = summary.get("commands", [])
    summary["scenario_commands"] = [" ".join(command) for command in commands]
    summary["responses_requests"] = responses_requests
    failures = assert_expectations(summary, scenario)
    summary["expectation_failures"] = failures
    put_json(paths.artifacts / "summary.json", summary)
    print(json.dumps({"scenario": name, "run_dir": str(paths.run_dir), "returncode": returncode, "failures": failures}, sort_keys=True))
    return 1 if failures else returncode


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run isolated Every Code `code exec --json` scenarios.")
    parser.add_argument("scenario", nargs="+", type=Path, help="Scenario JSON file(s).")
    parser.add_argument("--output-root", default=str(DEFAULT_OUTPUT_ROOT), help="Directory for run artifacts.")
    parser.add_argument("--code-bin", default="", help="Path to the code binary. Defaults to PATH lookup.")
    parser.add_argument("--model", default="", help="Default model override for scenarios without `model`.")
    parser.add_argument("--sandbox", default="danger-full-access", help="Default code exec sandbox mode.")
    parser.add_argument("--max-seconds", type=int, default=90, help="Default code exec --max-seconds value.")
    parser.add_argument("--skill-root", action="append", default=[], help="Additional skill root to expose under CODE_HOME/skills.")
    parser.add_argument("--inherit-auth", action="store_true", help="Copy auth files from the current CODE_HOME into the isolated run home.")
    parser.add_argument("--dry-run", action="store_true", help="Materialize the run directory and command without invoking code exec.")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    exit_code = 0
    try:
        for scenario in args.scenario:
            result = run_scenario(scenario.resolve(), args)
            exit_code = exit_code or result
    except HarnessError as exc:
        print(f"harness error: {exc}", file=sys.stderr)
        return 2
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
