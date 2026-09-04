set shell := ["bash", "-euo", "pipefail", "-c"]

# Show available development commands.
default:
    @just --list

# Build the Linux development image.
container-build:
    uv run dev/container build

# Start the persistent Linux development container.
container-start:
    uv run dev/container start

# Replace the persistent container with one using the current image.
container-recreate:
    uv run dev/container recreate

# Stop the development container.
container-stop:
    uv run dev/container stop

# Open an interactive shell in the development container.
container-shell:
    uv run dev/container shell

# Build the image and replace the development container.
bootstrap: container-build container-recreate

# Build Buildprof and run all conformance tests, or pass pytest arguments.
test *args:
    uv run dev/test {{args}}

# Confirm the build and test environment and collect the test suite.
check:
    uv run dev/test --collect-only

# Run the local checks required before packaging a release.
release-check:
    cargo fmt --all -- --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo test --all-targets --locked
    cargo package --allow-dirty --locked

# Checkout pinned Perfetto, apply patches, and link permanent overlays.
perfetto-setup:
    uv run tools/perfetto setup

# Capture committed Perfetto changes as the ordered patch series.
perfetto-capture:
    uv run tools/perfetto capture

# Move the Perfetto pin and reapply patches and overlays.
perfetto-uprev revision="latest":
    uv run tools/perfetto uprev {{revision}}

# Build the patched, self-hostable Perfetto UI.
perfetto-build-ui *args:
    uv run tools/perfetto build-ui {{args}}

# Serve the UI with live reload; open traces in it with `buildprof open --dev-server`.
perfetto-dev-server *args:
    uv run tools/perfetto dev-server {{args}}
