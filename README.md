# Ursa Minor FFB

Unlock the full potential of your Winwing Ursa Minor Sidestick with custom force-feedback and rumble effects powered by Microsoft Flight Simulator’s SimConnect API.

This project is a lightweight desktop app written in Rust with an egui UI and HID output.
It lets you tune rumble effects for different flight states like ground roll, flaps, gear, stall, and more.

[Official Flightsim.to store page](https://flightsim.to/addon/98251/ursa-minor-ffb)

---

## 🚀 Build Instructions

You’ll need the Rust toolchain installed ([rustup](https://rustup.rs/)) and enable the MSVC toolchain for rust:

```bash
rustup toolchain install stable-msvc
```

Clone and build:

```bash
git clone https://github.com/rtroncoso/ursa-minor-ffb.git
cd ursa-minor-ffb
cargo build --release --bin ursa-minor-ffb --features app
```

The resulting binary will be in target/release/ursa-minor-ffb.exe.

For debugging with console logs:

```bash
cargo run --bin ursa-minor-ffb --features app
```

## Testing

Core rumble math, HID frame encoding, and SimConnect parsing are covered by unit and integration tests that run on Linux without hardware.

```bash
# Run all library and integration tests (cross-platform)
cargo test --lib --tests

# Lint and format (same as CI)
cargo fmt --all -- --check
cargo clippy --lib --tests -- -D warnings
```

Reproducible local test run via Docker:

```bash
docker build -f Dockerfile.dev -t ursa-ffb-test .
docker run --rm ursa-ffb-test
```

On Windows, build and test the full app (including GUI/HID workers):

```bash
cargo test --features app
cargo build --release --bin ursa-minor-ffb --features app
```

## Disclaimer

This project is provided for educational purposes only.
It does not intend, in any way, to infringe upon or harm Winwing’s intellectual property,
nor does it attempt to interfere with the intended use of their peripherals.

The goal of this work is simply to explore and unlock the full potential of hardware already owned by the author and community.

## License

This project is licensed under the MIT License. See [LICENSE](./LICENSE) for details.
