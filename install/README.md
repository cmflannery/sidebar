# install/

Templates for keeping `sidebar serve` running across reboots.

## macOS (launchd)

```bash
# 1. install the binary on PATH
cargo install --path .

# 2. edit the plist — replace CHANGE_ME with your home dir
sed -i '' "s|CHANGE_ME|$USER|g" install/com.sidebar.daemon.plist

# 3. drop it into LaunchAgents and bootstrap
cp install/com.sidebar.daemon.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.sidebar.daemon.plist

# 4. verify
sidebar status
```

Logs land in `~/Library/Logs/sidebar.{out,err}.log`. To unload:

```bash
launchctl bootout gui/$(id -u)/com.sidebar.daemon
```

The plist still expects `/usr/local/bin/sidebar` by default — edit the
`<string>` under `ProgramArguments` if you installed elsewhere (e.g.
`~/.cargo/bin/sidebar`).

## Linux (systemd, user unit)

```bash
# 1. install the binary on PATH
cargo install --path .

# 2. drop the service into your user units
mkdir -p ~/.config/systemd/user
cp install/sidebar.service ~/.config/systemd/user/

# 3. enable + start
systemctl --user daemon-reload
systemctl --user enable --now sidebar

# 4. verify
sidebar status
```

Logs:

```bash
journalctl --user -u sidebar -f
```

Uninstall:

```bash
systemctl --user disable --now sidebar
```

The unit assumes the binary lives at `~/.cargo/bin/sidebar`. Edit the
`ExecStart` line if not.

## Verifying it survives a reboot

```bash
sudo reboot
# log back in
sidebar status              # should show "daemon: running"
sidebar tail                # should produce output immediately when agents connect
```
