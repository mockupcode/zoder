# zoder

Terminal coding assistant.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/mockupcode/zoder/main/install.sh | bash
```

Pin a version:

```bash
curl -fsSL https://raw.githubusercontent.com/mockupcode/zoder/main/install.sh | bash -s 0.1.0
```

The script downloads the matching binary from GitHub Releases into `~/.zoder/bin` and adds that directory to PATH.

A release is created when a version tag is pushed:

```bash
git tag v0.1.0
git push origin v0.1.0
```

GitHub Actions builds `macos-aarch64`, `macos-x86_64`, `linux-x86_64`, and `linux-aarch64`.

## Config

Copy `config.example.toml` to `~/.zoder/config.toml`. Hosts and models stay on the machine, not in this repo.

## Build from source

```bash
cargo run --release
```
