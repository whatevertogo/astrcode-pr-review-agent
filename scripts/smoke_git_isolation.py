#!/usr/bin/env python3
"""Exercise the real host hook in an isolated Astrcode instance, without GitHub writes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--worktree", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--port", type=int, default=3987)
    args = parser.parse_args()
    home = Path.home()
    root = args.root.resolve()
    root.mkdir(mode=0o700, parents=True, exist_ok=False)
    astrcode = root / ".astrcode"
    extension = astrcode / "extensions/astrcode-pr-review-agent"
    extension.mkdir(parents=True)
    shutil.copy2(home / ".astrcode/config.toml", astrcode / "config.toml")
    shutil.copy2(args.binary, extension / "astrcode-pr-review-agent")
    (extension / "extension.json").write_text(json.dumps({
        "extension_id": "astrcode-pr-review-agent", "protocol": {"s5r": "3.0"},
        "command": ["./astrcode-pr-review-agent", "s5r"]}))
    data = astrcode / "pr-review-agent"
    data.mkdir()
    config = json.loads((home / ".astrcode/pr-review-agent/config.json").read_text())
    config.update(worktree_dir=str(args.worktree.resolve()), webhook_enabled=False,
                  memory_dir=str(data / "memory"))
    (data / "config.json").write_text(json.dumps(config))
    original = (home / ".gitconfig").read_bytes()
    sentinel = root / "gitconfig.sentinel"
    sentinel.write_bytes(original)
    env = dict(os.environ, ASTRCODE_HOME_DIR=str(root),
               ASTRCODE_PR_REVIEW_AGENT_DATA_DIR=str(data),
               ASTRCODE_PR_REVIEW_AGENT_DISABLE_POLL="1", GIT_CONFIG_GLOBAL=str(sentinel))
    env.pop("ASTRCODE_TEST_HOME", None)
    base = f"http://127.0.0.1:{args.port}"

    def request(path, body=None):
        payload = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(base + path, data=payload,
                                     headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=15) as response:
            raw = response.read()
            return json.loads(raw) if raw else None

    with (root / "server.log").open("w") as log:
        process = subprocess.Popen([str(args.server.resolve()), "server", "--addr", f"127.0.0.1:{args.port}"],
                                   cwd=root, env=env, stdout=log, stderr=log, start_new_session=True)
        try:
            for _ in range(60):
                if process.poll() is not None:
                    raise RuntimeError("staging server exited; inspect server.log")
                try:
                    extensions = request("/api/extensions")
                    break
                except (OSError, ValueError):
                    time.sleep(1)
            else:
                raise RuntimeError("staging server startup timeout")
            (root / "extensions.json").write_text(json.dumps(extensions, indent=2))
            session = request("/api/sessions", {"workingDir": str(args.worktree.resolve())})
            sid = session.get("sessionId", session.get("session_id"))
            assert sid, session
            command = ('case "$GIT_CONFIG_GLOBAL" in */astrcode-pr-git.*) ;; *) exit 91;; esac\n'
                       'bash tests/board-scripts/board_build_fallback_contract.sh && printf "ISOLATION_CONTRACT_OK\\n"')
            request(f"/api/sessions/{sid}/prompt", {"text":
                "Run exactly this shell command once, then report the exit status. Do not edit files, "
                "change environment variables, or publish anything. This is an authorized test.\n```sh\n"
                + command + "\n```", "attachments": []})
            completed = False
            for _ in range(240):
                events = []
                for path in astrcode.glob(f"projects/*/sessions/{sid}/session-*.jsonl"):
                    events.extend(json.loads(line).get("payload", {}) for line in path.read_text().splitlines())
                tool_completed = False
                for event in events:
                    result = event.get("result", {})
                    if (event.get("type") == "tool_call_completed"
                            and not result.get("is_error", True)
                            and result.get("metadata", {}).get("exitCode") == 0
                            and "ISOLATION_CONTRACT_OK\n" in result.get("content", "")):
                        tool_completed = True
                    if event.get("type") == "assistant_message_completed" and tool_completed:
                        completed = True
                if completed:
                    break
                time.sleep(2)
            assert completed, "no completed real-tool test; inspect staging session"
            assert sentinel.read_bytes() == original, "staging global config changed"
            assert (home / ".gitconfig").read_bytes() == original, "host global config changed"
            result = {"session_id": sid, "passed": True, "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                      "host_config_unchanged": True, "staging_config_unchanged": True}
            (root / "result.json").write_text(json.dumps(result, indent=2))
            print(json.dumps(result), flush=True)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()


if __name__ == "__main__":
    main()
