# lpudp

`lpudp` is a small Rust service for a Raspberry Shake UDP stream.

It separates the live data path from the browser viewer:

- Rust receives and checks Raspberry Shake UDP packets.
- Rust keeps a bounded ring buffer for each channel.
- Rust runs a STA/LTA alert detector.
- Rust sends compact JSON snapshots and alert events over WebSocket.
- The browser draws waveform and spectrum views.
- The browser provides Dashboard and Settings pages.

The service includes a demo source. Use it to test the viewer before you connect a Shake.

## Run the demo

Install Rust 1.83 or newer. Then run:

```text
cargo run -- --demo
```

Open [http://127.0.0.1:8080](http://127.0.0.1:8080) in a browser. The demo generates four channels and a repeating test pulse.

## Install dependencies on Mint

Run the dependency script from the project directory:

```text
scripts/install-dependencies.sh
```

The script installs the required APT packages, installs Rust with rustup when the current Rust version is too old, fetches locked Rust dependencies, and runs `cargo check`.

Use the check-only mode before an installation:

```text
scripts/install-dependencies.sh --check-only
```

The script targets Linux Mint and other Debian-based systems. It does not change Raspberry Shake settings.

## Run with a Raspberry Shake

The default UDP listener is `0.0.0.0:8888`. The default HTTP listener is `127.0.0.1:8080`.

```text
LPUDP_UDP_ADDR=0.0.0.0:8888 cargo run --release
```

Set `LPUDP_HTTP_ADDR=0.0.0.0:8080` when other computers must open the viewer. Put the HTTP service behind the department firewall when it is reachable from another network.

Set `LPUDP_SETTINGS=/path/to/lpudp.toml` to use a different settings file. The service creates or updates this file when a user saves shared settings.

## Settings

The Settings page stores display values in the current browser. These values control the visible window, browser refresh rate, channel count, and spectrum view.

Alert values are shared by the station. They include:

- alert enable state and channel
- STA and LTA durations
- trigger and reset ratios
- minimum alert duration
- sound enable state and sound file path
- graph image capture on an alert

The `sound_file` path is read by the Rust service on the Linux host. The service starts `paplay` when an alert starts.

Set `LPUDP_ADMIN_TOKEN` to require the same token in the Settings page before a user can save shared values. A GET request does not require this token.

Example shared alert update:

```text
curl -X PUT http://127.0.0.1:8080/api/v1/settings \
  -H 'Content-Type: application/json' \
  -H 'X-Admin-Token: change-this-token' \
  -d '{"alert":{"enabled":true,"channel":"EHZ","sta_seconds":6,"lta_seconds":30,"threshold":3.95,"reset":0.9,"minimum_duration_seconds":0,"sound_enabled":true,"sound_file":"alert.wav","screenshot_enabled":true}}'
```

The service rejects an LTA value that is not greater than STA. It also rejects invalid trigger, reset, sample-rate, and display values.

## API

- `GET /api/v1/status` returns counters and channel health.
- `GET /api/v1/settings` returns the full settings document.
- `PUT /api/v1/settings` updates the `alert` and `archive` sections.
- `GET /api/v1/live` upgrades to a WebSocket stream.

The live stream sends a bounded sample array for each channel. It also sends `Alarm` and `Reset` events. The client calculates the displayed spectrum from the primary channel in the browser.

## Current limits

This first build does not write MiniSEED. The `archive` settings are present so the settings format has a stable shape, but the archive writer is not connected yet. UDP also cannot prove that every packet arrived. The long-term archive path must use SeedLink or another reliable source when complete data is required.

The alert detector is an initial Rust implementation. It uses moving STA and LTA amplitude means. It is not yet a byte-for-byte replacement for every RSUDP or ObsPy alert detail. Validate its results against recorded Shake data before using it for research decisions.

## Next build steps

1. Add a SeedLink or libmseed archive worker.
2. Add binary WebSocket frames after the JSON API is stable.
3. Add browser event history and an explicit “save image” control.
4. Compare alert timing and thresholds with the current RSUDP installation.
5. Add a Linux service unit and a small installation script.
