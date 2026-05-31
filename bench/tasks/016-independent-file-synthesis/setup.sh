#!/usr/bin/env bash
set -euo pipefail
mkdir -p services
cat > services/api.toml <<'TOML'
name = "api"
owner = "platform"
timeout_ms = 1200
retries = 2
TOML
cat > services/billing.toml <<'TOML'
name = "billing"
owner = "payments"
timeout_ms = 1200
retries = 2
TOML
cat > services/search.toml <<'TOML'
name = "search"
owner = "discovery"
timeout_ms = 1200
retries = 2
TOML
cat > services/worker.toml <<'TOML'
name = "worker"
owner = "platform"
timeout_ms = 1200
retries = 2
TOML
cat > services/notifications.toml <<'TOML'
name = "notifications"
owner = "messaging"
timeout_ms = 1200
retries = 2
TOML
cat > services/reports.toml <<'TOML'
name = "reports"
owner = "analytics"
timeout_ms = 9000
retries = 2
TOML
cat > README.md <<'MD'
# service config audit

All service configs should use the shared timeout policy unless an exception is documented. There are no documented exceptions in this fixture.
MD
