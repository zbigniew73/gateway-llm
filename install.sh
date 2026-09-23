#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_PATH="$SCRIPT_DIR/target/release/gateway-llm"
SERVICE_NAME="gateway-llm.service"
SYSTEMD_USER_DIR="$HOME/.config/systemd/user"
SERVICE_PATH="$SYSTEMD_USER_DIR/$SERVICE_NAME"

echo "==> gateway-llm installer (repo: $SCRIPT_DIR)"

if ! command -v cargo >/dev/null 2>&1; then
  echo "==> cargo not found -- installing rustup (stable, minimal profile)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
  source "$HOME/.cargo/env"
else
  echo "==> cargo found: $(cargo --version)"
fi

echo "==> cargo build --release (moze potrwac kilka minut przy pierwszym buildzie)"
( cd "$SCRIPT_DIR" && cargo build --release )
[[ -x "$BIN_PATH" ]] || { echo "ERROR: brak $BIN_PATH po buildzie" >&2; exit 1; }

if [[ ! -f "$SCRIPT_DIR/.env" ]]; then
  cp "$SCRIPT_DIR/.env.example" "$SCRIPT_DIR/.env"
  echo "!!! Uzupelnij $SCRIPT_DIR/.env (GATEWAY_API_KEY + klucze providerow) przed startem uslugi."
fi
chmod 600 "$SCRIPT_DIR/.env"

mkdir -p "$SYSTEMD_USER_DIR"
cat > "$SERVICE_PATH" <<EOF
[Unit]
Description=gateway-llm - local LLM gateway (OpenAI + Anthropic compatible)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=$SCRIPT_DIR
EnvironmentFile=$SCRIPT_DIR/.env
ExecStart=$BIN_PATH
Restart=on-failure
RestartSec=2
StandardOutput=journal
StandardError=journal
NoNewPrivileges=yes

[Install]
WantedBy=default.target
EOF
echo "==> zapisano $SERVICE_PATH"

systemctl --user daemon-reload
systemctl --user enable --now "$SERVICE_NAME"

if ! loginctl show-user "$(whoami)" -p Linger 2>/dev/null | grep -q "yes"; then
  loginctl enable-linger "$(whoami)" || echo "WARN: uruchom recznie: loginctl enable-linger $(whoami)"
fi

echo "==> gotowe."
echo "    status: systemctl --user status $SERVICE_NAME"
echo "    logi:   journalctl --user -u $SERVICE_NAME -f"
echo "    diagnostyka: cd $SCRIPT_DIR && ./target/release/gateway-llm doctor --providers"
