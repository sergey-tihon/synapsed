#!/bin/sh
set -eu

usage() {
  printf '%s\n' 'Usage: install.sh [--generic-only] [--idasen] [--no-omarchy]' >&2
}

generic_only=0
idasen=0
no_omarchy=0
for argument in "$@"; do
  case "$argument" in
    --generic-only) generic_only=1 ;;
    --idasen) idasen=1 ;;
    --no-omarchy) no_omarchy=1 ;;
    *) usage; exit 2 ;;
  esac
done

[ "$(uname -s)" = Linux ] || { printf '%s\n' 'This installer requires Linux.' >&2; exit 1; }
[ "$(id -u)" -ne 0 ] || { printf '%s\n' 'Install as the desktop user, not root.' >&2; exit 1; }

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
if [ ! -x "$root/bin/synapsed" ] || [ ! -x "$root/bin/synapsectl" ]; then
  printf '%s\n' 'Run this from an extracted Synapsed Linux archive (bin/ is missing); from a checkout run npm run pack and extract dist/Synapsed-*.tar.gz.' >&2
  exit 1
fi

case ${XDG_DATA_HOME:-} in
  /*) data_home=$XDG_DATA_HOME ;;
  *) data_home=$HOME/.local/share ;;
esac
case ${XDG_CONFIG_HOME:-} in
  /*) config_home=$XDG_CONFIG_HOME ;;
  *) config_home=$HOME/.config ;;
esac
case ${XDG_STATE_HOME:-} in
  /*) state_home=$XDG_STATE_HOME ;;
  *) state_home=$HOME/.local/state ;;
esac
newline='
'
case "$HOME$config_home$data_home$state_home" in
  *"$newline"*)
    printf '%s\n' 'Home and XDG paths must not contain newline characters.' >&2
    exit 1
    ;;
esac
bin_dir=$HOME/.local/bin
drivers=$data_home/synapsed/drivers
config=$config_home/synapsed/config.toml
# User managers always search this standard location, even when XDG_CONFIG_HOME
# is customized in the installing shell.
unit=$HOME/.config/systemd/user/synapsed.service
dropin=$HOME/.config/systemd/user/synapsed.service.d/10-xdg-paths.conf
plugin=$HOME/.config/omarchy/plugins/sergey-tihon.synapsed

mkdir -p "$bin_dir" "$drivers" "$(dirname "$config")" "$(dirname "$unit")" "$(dirname "$dropin")"
install -m 0755 "$root/bin/synapsed" "$bin_dir/synapsed"
install -m 0755 "$root/bin/synapsectl" "$bin_dir/synapsectl"

install_driver() {
  driver=$1
  staging=$drivers/.$driver.new
  rm -rf "$staging"
  cp -R "$root/drivers/$driver" "$staging"
  rm -rf "$drivers/$driver"
  mv "$staging" "$drivers/$driver"
}

install_driver generic-ddc
if [ "$generic_only" -eq 0 ]; then
  install_driver benq-rd280u
fi
if [ "$idasen" -eq 1 ]; then
  install_driver ikea-idasen
fi

if [ -e "$config" ]; then
  printf 'Kept existing %s\n' "$config"
else
  (umask 077; cp "$root/deploy/config.example.toml" "$config")
fi
install -m 0644 "$root/deploy/systemd/synapsed.service" "$unit"

# The service manager may not inherit the installer's environment. Persist the
# resolved XDG locations so the daemon finds the same config and drivers later.
systemd_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/%/%%/g'
}
config_home_escaped=$(systemd_escape "$config_home")
data_home_escaped=$(systemd_escape "$data_home")
state_home_escaped=$(systemd_escape "$state_home")
cat > "$dropin" <<EOF
[Service]
Environment="XDG_CONFIG_HOME=$config_home_escaped"
Environment="XDG_DATA_HOME=$data_home_escaped"
Environment="XDG_STATE_HOME=$state_home_escaped"
EOF
chmod 0644 "$dropin"

widget_installed=0
if [ "$no_omarchy" -eq 0 ] && [ -d "$HOME/.config/omarchy" ] && [ -f "$root/omarchy/manifest.json" ]; then
  plugin_parent=$(dirname "$plugin")
  mkdir -p "$plugin_parent"
  staging=$plugin_parent/.sergey-tihon.synapsed.new
  rm -rf "$staging"
  cp -R "$root/omarchy" "$staging"
  rm -rf "$plugin"
  mv "$staging" "$plugin"
  widget_installed=1
elif [ "$no_omarchy" -eq 1 ]; then
  printf '%s\n' 'Omarchy widget installation skipped (--no-omarchy).'
else
  printf '%s\n' 'Omarchy not detected; widget not installed.'
fi

if command -v systemctl >/dev/null 2>&1; then
  if ! systemctl --user daemon-reload; then
    printf '%s\n' 'Warning: systemd user daemon-reload failed; reload it manually.' >&2
  fi
fi

case ":${PATH:-}:" in
  *":$bin_dir:"*) ;;
  *) printf 'Warning: %s is not on PATH.\n' "$bin_dir" >&2 ;;
esac

accessible_i2c=0
for device in /dev/i2c-*; do
  [ -e "$device" ] || continue
  if [ -r "$device" ] && [ -w "$device" ]; then
    accessible_i2c=1
    break
  fi
done
if [ "$accessible_i2c" -eq 0 ]; then
  printf '%s\n' 'Warning: no /dev/i2c-* device is readable and writable by this user. Load i2c-dev and grant display-bus access through your distribution policy (for example the ddcutil package udev rule); this installer never changes permissions.' >&2
fi

printf 'Installed for %s; service not started.\n' "$(id -un)"
printf '%s\n' 'Next steps: systemctl --user enable --now synapsed.service (or systemctl --user restart synapsed.service after an upgrade).'
if [ "$widget_installed" -eq 1 ]; then
  printf '%s\n' 'Enable the Omarchy widget with: omarchy plugin enable sergey-tihon.synapsed'
fi
