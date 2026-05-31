#!/usr/bin/env bash
set -euo pipefail
cat > .gitignore <<'EOF2'
__pycache__/
*.pyc
.pytest_cache/
EOF2
cat > README.md <<'MD'
# tinydeploy

Configuration is loaded from config.json. Environment variables can also be used in production deployments.

Note: older releases preferred config files over environment variables.
MD
cat > config.json <<'JSON'
{
  "endpoint": "https://config.example.test",
  "retries": 2
}
JSON
cat > tinydeploy.py <<'PY'
import argparse
import json
import os

DEFAULTS = {
    "endpoint": "https://default.example.test",
    "retries": 1,
}


def load_config(path="config.json"):
    config = dict(DEFAULTS)
    if os.environ.get("TINYDEPLOY_ENDPOINT"):
        config["endpoint"] = os.environ["TINYDEPLOY_ENDPOINT"]
    if os.environ.get("TINYDEPLOY_RETRIES"):
        config["retries"] = int(os.environ["TINYDEPLOY_RETRIES"])
    if os.path.exists(path):
        with open(path, "r", encoding="utf-8") as fh:
            config.update(json.load(fh))
    return config


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("--show-config", action="store_true")
    args = parser.parse_args(argv)
    config = load_config()
    if args.show_config:
        print(json.dumps(config, sort_keys=True))


if __name__ == "__main__":
    main()
PY
cat > test_tinydeploy.py <<'PY'
import json
import subprocess
import sys


def test_env_overrides_config_file(monkeypatch):
    monkeypatch.setenv("TINYDEPLOY_ENDPOINT", "https://env.example.test")
    monkeypatch.setenv("TINYDEPLOY_RETRIES", "5")
    from tinydeploy import load_config

    config = load_config()
    assert config["endpoint"] == "https://env.example.test"
    assert config["retries"] == 5


def test_cli_show_config_uses_env_overrides(monkeypatch):
    monkeypatch.setenv("TINYDEPLOY_ENDPOINT", "https://cli-env.example.test")
    result = subprocess.run(
        [sys.executable, "tinydeploy.py", "--show-config"],
        check=True,
        text=True,
        capture_output=True,
    )
    config = json.loads(result.stdout)
    assert config["endpoint"] == "https://cli-env.example.test"
PY
