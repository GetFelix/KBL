# Sourced by up.sh and down.sh: sets `compose` to the command that runs
# dev/compose.yml. CONTAINER_ENGINE=docker or podman picks the engine; else
# Docker when its daemon answers, else Podman through `podman compose`, which
# needs a compose provider that honors depends_on conditions: Docker Compose
# v2, or podman-compose 1.4 or later.
engine="${CONTAINER_ENGINE:-}"
if [[ -z "$engine" ]]; then
  if docker info >/dev/null 2>&1; then
    engine=docker
  elif command -v podman >/dev/null 2>&1; then
    engine=podman
  else
    echo "no container engine: start Docker, or install Podman (and run podman machine start on macOS)" >&2
    exit 1
  fi
fi
case "$engine" in
  docker) compose=(docker compose) ;;
  podman)
    if ! podman compose version >/dev/null 2>&1; then
      echo "podman has no compose provider: install Docker Compose v2 or podman-compose 1.4+" >&2
      exit 1
    fi
    compose=(podman compose)
    ;;
  *)
    echo "CONTAINER_ENGINE must be docker or podman, not $engine" >&2
    exit 1
    ;;
esac
compose+=(-f "$(dirname "${BASH_SOURCE[0]}")/compose.yml")
