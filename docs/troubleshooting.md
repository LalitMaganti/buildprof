# Troubleshooting

Buildprof only records the process tree under the command it runs. Work
outside that tree is not recorded. It usually appears as one long command
with nothing underneath it.

<!-- buildprof.lalitm.com/diagnose/daemons links here; see infra/buildprof.lalitm.com/assemble-site -->
## Bazel, Gradle, and other daemon build systems

Buildprof cannot see work sent to an existing daemon or a remote executor.
An existing daemon leaves only the client in the recording. If the recorded
command starts the daemon, recording continues until the daemon exits.
Disable the daemon or stop it before and after the build:

```sh
bazel shutdown && buildprof -- sh -c 'bazel build //... ; bazel shutdown'
buildprof -- ./gradlew --no-daemon build
buck2 kill && buildprof -- sh -c 'buck2 build //... ; buck2 kill'
sccache --stop-server && buildprof -- sh -c 'cargo build; sccache --stop-server'
```

<!-- buildprof.lalitm.com/diagnose/containers links here; see infra/buildprof.lalitm.com/assemble-site -->
## Builds inside Docker containers

`docker run` hands the container to the Docker daemon. The container's
processes are outside the recorded tree. You can record inside the container
or use Podman.

## Record inside the container

Download the static musl build on the host and mount it into the container.
It runs in any Linux image without changing the image. Use `aarch64` instead
of `x86_64` for ARM images, including Docker Desktop on Apple Silicon:

```sh
mkdir -p ~/.cache/buildprof-linux
curl -fsSL https://github.com/LalitMaganti/buildprof/releases/latest/download/buildprof-x86_64-unknown-linux-musl.tar.xz \
  | tar -xJ -C ~/.cache/buildprof-linux --strip-components=1
```

Run the build through the mounted binary. Docker needs `--cap-add SYS_PTRACE`
to allow tracing. The recording goes in the mounted project directory:

```sh
docker run --rm --cap-add SYS_PTRACE \
  -v ~/.cache/buildprof-linux:/opt/buildprof:ro \
  -v "$PWD:/src" -w /src my-build-image \
  /opt/buildprof/buildprof -o build.buildprof --no-open -- cargo build
buildprof open build.buildprof
```

Use `--no-open` because the host browser cannot reach the container's
localhost, where the recording would be served. Open the recording with
`buildprof open` on the host. To make the binary a permanent part of the image,
copy it in the Dockerfile.

<!-- buildprof.lalitm.com/diagnose/podman links here; see infra/buildprof.lalitm.com/assemble-site -->
## Use Podman

Podman has no daemon. Container processes are descendants of `podman run`,
so Buildprof records them without image changes. Podman accepts the same
`run` options as Docker:

```sh
podman info > /dev/null
buildprof -- podman run --rm -v "$PWD:/src" -w /src my-build-image cargo build
```

> [!IMPORTANT]
> Run `podman info` before recording. The first Podman command after a reboot
> or `podman system migrate` sets up the rootless user namespace using setuid
> `newuidmap`. It cannot gain privileges while traced, causing
> `newuidmap: write to uid_map failed: Operation not permitted`.
> Any Podman command run outside the recording sets up the namespace.
> Later commands reuse it.

Commands and file paths from inside the container are recorded as the
container sees them, such as `/src/target/...` rather than the host path.
