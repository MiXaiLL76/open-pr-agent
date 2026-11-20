# Build from source

For regular use, pull the [published image](https://github.com/MiXaiLL76/open-pr-agent/pkgs/container/open-pr-agent):

```sh
docker pull ghcr.io/mixaill76/open-pr-agent:latest
```

## Build a custom container image

Run these commands from the repository root:

```sh
make docker-build IMAGE=open-pr-agent TAG=local
```

To build and publish to your own registry:

```sh
make docker-build IMAGE=registry.example.com/open-pr-agent TAG=latest
make docker-push IMAGE=registry.example.com/open-pr-agent TAG=latest
```

The Dockerfile pins the OpenCode release and SHA-256 checksum. To build through
a corporate mirror, set the matching URL and checksum:

```sh
docker build -f docker/Dockerfile.rust -t open-pr-agent:local \
  --build-arg OPENCODE_TARBALL_URL=https://mirror.example.com/opencode-linux-x64.tar.gz \
  --build-arg OPENCODE_SHA256="$OPENCODE_SHA256" .
```

Use the custom tag in the relevant CI template or as `PR_AGENT_IMAGE` in
the GitLab CI and Compose examples.

## Build and test the Rust binaries

```sh
make format
make lint
make test
make build
```

The binaries are in `rust/target/release/`. Use `make help` for the full list
of development targets.
