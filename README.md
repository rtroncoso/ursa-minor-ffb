# Ursa Minor FFB

Unlock the full potential of your Winwing Ursa Minor Sidestick with custom force-feedback and rumble effects powered by Microsoft Flight Simulator’s SimConnect API.

This project is a lightweight desktop app written in Rust with an egui UI and HID output.
It lets you tune rumble effects for different flight states like ground roll, flaps, gear, stall, and more.

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
cargo build --release
```

The resulting binary will be in target/release/ursa-minor-ffb.exe.

For debugging with console logs:

```bash
cargo run
```

## Disclaimer

This project is provided for educational purposes only.
It does not intend, in any way, to infringe upon or harm Winwing’s intellectual property,
nor does it attempt to interfere with the intended use of their peripherals.

The goal of this work is simply to explore and unlock the full potential of hardware already owned by the author and community.

## License

This project is licensed under the MIT License. See [LICENSE](./LICENSE) for details.
