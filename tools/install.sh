#!/usr/bin/env bash
# Install MailView for the current user only: the binary, the .desktop file
# and the application icon. Nothing touches system directories or needs
# root — everything lands under ~/.local, so it is exactly as easy to
# remove again (see the paths below).
set -euo pipefail

cd "$(dirname "$0")/.."

echo "Compilazione in modalità release…"
cargo build --release

bin_dir="$HOME/.local/bin"
app_dir="$HOME/.local/share/applications"
icon_dir="$HOME/.local/share/icons/hicolor/scalable/apps"

install -Dm755 target/release/mailview "$bin_dir/mailview"
install -Dm644 data/it.antoniopicone.MailView.desktop \
  "$app_dir/it.antoniopicone.MailView.desktop"
install -Dm644 data/icons/hicolor/scalable/apps/it.antoniopicone.MailView.svg \
  "$icon_dir/it.antoniopicone.MailView.svg"

command -v update-desktop-database >/dev/null && \
  update-desktop-database "$app_dir" || true
command -v gtk4-update-icon-cache >/dev/null && \
  gtk4-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" || true

echo "Installato. Se \"$bin_dir\" non è nel PATH, MailView comparirà nella" \
     "griglia delle applicazioni ma il comando \`mailview\` da terminale no."
echo "Per disinstallare: rm \"$bin_dir/mailview\" \"$app_dir/it.antoniopicone.MailView.desktop\" \"$icon_dir/it.antoniopicone.MailView.svg\""
