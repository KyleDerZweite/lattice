# Lattice

Lattice is an early Rust-native Markdown workspace. The repository is currently
through Phase 0/1: planning docs, crate boundaries, a native `egui/eframe` app
shell, settings persistence, vault opening, and flat file listing.

## Commands

```sh
cargo run -p lattice-app -- [PATH]
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Product direction lives in [PRODUCT.md](PRODUCT.md). Implementation phases live
in [docs/ROADMAP.md](docs/ROADMAP.md). Current and planned crate ownership is
summarized in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
