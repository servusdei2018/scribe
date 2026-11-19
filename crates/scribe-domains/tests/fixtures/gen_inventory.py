"""Generate an objects.inv v2 byte-for-byte using the upstream algorithm.

This mirrors ``sphinx.util.inventory.InventoryFile.dump`` exactly: the same
four-line header (with ``re.sub(r'\\s+', ' ', ...)`` escaping), the same
``zlib.compressobj(9)`` streaming compressor, the same per-domain / per-object
sort order, the same ``$`` anchor shortening and ``-`` display-name collapse.

It runs under the oracle venv so its ``zlib`` is the very same zlib-ng build
the pinned Sphinx uses.  Usage: ``python gen_inventory.py <output-path>``.
"""

from __future__ import annotations

import re
import sys
import zlib

# (fullname, dispname, objtype, docname, anchor, priority)
OBJECTS: dict[str, list[tuple[str, str, str, str, str, int]]] = {
    "py": [
        ("module.func", "module.func", "function", "api", "module.func", 1),
        ("Class", "Custom Display", "class", "api", "Class", 1),
    ],
    "c": [
        ("CFunc", "CFunc", "function", "c-api", "c.CFunc", 1),
    ],
    "std": [
        ("label-one", "label-one", "label", "index", "label-one", -1),
        ("a term", "a term", "term", "glossary", "term-a-term", -1),
        ("index", "Hello", "doc", "index", "", -1),
    ],
}

PROJECT = "My  Project"
VERSION = "v1.0\nbeta"


def escape(value: str) -> str:
    return re.sub(r"\s+", " ", value)


def dump(path: str) -> None:
    header = (
        "# Sphinx inventory version 2\n"
        f"# Project: {escape(PROJECT)}\n"
        f"# Version: {escape(VERSION)}\n"
        "# The remainder of this file is compressed using zlib.\n"
    )
    with open(path, "wb") as stream:
        stream.write(header.encode())
        compressor = zlib.compressobj(9)
        for domain in sorted(OBJECTS):
            for fullname, dispname, objtype, docname, anchor, priority in sorted(OBJECTS[domain]):
                if anchor.endswith(fullname):
                    anchor = anchor.removesuffix(fullname) + "$"
                uri = f"{docname}.html"
                if anchor:
                    uri += f"#{anchor}"
                if dispname == fullname:
                    dispname = "-"
                entry = f"{fullname} {domain}:{objtype} {priority} {uri} {dispname}\n"
                stream.write(compressor.compress(entry.encode()))
        stream.write(compressor.flush())


if __name__ == "__main__":
    dump(sys.argv[1])
