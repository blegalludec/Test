#!/usr/bin/env python3
"""Génère cargo-sources.json pour une construction Flatpak hors ligne.

flatpak-builder n'a pas accès au réseau pendant la compilation : chaque crate
de Cargo.lock est déclarée comme source téléchargée à l'avance (somme SHA-256
issue de Cargo.lock), puis Cargo est configuré pour utiliser ce dossier
« vendor » à la place de crates.io.

Usage : build-aux/flatpak/generate-cargo-sources.py [Cargo.lock] [sortie.json]
À relancer après chaque modification de Cargo.lock (la CI le vérifie).
"""

import json
import sys
import tomllib
from pathlib import Path

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
VENDOR = "cargo/vendor"

CARGO_CONFIG = f"""[source.vendored-sources]
directory = "{VENDOR}"

[source.crates-io]
replace-with = "vendored-sources"
"""


def generate(lock_path: Path) -> list:
    lock = tomllib.loads(lock_path.read_text())
    sources = []
    for pkg in lock.get("package", []):
        source = pkg.get("source")
        if source is None:
            continue  # crate locale (ce projet)
        if source != CRATES_IO:
            sys.exit(f"source non prise en charge pour {pkg['name']} : {source}")
        name, version, checksum = pkg["name"], pkg["version"], pkg["checksum"]
        dest = f"{VENDOR}/{name}-{version}"
        sources.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                "sha256": checksum,
                "dest": dest,
            }
        )
        sources.append(
            {
                "type": "inline",
                "contents": json.dumps({"package": checksum, "files": {}}),
                "dest": dest,
                "dest-filename": ".cargo-checksum.json",
            }
        )
    sources.append(
        {
            "type": "inline",
            "contents": CARGO_CONFIG,
            "dest": "cargo",
            "dest-filename": "config.toml",
        }
    )
    return sources


def main() -> None:
    root = Path(__file__).resolve().parents[2]
    lock = Path(sys.argv[1]) if len(sys.argv) > 1 else root / "Cargo.lock"
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else Path(__file__).with_name("cargo-sources.json")
    out.write_text(json.dumps(generate(lock), indent=4) + "\n")
    print(f"{out} : {len(generate(lock)) // 2} crates")


if __name__ == "__main__":
    main()
