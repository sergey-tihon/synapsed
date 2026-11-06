#!/bin/sh
# Materialize only. Starting or enabling the user service is a separate explicit command.
set -eu
umask 077
[ "$(uname -s)" = Darwin ] || { echo 'This installer requires macOS.' >&2; exit 1; }
[ "$(id -u)" -ne 0 ] || { echo 'Install as the desktop user, not root.' >&2; exit 1; }
[ "$#" -le 4 ] || { echo 'Usage: install.sh [EXECUTABLE [CONFIG [STATE_DIR [OUTPUT]]]]' >&2; exit 2; }
case ${XDG_CONFIG_HOME:-} in
  /*) config_home=$XDG_CONFIG_HOME ;;
  *) config_home=$HOME/.config ;;
esac
case ${XDG_DATA_HOME:-} in
  /*) data_home=$XDG_DATA_HOME ;;
  *) data_home=$HOME/.local/share ;;
esac
case ${XDG_STATE_HOME:-} in
  /*) state_home=$XDG_STATE_HOME ;;
  *) state_home=$HOME/.local/state ;;
esac
executable=${1:-"$HOME/.local/bin/synapsed"}
config=${2:-"$config_home/synapsed/config.toml"}
state=${3:-"$state_home/synapsed"}
output=${4:-"$HOME/Library/LaunchAgents/com.sergey-tihon.synapsed.plist"}
for path in "$executable" "$config" "$state" "$output"; do
  case "$path" in /*) ;; *) echo 'All materialized paths must be absolute.' >&2; exit 2;; esac
done
[ -f "$executable" ] && [ -x "$executable" ] || { echo 'Install the real synapsed executable first.' >&2; exit 1; }
[ -f "$config" ] || { echo 'Create the selected daemon configuration first.' >&2; exit 1; }
[ ! -e "$output" ] || { echo 'LaunchAgent already exists; preserve it and choose a different output path.' >&2; exit 1; }
mkdir -p "$state" "$(dirname "$output")"
chmod 700 "$state"
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cp "$source_dir/com.sergey-tihon.synapsed.plist" "$output"
plutil -replace ProgramArguments -array "$output"
plutil -insert ProgramArguments.0 -string "$executable" "$output"
plutil -insert ProgramArguments.1 -string --config "$output"
plutil -insert ProgramArguments.2 -string "$config" "$output"
plutil -replace StandardOutPath -string "$state/daemon.stdout.log" "$output"
plutil -replace StandardErrorPath -string "$state/daemon.stderr.log" "$output"
plutil -replace EnvironmentVariables.XDG_CONFIG_HOME -string "$config_home" "$output"
plutil -replace EnvironmentVariables.XDG_DATA_HOME -string "$data_home" "$output"
plutil -replace EnvironmentVariables.XDG_STATE_HOME -string "$state_home" "$output"
plutil -lint "$output"
printf 'Materialized %s; service not started.\n' "$output"
