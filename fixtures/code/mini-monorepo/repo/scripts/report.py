import json
import sys
from pathlib import Path


def load_stats(path: Path) -> dict:
    with path.open() as source:
        return json.load(source)


def render(stats: dict) -> str:
    saved = stats["net_tokens_saved"]
    observed = stats["observed_tokens"]
    pct = 100.0 * saved / observed if observed else 0.0
    return f"saved {saved} of {observed} tokens ({pct:.1f}%)"


def main() -> int:
    stats = load_stats(Path(sys.argv[1]))
    print(render(stats))
    return 0


if __name__ == "__main__":
    sys.exit(main())
