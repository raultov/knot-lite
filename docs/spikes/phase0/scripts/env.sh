# Sourced by every spike script. Keep it side-effect free.
export SPIKE_ROOT=/tmp/knot-rag-spike
export SPIKE_CRATE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export KNOT_EMBED_MODEL=AllMiniLML6V2
export KNOT_EMBED_DIM=384
export KNOT_FASTEMBED_CACHE_DIR="$HOME/.knot/repos/fastembed_cache"
export CARGO_TARGET_DIR_RA="$SPIKE_ROOT/target-ra"
export RUST_LOG="${RUST_LOG:-info}"
